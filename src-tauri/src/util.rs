use std::fs;
use std::path::PathBuf;

/// `%LOCALAPPDATA%\Riff Rust Boot\data-dir.json` (Windows),
/// `~/Library/Application Support/Riff Rust Boot/data-dir.json` (macOS),
/// `$XDG_DATA_HOME/riff-rust-boot/data-dir.json` or `~/.local/share/riff-rust-boot/data-dir.json`
/// (Linux). Fixed per-user location for the bootstrap pointer. It deliberately lives
/// OUTSIDE the data directory: the pointer says where the data dir is, so it cannot
/// live inside it (chicken and egg).
pub fn bootstrap_pointer_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(local).join("Riff Rust Boot").join("data-dir.json");
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("AppData\\Local\\Riff Rust Boot\\data-dir.json");
        }
        PathBuf::from("Riff Rust Boot").join("data-dir.json")
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Application Support/Riff Rust Boot/data-dir.json");
        }
        PathBuf::from("Riff Rust Boot/data-dir.json")
    }
    #[cfg(target_os = "android")]
    {
        PathBuf::from("/data/user/0/io.github.rootscripts.riffrust/files/boot")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "android")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("riff-rust-boot/data-dir.json");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".local/share/riff-rust-boot/data-dir.json");
        }
        PathBuf::from("riff-rust-boot/data-dir.json")
    }
}

/// Platform default data dir; the bootstrap pointer is NOT consulted.
/// `%LOCALAPPDATA%\Riff Rust` (Windows), `~/Library/Application Support/Riff Rust` (macOS),
/// `~/.local/share/Riff Rust` (Linux). Mirrors Riff's data-dir convention but under the new name.
pub fn default_data_dir() -> PathBuf {
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
    #[cfg(target_os = "android")]
    {
        PathBuf::from("/data/user/0/io.github.rootscripts.riffrust/files/data")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "android")))]
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

/// Read the bootstrap pointer's `dataDir` value, if the pointer exists and parses.
/// Does not validate that the target is usable; `data_dir()` does that.
pub fn read_data_dir_pointer() -> Option<String> {
    let raw = fs::read_to_string(bootstrap_pointer_path()).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let dir = parsed
        .get("dataDir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if dir.is_empty() {
        None
    } else {
        Some(dir.to_string())
    }
}

/// The directory the app reads and writes. When the bootstrap pointer
/// (`bootstrap_pointer_path()`) names a directory that exists or can be created,
/// that directory wins; otherwise the platform default is used and a WARN is
/// logged (via `log_line`). Never panics, never deletes anything.
///
/// Memoized for the process lifetime: a changed pointer takes effect on the
/// next launch, not mid-run. That is the honest semantics of the original
/// env-derived design too (a new env var never moved the dir mid-process either).
pub fn data_dir() -> PathBuf {
    static DATA_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    static POINTER_WARN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static POINTER_WARN_LOGGED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    let dir = DATA_DIR
        .get_or_init(|| {
            if let Some(custom) = read_data_dir_pointer() {
                let candidate = PathBuf::from(&custom);
                // create_dir_all succeeds for an existing dir too, so this covers
                // both "pointer target exists" and "target can be created".
                match fs::create_dir_all(&candidate) {
                    Ok(()) => return candidate,
                    Err(e) => {
                        let _ = POINTER_WARN.set(format!(
                            "bootstrap pointer {custom:?} is unusable ({e}); falling back to the default data dir"
                        ));
                    }
                }
            }
            default_data_dir()
        })
        .clone();
    // Logged here, after the OnceLock initializer has returned: log_line() goes
    // through logs_dir() -> data_dir(), which would re-enter the initializing
    // OnceLock on this thread. Exactly one WARN is emitted per process.
    if !POINTER_WARN_LOGGED.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(message) = POINTER_WARN.get() {
            if !POINTER_WARN_LOGGED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log_line("WARN", message);
            }
        }
    }
    dir
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
        // A truncated 0-byte binary from a failed copy used to count as
        // "installed" and every yt-dlp call failed at spawn. Require a real
        // payload before trusting the path.
        let usable = candidate
            .metadata()
            .map(|meta| meta.is_file() && meta.len() >= 65_536)
            .unwrap_or(false);
        if usable {
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
        "jpg" | "jpeg" | "jfif" | "jif" | "jpe" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
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

/// Auto-purge of the stream cache / offline library (both live in `tracks/`).
/// Intervals come from the renderer storage map: `riff_cache_clear_days`
/// (default 7) and `riff_offline_clear_days` (default 30); 0 disables a purge.
/// Last-run timestamps persist in `auto-clean.json`; a missing file is seeded
/// with "now" so an app update never wipes an existing library on first launch.
/// Returns bytes freed.
pub fn auto_clean_tick() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = data_dir().join("auto-clean.json");
    let mut stamps = read_json_file(&path);
    if !stamps.is_object() {
        stamps = serde_json::json!({});
    }
    if stamps.get("lastCacheClean").is_none() && stamps.get("lastOfflineClean").is_none() {
        let _ = write_json_file(
            &path,
            &serde_json::json!({ "lastCacheClean": now, "lastOfflineClean": now }),
        );
        return 0;
    }
    let storage = read_json_file(&storage_path());
    let read_days = |key: &str, default: u64| -> u64 {
        storage
            .get(key)
            .and_then(|v| {
                v.as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
            })
            .map(|d| d.max(0.0) as u64)
            .unwrap_or(default)
    };
    let mut freed = 0u64;
    let due = |days: u64, stamp: &str| -> bool {
        if days == 0 {
            return false;
        }
        let last = stamps.get(stamp).and_then(|v| v.as_u64()).unwrap_or(0);
        now.saturating_sub(last) >= days.saturating_mul(86_400)
    };
    let cache_days = read_days("riff_cache_clear_days", 7);
    let offline_days = read_days("riff_offline_clear_days", 30);
    if due(cache_days, "lastCacheClean") || due(offline_days, "lastOfflineClean") {
        // Both settings target the same tracks/ folder: the existing Clear cache
        // and Clear offline library buttons do too. One sweep satisfies both, so
        // stamp both timers to avoid a redundant second pass on the next tick.
        freed += clear_dir_contents(&cache_dir());
        stamps["lastCacheClean"] = serde_json::json!(now);
        stamps["lastOfflineClean"] = serde_json::json!(now);
        let _ = write_json_file(&path, &stamps);
        log_line("INFO", &format!("auto-clean freed {freed} bytes from tracks/"));
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
