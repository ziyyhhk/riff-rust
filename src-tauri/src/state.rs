use std::sync::Mutex;

/// Process-wide shared state. Everything here is deliberately small; payload-heavy data
/// (library contents, storage snapshot) lives on disk.
pub struct AppState {
    /// Port of the embedded HTTP server (ephemeral, 127.0.0.1).
    pub server_port: u16,
    /// Per-run token. Reserved for /api auth hardening (currently localhost-only trust).
    #[allow(dead_code)]
    pub server_token: String,
    /// Tray "close to tray" behaviour (mirrors Riff's tray-on-close setting).
    pub tray_on_close: Mutex<bool>,
    /// True while the user asked to quit, so the close handler lets the window destroy.
    pub quitting: Mutex<bool>,
    /// Proxy URL for server-side HTTP fetches and yt-dlp (empty = direct).
    pub proxy_url: Mutex<String>,
    /// Custom download directory from config.json (empty = OS default).
    pub download_dir: Mutex<String>,
    /// Multi-instance experimental flag.
    pub multi_instance: Mutex<bool>,
    /// Global shortcuts enabled flag.
    pub global_shortcuts: Mutex<bool>,
    /// Media keys enabled flag.
    pub media_keys: Mutex<bool>,
}

impl AppState {
    pub fn new(server_port: u16, server_token: String, download_dir: String) -> Self {
        Self {
            server_port,
            server_token,
            tray_on_close: Mutex::new(true),
            quitting: Mutex::new(false),
            proxy_url: Mutex::new(String::new()),
            download_dir: Mutex::new(download_dir),
            multi_instance: Mutex::new(false),
            global_shortcuts: Mutex::new(true),
            media_keys: Mutex::new(true),
        }
    }
}
