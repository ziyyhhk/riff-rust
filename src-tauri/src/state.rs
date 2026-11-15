use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Saved geometry while the main window is in mini-player mode.
#[derive(Clone, Copy, Debug)]
pub struct MiniWindowState {
    pub width: f64,
    pub height: f64,
    pub x: i32,
    pub y: i32,
    pub maximized: bool,
}

/// Progress of the background yt-dlp/ffmpeg installer. The renderer polls
/// `/api/tools/status` every 500 ms and closes its modal when everything resolves.
#[derive(Clone, Debug)]
pub struct ToolInstallState {
    /// idle | downloading | done | error
    pub status: String,
    /// "yt-dlp" or "ffmpeg"
    pub tool: String,
    pub percent: u8,
    pub error: String,
    pub ffmpeg_error: String,
}

impl Default for ToolInstallState {
    fn default() -> Self {
        Self {
            status: "idle".to_string(),
            tool: String::new(),
            percent: 100,
            error: String::new(),
            ffmpeg_error: String::new(),
        }
    }
}

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
    /// Renderer has told us its audio graph is ready, so media keys may be grabbed.
    pub media_keys_active: Mutex<bool>,
    /// Cached reqwest client, keyed by proxy URL. Rebuilding a client per request
    /// throws away the connection pool and forces a fresh TLS handshake every time.
    pub http: Mutex<Option<(String, reqwest::Client)>>,
    /// Resolved stream URLs (short TTL; URLs expire and are signature-bound).
    pub stream_cache: Mutex<HashMap<String, (Instant, Value)>>,
    /// YouTube Music search results (short TTL).
    pub search_cache: Mutex<HashMap<String, (Instant, Vec<Value>)>>,
    /// Pre-mini-player window geometry.
    pub mini: Mutex<Option<MiniWindowState>>,
    /// Window height before mini-player lyrics were expanded.
    pub mini_lyrics_height: Mutex<Option<f64>>,
    /// Last visibility value forwarded to the renderer (avoid event spam on resize).
    pub window_visible: Mutex<Option<bool>>,
    /// Throttles window-state.json writes during drag/resize.
    pub last_window_save: Mutex<Instant>,
    /// Wizard result waiting for index.html to finish loading.
    pub pending_welcome_result: Mutex<Option<Value>>,
    /// Background installer progress (yt-dlp/ffmpeg) reported to the renderer.
    pub tool_install: Mutex<ToolInstallState>,
    /// Discord Rich Presence bridge (named pipe \?\pipe\discord-ipc-N).
    pub discord: Arc<crate::discord_rpc::DiscordRpc>,
    /// LRCLIB lyric responses (24h TTL).
    pub lyrics_cache: Mutex<HashMap<String, (Instant, Value)>>,
    /// Word-level (NetEase yrc) lyric responses (6h pos / 10min neg).
    pub lyrics_words_cache: Mutex<HashMap<String, (Instant, Value)>>,
    /// Scraped SoundCloud `client_id` (12h TTL), shared by search and resolve.
    pub soundcloud_client_id: Mutex<Option<(String, Instant)>>,
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
            media_keys_active: Mutex::new(false),
            http: Mutex::new(None),
            stream_cache: Mutex::new(HashMap::new()),
            search_cache: Mutex::new(HashMap::new()),
            mini: Mutex::new(None),
            mini_lyrics_height: Mutex::new(None),
            window_visible: Mutex::new(None),
            last_window_save: Mutex::new(Instant::now()),
            pending_welcome_result: Mutex::new(None),
            tool_install: Mutex::new(ToolInstallState::default()),
            discord: Arc::new(crate::discord_rpc::DiscordRpc::new()),
            lyrics_cache: Mutex::new(HashMap::new()),
            lyrics_words_cache: Mutex::new(HashMap::new()),
            soundcloud_client_id: Mutex::new(None),
        }
    }

    /// Shared HTTP client with connection pooling. Rebuilt only when the proxy changes.
    pub fn http(&self) -> reqwest::Client {
        let proxy = self.proxy_url.lock().map(|p| p.clone()).unwrap_or_default();
        let mut slot = match self.http.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((cached_proxy, client)) = slot.as_ref() {
            if cached_proxy == &proxy {
                return client.clone();
            }
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(6))
            .timeout(Duration::from_secs(8))
            .pool_max_idle_per_host(16)
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) RiffRust/0.1")
            .gzip(true);
        if !proxy.trim().is_empty() {
            match reqwest::Proxy::all(proxy.trim()) {
                Ok(p) => builder = builder.proxy(p),
                Err(e) => crate::util::log_line("WARN", &format!("ignoring invalid proxy {proxy:?}: {e}")),
            }
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        *slot = Some((proxy, client.clone()));
        client
    }

    /// Forward window visibility to the renderer only when it changes.
    pub fn set_window_visible(&self, visible: bool) -> bool {
        let mut last = match self.window_visible.lock() {
            Ok(last) => last,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *last == Some(visible) {
            return false;
        }
        *last = Some(visible);
        true
    }

    /// Poison-tolerant access to the background installer progress.
    pub fn tool_install(&self) -> std::sync::MutexGuard<'_, ToolInstallState> {
        self.tool_install.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// True when both the setting and the renderer's audio readiness allow media keys.
    pub fn media_keys_wanted(&self) -> bool {
        let enabled = self.media_keys.lock().map(|v| *v).unwrap_or(false);
        let active = self.media_keys_active.lock().map(|v| *v).unwrap_or(false);
        enabled && active
    }
}

/// Read a value from a TTL cache, dropping it when stale.
pub fn cache_get<V: Clone>(
    map: &Mutex<HashMap<String, (Instant, V)>>,
    key: &str,
    ttl: Duration,
) -> Option<V> {
    let mut map = match map.lock() {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    };
    match map.get(key) {
        Some((at, value)) if at.elapsed() < ttl => Some(value.clone()),
        Some(_) => {
            map.remove(key);
            None
        }
        None => None,
    }
}

/// Insert into a bounded TTL cache. Stale entries are dropped before a hard cap is applied.
pub fn cache_put<V>(
    map: &Mutex<HashMap<String, (Instant, V)>>,
    key: String,
    value: V,
    cap: usize,
    ttl: Duration,
) {
    let mut map = match map.lock() {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    };
    if map.len() >= cap {
        map.retain(|_, (at, _)| at.elapsed() < ttl);
        if map.len() >= cap {
            map.clear();
        }
    }
    map.insert(key, (Instant::now(), value));
}
