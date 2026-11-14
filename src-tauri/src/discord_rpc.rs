//! Discord Rich Presence over the local IPC named pipe.
//!
//! Port of upstream Riff's `discord.js` (Electron + discord-rpc npm package):
//! - Windows path: `\\?\pipe\discord-ipc-{0..9}`
//! - Framing: `<u32 LE opcode><u32 LE payload len><utf8 JSON payload>`
//! - Opcodes: Handshake 0, Frame 1, Close 2, Ping 3, Pong 4
//! - Lifecycle: connect → HANDSHAKE → READY → repeated SET_ACTIVITY
//! - Reconnect: fixed 15 s poll while enabled; silent failure otherwise.
//! - Activity shape mirrors upstream's `_setActivity()` exactly.

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const CLIENT_ID: &str = "1520100510877749358";
const RECONNECT: Duration = Duration::from_secs(15);
const READ_POLL: Duration = Duration::from_millis(200);

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

fn pipe_path(id: u32) -> PathBuf {
    // Windows: IPC pipe path uses the \\?\ prefix.
    PathBuf::from(format!("\\\\?\\pipe\\discord-ipc-{id}"))
}

fn read_packet(file: &mut File) -> std::io::Result<(u32, Vec<u8>)> {
    let mut head = [0u8; 8];
    file.read_exact(&mut head)?;
    let op = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        file.read_exact(&mut payload)?;
    }
    Ok((op, payload))
}

fn write_packet(file: &mut File, op: u32, payload: &str) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(&op.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload.as_bytes());
    file.write_all(&buf)?;
    file.flush()
}

#[derive(Debug)]
pub enum Cmd {
    Set(Value),
    Clear,
    Reconnect,
}

struct Shared {
    enabled: bool,
    pipe: Option<Arc<Mutex<File>>>,
    last_activity: Option<Value>,
}

/// Handle for Discord Rich Presence. Clone-friendly; the background threads live
/// for the life of the process.
#[allow(dead_code)]
pub struct DiscordRpc {
    shared: Arc<Mutex<Shared>>,
    connected: Arc<AtomicBool>,
    tx: Mutex<mpsc::Sender<Cmd>>,
    threads: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl DiscordRpc {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            enabled: false,
            pipe: None,
            last_activity: None,
        }));
        let connected = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let writer = {
            let shared = Arc::clone(&shared);
            let connected = Arc::clone(&connected);
            thread::Builder::new()
                .name("discord-rpc-writer".into())
                .spawn(move || writer_loop(shared, connected, rx))
                .expect("spawn discord-rpc writer")
        };
        let reader = {
            let shared = Arc::clone(&shared);
            let connected = Arc::clone(&connected);
            thread::Builder::new()
                .name("discord-rpc-reader".into())
                .spawn(move || reader_loop(shared, connected))
                .expect("spawn discord-rpc reader")
        };

        Self {
            shared,
            connected,
            tx: Mutex::new(tx),
            threads: Mutex::new(vec![writer, reader]),
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> bool {
        if let Ok(mut guard) = self.shared.lock() {
            guard.enabled = enabled;
            if !enabled {
                guard.pipe = None; // signal both threads to drop the pipe
                guard.last_activity = guard.last_activity.take();
                let _ = self.tx.lock().map(|tx| tx.send(Cmd::Clear));
            } else {
                let _ = self.tx.lock().map(|tx| tx.send(Cmd::Reconnect));
            }
        }
        enabled
    }

    pub fn is_enabled(&self) -> bool {
        self.shared.lock().map(|s| s.enabled).unwrap_or(false)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn set_activity(&self, track_info: Value) {
        if let Ok(mut guard) = self.shared.lock() {
            guard.last_activity = Some(track_info.clone());
        }
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Cmd::Set(track_info));
        }
    }

    pub fn clear_activity(&self) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Cmd::Clear);
        }
    }

    pub fn reconnect(&self) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Cmd::Reconnect);
        }
    }
}

fn try_connect(shared: &Arc<Mutex<Shared>>) {
    for id in 0..10 {
        let path = pipe_path(id);
        let Ok(mut file) = File::options().read(true).write(true).open(&path) else {
            continue;
        };
        let handshake = format!(r#"{{"v":1,"client_id":"{CLIENT_ID}"}}"#);
        if write_packet(&mut file, OP_HANDSHAKE, &handshake).is_err() {
            continue;
        }
        // Wait for READY/PING (reads block until a packet arrives; Discord is prompt).
        match read_packet(&mut file) {
            Ok((OP_FRAME, payload)) => {
                let frame: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                if frame.get("cmd").and_then(Value::as_str) == Some("DISPATCH")
                    && frame.get("evt").and_then(Value::as_str) == Some("READY")
                {
                    let arc = Arc::new(Mutex::new(file));
                    if let Ok(mut guard) = shared.lock() {
                        guard.pipe = Some(arc);
                    }
                    crate::util::log_line("INFO", "discord rpc connected");
                    return;
                }
                crate::util::log_line("WARN", "discord rpc handshake returned unknown frame; dropping pipe");
                return;
            }
            Ok((OP_PING, payload)) => {
                let _ = write_packet(&mut file, OP_PONG, &String::from_utf8_lossy(&payload));
                let arc = Arc::new(Mutex::new(file));
                if let Ok(mut guard) = shared.lock() {
                    guard.pipe = Some(arc);
                }
                crate::util::log_line("INFO", "discord rpc connected (ping-first)");
                return;
            }
            Ok(_) | Err(_) => {
                crate::util::log_line("WARN", "discord rpc handshake failed; trying next pipe id");
                return;
            }
        }
    }
}

fn writer_loop(shared: Arc<Mutex<Shared>>, connected: Arc<AtomicBool>, rx: mpsc::Receiver<Cmd>) {
    let mut last_activity: Option<Value> = None;
    let mut had_pipe = false;
    // Start with a probe scheduled immediately, then every RECONNECT seconds.
    let mut last_reconnect = Instant::now() - RECONNECT;

    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Cmd::Set(activity)) => {
                last_activity = Some(activity.clone());
                write_activity(&shared, &activity, &connected);
            }
            Ok(Cmd::Clear) => {
                write_clear(&shared, &connected);
            }
            Ok(Cmd::Reconnect) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.pipe = None;
                }
                had_pipe = false;
                last_reconnect = Instant::now() - RECONNECT;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let has_pipe = shared.lock().map(|s| s.pipe.is_some()).unwrap_or(false);
                if has_pipe && !had_pipe {
                    if let Some(activity) = &last_activity {
                        write_activity(&shared, activity, &connected);
                    }
                    had_pipe = true;
                } else if !has_pipe {
                    had_pipe = false;
                    let enabled = shared.lock().map(|s| s.enabled).unwrap_or(false);
                    if enabled && last_reconnect.elapsed() >= RECONNECT {
                        last_reconnect = Instant::now();
                        try_connect(&shared);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn reader_loop(shared: Arc<Mutex<Shared>>, connected: Arc<AtomicBool>) {
    loop {
        let pipe = shared.lock().ok().and_then(|s| s.pipe.clone());
        let Some(pipe) = pipe else {
            thread::sleep(READ_POLL);
            continue;
        };
        let packet = {
            let mut file = pipe.lock().unwrap_or_else(|p| p.into_inner());
            read_packet(&mut file)
        };
        match packet {
            Ok((OP_PING, payload)) => {
                let mut file = pipe.lock().unwrap_or_else(|p| p.into_inner());
                let _ = write_packet(&mut file, OP_PONG, &String::from_utf8_lossy(&payload));
            }
            Ok((OP_FRAME, payload)) => {
                let frame: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                match frame.get("cmd").and_then(Value::as_str) {
                    Some("DISPATCH") if frame.get("evt").and_then(Value::as_str) == Some("READY") => {
                        connected.store(true, Ordering::Relaxed);
                    }
                    Some("SET_ACTIVITY") => {}
                    _ => {}
                }
            }
            Ok((OP_CLOSE, _)) | Err(_) => {
                connected.store(false, Ordering::Relaxed);
                if let Ok(mut guard) = shared.lock() {
                    guard.pipe = None;
                }
            }
            Ok(_) => {}
        }
    }
}

fn activity_from(value: &Value) -> Value {
    let title = match value.get("title").and_then(Value::as_str) {
        Some(t) if t.chars().count() >= 2 => t.chars().take(128).collect::<String>(),
        Some(t) => format!("{t} ").chars().take(2).collect(),
        None => "Unknown track ".to_string(),
    };
    let artist = match value.get("artist").and_then(Value::as_str) {
        Some(a) if a.chars().count() >= 2 => a.chars().take(128).collect::<String>(),
        Some(a) => format!("{a} ").chars().take(2).collect(),
        None => "Unknown artist".to_string(),
    };
    let duration_secs = value.get("duration").and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
    let start = value
        .get("startTimestamp")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    let mut activity = json!({
        "type": 2,
        "details": title,
        "state": artist,
        "instance": false,
        "timestamps": { "start": start },
    });
    if duration_secs > 0.0 {
        activity["timestamps"]["end"] = json!(start + duration_secs.floor() as i64);
    }
    if let Some(url) = value.get("url").and_then(Value::as_str) {
        if url.starts_with("https://") && url.len() <= 512 {
            activity["buttons"] = json!([{ "label": "Listen", "url": url }]);
        }
    }
    if let Some(thumb) = value.get("thumbnail").and_then(Value::as_str) {
        if thumb.starts_with("https://") || thumb.starts_with("http://") {
            activity["assets"] = json!({ "large_image": thumb });
        }
    }
    activity["status_display_type"] = json!(1);
    activity
}

fn call_set_activity(pipe: &Arc<Mutex<File>>, activity: &Value, pid: u32) -> std::io::Result<()> {
    let nonce = format!("{:016x}", rand::random::<u64>());
    let frame = json!({ "cmd": "SET_ACTIVITY", "args": { "pid": pid, "activity": activity }, "evt": null, "nonce": nonce }).to_string();
    let mut file = pipe.lock().unwrap_or_else(|p| p.into_inner());
    write_packet(&mut file, OP_FRAME, &frame)
}

fn call_set_clear(pipe: &Arc<Mutex<File>>, pid: u32) -> std::io::Result<()> {
    let nonce = format!("{:016x}", rand::random::<u64>());
    let frame = json!({ "cmd": "SET_ACTIVITY", "args": { "pid": pid }, "evt": null, "nonce": nonce }).to_string();
    let mut file = pipe.lock().unwrap_or_else(|p| p.into_inner());
    write_packet(&mut file, OP_FRAME, &frame)
}

fn write_activity(shared: &Arc<Mutex<Shared>>, activity: &Value, connected: &Arc<AtomicBool>) {
    let pipe = shared.lock().ok().and_then(|s| s.pipe.clone());
    let Some(pipe) = pipe else {
        // Trigger a connect attempt immediately; the retry loop will re-send.
        if let Ok(mut guard) = shared.lock() {
            guard.pipe = None;
        }
        return;
    };
    let pid = std::process::id();
    match call_set_activity(&pipe, &activity_from(activity), pid) {
        Ok(()) => connected.store(true, Ordering::Relaxed),
        Err(_) => {
            connected.store(false, Ordering::Relaxed);
            if let Ok(mut guard) = shared.lock() {
                guard.pipe = None;
            }
        }
    }
}

fn write_clear(shared: &Arc<Mutex<Shared>>, connected: &Arc<AtomicBool>) {
    let pipe = shared.lock().ok().and_then(|s| s.pipe.clone());
    let Some(pipe) = pipe else {
        connected.store(false, Ordering::Relaxed);
        return;
    };
    let pid = std::process::id();
    match call_set_clear(&pipe, pid) {
        Ok(()) => connected.store(true, Ordering::Relaxed),
        Err(_) => {
            connected.store(false, Ordering::Relaxed);
            if let Ok(mut guard) = shared.lock() {
                guard.pipe = None;
            }
        }
    }
}

