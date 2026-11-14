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

/// Write to the app log (best effort).
pub fn log_line(level: &str, message: &str) {    let dir = logs_dir();
    let _ = fs::create_dir_all(&dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{stamp}] [{level}] {message}\n");
    let _ = fs::OpenOptions::new().create(true).append(true).open(dir.join("riff-rust.log")).map(|mut f| {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    });
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
