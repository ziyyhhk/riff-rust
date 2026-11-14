use std::fs;
use std::path::PathBuf;

/// `%LOCALAPPDATA%\Riff Rust` (Windows), `~/Library/Application Support/Riff Rust` (macOS),
/// `~/.local/share/Riff Rust` (Linux). Mirrors Riff's data-dir convention but under the new name.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(local).join("Riff Rust");
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("AppData\\Local\\Riff Rust");
        }
        PathBuf::from("Riff Rust")
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Application Support/Riff Rust");
        }
        PathBuf::from("Riff Rust")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("Riff Rust");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".local/share/Riff Rust");
        }
        PathBuf::from("Riff Rust")
    }
}

pub fn library_dir() -> PathBuf {
    data_dir().join("library")
}
pub fn library_path() -> PathBuf {
    data_dir().join("library.json")
}
pub fn cache_dir() -> PathBuf {
    data_dir().join("tracks")
}
pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}
pub fn covers_dir() -> PathBuf {
    data_dir().join("local-covers")
}
pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}
pub fn storage_path() -> PathBuf {
    data_dir().join("storage.json")
}

/// Window geometry, same JSON shape as upstream Electron (`bounds`/`isMaximized`/`isFullScreen`).
pub fn window_state_path() -> PathBuf {
    data_dir().join("window-state.json")
}
pub fn playlists_path() -> PathBuf {
    data_dir().join("playlists.json")
}
pub fn themes_state_path() -> PathBuf {
    data_dir().join("themes-state.json")
}
pub fn plugins_state_path() -> PathBuf {
    data_dir().join("plugins-state.json")
}

pub fn ensure_dirs() {
    for dir in [data_dir(), library_dir(), cache_dir(), bin_dir(), logs_dir(), covers_dir()] {
        let _ = fs::create_dir_all(&dir);
    }
}

pub fn read_json_file(path: &PathBuf) -> serde_json::Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(serde_json::json!({}))
}

pub fn write_json_file(path: &PathBuf, value: &serde_json::Value) -> bool {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into())).is_ok()
}

pub fn dir_size(path: &PathBuf) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.clone()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(meta) = p.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Find a helper binary (yt-dlp / ffmpeg): env override, app bin dir, bundled resources, PATH,
/// then the usual Windows install locations (mirrors Riff's resolveBinary).
pub fn resolve_binary(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let env_key = format!("RIFF_{}", name.replace(['-', '_'], "").to_uppercase());

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(custom) = std::env::var(&env_key) {
        candidates.push(PathBuf::from(custom));
    }
    candidates.push(bin_dir().join(&exe));
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            candidates.push(dir.join("bin").join(&exe));
            candidates.push(dir.join(&exe));
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(&local).join("Microsoft\\WinGet\\Links").join(&exe));
        }
        if let Ok(home) = std::env::var("USERPROFILE") {
            candidates.push(PathBuf::from(&home).join("scoop\\shims").join(&exe));
        }
        candidates.push(PathBuf::from("C:\\ProgramData\\chocolatey\\bin").join(&exe));
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(PathBuf::from(&home).join(".local/bin").join(&exe));
        }
        candidates.push(PathBuf::from("/opt/homebrew/bin").join(&exe));
        candidates.push(PathBuf::from("/usr/local/bin").join(&exe));
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // PATH lookup
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn sha1_hex(data: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(data);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn random_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..24).map(|_| format!("{:02x}", rng.gen::<u8>())).collect()
}

/// Remove the contents of a directory (used by "clear downloads cache").
pub fn clear_dir_contents(path: &PathBuf) -> u64 {
    let mut removed = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            let size = if p.is_dir() { dir_size(&p) } else { p.metadata().map(|m| m.len()).unwrap_or(0) };
            let result = if p.is_dir() {
                fs::remove_dir_all(&p)
            } else {
                fs::remove_file(&p)
            };
            if result.is_ok() {
                removed += size;
            }
        }
    }
    removed
}

pub fn mime_from_ext(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "m4a" | "aac" => "audio/mp4",
        "webm" | "weba" => "audio/webm",
        "mp4" => "video/mp4",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
const LOG_FILES: usize = 3;

/// Serializes rotation + append so concurrent log writers cannot rotate twice.
static LOG_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

fn rotate_log_file(path: &PathBuf) {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < LOG_MAX_BYTES {
        return;
    }
    // riff-rust.log.2 -> .3, .1 -> .2, base -> .1. Windows rename fails when the
    // destination exists, so remove it first.
    for index in (1..LOG_FILES).rev() {
        let from = PathBuf::from(format!("{}.{index}", path.to_string_lossy()));
        if !from.exists() {
            continue;
        }
        let to = PathBuf::from(format!("{}.{}", path.to_string_lossy(), index + 1));
        let _ = fs::remove_file(&to);
        let _ = fs::rename(from, to);
    }
    let backup = PathBuf::from(format!("{}.1", path.to_string_lossy()));
    let _ = fs::remove_file(&backup);
    let _ = fs::rename(path, backup);
}

/// Write to the app log (best effort), rotating at 4 MB and keeping three old files.
/// Hide the console window of a child process on Windows (CREATE_NO_WINDOW).
/// Console binaries spawned from the GUI app otherwise allocate a visible
/// console; `stdout`/`stderr` capture alone is not enough.
pub fn hide_console(cmd: &mut std::process::Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
}

pub fn log_line(level: &str, message: &str) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = logs_dir();
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("riff-rust.log");
    rotate_log_file(&path);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{stamp}] [{level}] {message}\n");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map(|mut f| {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        });
}

/// Optional disk guard for the stream/download cache. `config.json` may set
/// `"cacheLimitMB": <n>`; 0/absent keeps the upstream behaviour (no automatic deletion),
/// because these files are also the user's offline library.
pub fn prune_cache_limit() -> u64 {
    let config = read_json_file(&config_path());
    let limit_mb = config
        .get("cacheLimitMB")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if limit_mb == 0 {
        return 0;
    }
    let limit = limit_mb.saturating_mul(1024 * 1024);
    let dir = cache_dir();
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(meta) = path.metadata() {
                let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((modified, meta.len(), path));
                total += meta.len();
            }
        }
    }
    if total <= limit {
        return 0;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    // Trim to 90% of the cap so we do not run this on every launch.
    let target = limit.saturating_mul(9) / 10;
    let mut freed = 0u64;
    for (_, size, path) in files {
        if total <= target {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
            freed += size;
        }
    }
    freed
}

/// Percent-encode a full string (for building outbound URLs).
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
