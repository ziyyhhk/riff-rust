//! Discord Rich Presence over the local IPC named pipe.
//!
//! Framing: `<u32 LE opcode><u32 LE payload len><utf8 JSON payload>`
//! Opcodes: Handshake 0, Frame 1, Close 2, Ping 3, Pong 4
//!
//! Windows serializes synchronous I/O on a pipe: a thread blocked in
//! `read_exact` also blocks writes on a duplicated handle. The old
//! reader/writer pair could therefore deadlock after the handshake, leaving
//! the app marked connected while no SET_ACTIVITY frame ever reached Discord.
//! A single thread now peeks for incoming packets and writes only when the
//! pipe is idle.

use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const CLIENT_ID: &str = "1520100510877749358";
const RECONNECT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);
const MAX_PACKET: usize = 64 * 1024;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

#[link(name = "kernel32")]
extern "system" {
    fn PeekNamedPipe(
        pipe: *mut c_void,
        buffer: *mut c_void,
        buffer_size: u32,
        bytes_read: *mut u32,
        bytes_available: *mut u32,
        bytes_left: *mut u32,
    ) -> i32;
}

fn pipe_path(id: u32) -> PathBuf {
    // Windows: IPC pipe path uses the \\?\ prefix.
    PathBuf::from(format!("\\\\?\\pipe\\discord-ipc-{id}"))
}

fn write_packet(file: &mut File, op: u32, payload: &str) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(&op.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload.as_bytes());
    file.write_all(&buf)
}

/// Return the next packet's opcode and payload length once the whole packet
/// is buffered, without consuming it. `None` means nothing is ready yet.
fn ready_packet(file: &File) -> std::io::Result<Option<(u32, usize)>> {
    let handle = file.as_raw_handle() as *mut c_void;
    let mut available = 0u32;
    let ok = unsafe {
        PeekNamedPipe(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if available < 8 {
        return Ok(None);
    }
    let mut head = [0u8; 8];
    let mut read = 0u32;
    let ok = unsafe {
        PeekNamedPipe(
            handle,
            head.as_mut_ptr() as *mut c_void,
            8,
            &mut read,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if read < 8 {
        return Ok(None);
    }
    let op = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len > MAX_PACKET {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "discord rpc packet too large",
        ));
    }
    if available as usize >= 8 + len {
        Ok(Some((op, len)))
    } else {
        Ok(None)
    }
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

#[derive(Debug)]
pub enum Cmd {
    Set(Value),
    Clear,
    Reconnect,
}

struct Shared {
    enabled: bool,
}

/// Handle for Discord Rich Presence. Clone-friendly; the background thread
/// lives for the life of the process.
#[allow(dead_code)]
pub struct DiscordRpc {
    shared: Arc<Mutex<Shared>>,
    connected: Arc<AtomicBool>,
    tx: Mutex<mpsc::Sender<Cmd>>,
    threads: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl DiscordRpc {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared { enabled: false }));
        let connected = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let worker = {
            let shared = Arc::clone(&shared);
            let connected = Arc::clone(&connected);
            thread::Builder::new()
                .name("discord-rpc".into())
                .spawn(move || rpc_loop(shared, connected, rx))
                .expect("spawn discord-rpc")
        };

        Self {
            shared,
            connected,
            tx: Mutex::new(tx),
            threads: Mutex::new(vec![worker]),
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> bool {
        if let Ok(mut guard) = self.shared.lock() {
            guard.enabled = enabled;
            let cmd = if enabled { Cmd::Reconnect } else { Cmd::Clear };
            let _ = self.tx.lock().map(|tx| tx.send(cmd));
        }
        if !enabled {
            self.connected.store(false, Ordering::Relaxed);
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
        if !self.is_enabled() {
            return;
        }
        let title = track_info.get("title").and_then(Value::as_str).unwrap_or("?").to_string();
        let artist = track_info.get("artist").and_then(Value::as_str).unwrap_or("?").to_string();
        crate::util::log_line("INFO", &format!("discord rpc: activity update received ({artist} — {title})"));
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Cmd::Set(track_info));
        }
    }

    pub fn clear_activity(&self) {
        if !self.is_enabled() {
            return;
        }
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Cmd::Clear);
        }
    }

    pub fn reconnect(&self) {
        if !self.is_enabled() {
            return;
        }
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Cmd::Reconnect);
        }
    }
}

fn try_connect() -> Option<File> {
    for id in 0..10 {
        let path = pipe_path(id);
        let Ok(mut file) = File::options().read(true).write(true).open(&path) else {
            continue;
        };
        let handshake = format!(r#"{{"v":1,"client_id":"{CLIENT_ID}"}}"#);
        if write_packet(&mut file, OP_HANDSHAKE, &handshake).is_ok() {
            return Some(file);
        }
    }
    None
}

fn rpc_loop(shared: Arc<Mutex<Shared>>, connected: Arc<AtomicBool>, rx: mpsc::Receiver<Cmd>) {
    let mut pipe: Option<File> = None;
    let mut last_activity: Option<Value> = None;
    let mut reconnect_at = Instant::now() - RECONNECT;
    let mut warned_missing = false;

    loop {
        match rx.recv_timeout(POLL) {
            Ok(Cmd::Set(activity)) => {
                last_activity = Some(activity.clone());
                let mut failed = false;
                if let Some(file) = pipe.as_mut() {
                    failed = !send_presence(file, &activity, &connected);
                } else {
                    reconnect_at = Instant::now() - RECONNECT;
                }
                if failed {
                    pipe = None;
                }
            }
            Ok(Cmd::Clear) => {
                last_activity = None;
                let mut failed = false;
                if let Some(file) = pipe.as_mut() {
                    failed = send_clear(file).is_err();
                }
                if failed {
                    connected.store(false, Ordering::Relaxed);
                    pipe = None;
                }
            }
            Ok(Cmd::Reconnect) => {
                pipe = None;
                connected.store(false, Ordering::Relaxed);
                reconnect_at = Instant::now() - RECONNECT;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        let enabled = shared.lock().map(|s| s.enabled).unwrap_or(false);
        if !enabled {
            if let Some(file) = pipe.as_mut() {
                let _ = send_clear(file);
            }
            pipe = None;
            last_activity = None;
            connected.store(false, Ordering::Relaxed);
            continue;
        }

        if let Some(mut file) = pipe.take() {
            let mut keep = true;
            loop {
                match ready_packet(&file) {
                    Ok(Some(_)) => match read_packet(&mut file) {
                        Ok((OP_PING, payload)) => {
                            let text = String::from_utf8_lossy(&payload);
                            if write_packet(&mut file, OP_PONG, &text).is_err() {
                                keep = false;
                                break;
                            }
                        }
                        Ok((OP_FRAME, payload)) => {
                            if !handle_frame(&mut file, &payload, &connected, &mut last_activity) {
                                keep = false;
                                break;
                            }
                        }
                        Ok((OP_CLOSE, _)) => {
                            crate::util::log_line("WARN", "discord rpc: pipe closed by Discord");
                            keep = false;
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            crate::util::log_line("WARN", &format!("discord rpc: read failed: {e}"));
                            keep = false;
                            break;
                        }
                    },
                    Ok(None) => break,
                    Err(e) => {
                        crate::util::log_line("WARN", &format!("discord rpc: pipe lost: {e}"));
                        keep = false;
                        break;
                    }
                }
            }
            if keep {
                pipe = Some(file);
            } else {
                connected.store(false, Ordering::Relaxed);
            }
        }

        if pipe.is_none() && reconnect_at.elapsed() >= RECONNECT {
            reconnect_at = Instant::now();
            match try_connect() {
                Some(mut file) => {
                    if let Some(activity) = last_activity.as_ref() {
                        if !send_presence(&mut file, activity, &connected) {
                            continue;
                        }
                    }
                    pipe = Some(file);
                    warned_missing = false;
                }
                None => {
                    if !warned_missing {
                        crate::util::log_line("WARN", "discord rpc: Discord not running; retrying");
                        warned_missing = true;
                    }
                }
            }
        }
    }
}

fn handle_frame(
    file: &mut File,
    payload: &[u8],
    connected: &AtomicBool,
    last_activity: &mut Option<Value>,
) -> bool {
    let frame: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
    match frame.get("cmd").and_then(Value::as_str) {
        Some("DISPATCH") if frame.get("evt").and_then(Value::as_str) == Some("READY") => {
            if !connected.swap(true, Ordering::Relaxed) {
                crate::util::log_line("INFO", "discord rpc connected");
            }
            // Discord may accept SET_ACTIVITY before READY, but re-assert it
            // once the session is ready so the presence is never dropped.
            if let Some(activity) = last_activity.as_ref() {
                send_presence(file, activity, connected);
            }
            true
        }
        Some("SET_ACTIVITY") | Some("ERROR") => {
            if let Some(code) = frame.get("data").and_then(|d| d.get("code")).and_then(Value::as_i64) {
                if code != 0 {
                    let message = frame
                        .get("data")
                        .and_then(|d| d.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    crate::util::log_line("WARN", &format!("discord rpc error {code}: {message}"));
                }
            }
            true
        }
        _ => true,
    }
}

fn activity_from(value: &Value) -> Value {
    // Non-playing presence: "Riff Rust — Browsing X" / "Searching songs" etc.
    // The renderer sends this instead of a track activity when nothing plays.
    if value.get("mode").and_then(Value::as_str) == Some("browsing") {
        let details = value.get("details").and_then(Value::as_str).unwrap_or("Riff Rust").chars().take(128).collect::<String>();
        let mut activity = json!({ "type": 0, "details": details, "instance": false });
        if let Some(state) = value.get("state").and_then(Value::as_str) {
            if !state.is_empty() {
                activity["state"] = json!(state.chars().take(128).collect::<String>());
            }
        }
        if let Some(thumb) = value.get("thumbnail").and_then(Value::as_str) {
            if thumb.starts_with("https://") {
                activity["assets"] = json!({ "large_image": thumb });
            }
        }
        if let Some(url) = value.get("url").and_then(Value::as_str) {
            if url.starts_with("https://") && url.len() <= 512 {
                activity["buttons"] = json!([{ "label": "Open Riff Rust", "url": url }]);
            }
        }
        activity["status_display_type"] = json!(1);
        return activity;
    }

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

fn call_set_activity(file: &mut File, activity: &Value) -> std::io::Result<()> {
    let nonce = format!("{:016x}", rand::random::<u64>());
    let frame = json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "evt": null,
        "nonce": nonce
    })
    .to_string();
    write_packet(file, OP_FRAME, &frame)
}

fn send_clear(file: &mut File) -> std::io::Result<()> {
    let nonce = format!("{:016x}", rand::random::<u64>());
    let frame = json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id() },
        "evt": null,
        "nonce": nonce
    })
    .to_string();
    write_packet(file, OP_FRAME, &frame)
}

fn send_presence(file: &mut File, activity: &Value, connected: &AtomicBool) -> bool {
    match call_set_activity(file, &activity_from(activity)) {
        Ok(()) => {
            let label = if activity.get("mode").and_then(Value::as_str) == Some("browsing") {
                activity.get("state").and_then(Value::as_str).unwrap_or("Browsing").to_string()
            } else {
                let title = activity.get("title").and_then(Value::as_str).unwrap_or("?");
                let artist = activity.get("artist").and_then(Value::as_str).unwrap_or("?");
                format!("{artist} — {title}")
            };
            crate::util::log_line("INFO", &format!("discord activity sent: {label}"));
            true
        }
        Err(e) => {
            connected.store(false, Ordering::Relaxed);
            crate::util::log_line("WARN", &format!("discord activity write failed: {e}"));
            false
        }
    }
}
