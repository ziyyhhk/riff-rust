use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

use crate::state::AppState;
use crate::util;

type Shared = Arc<AppState>;

// ---------------------------------------------------------------- windowing

#[tauri::command]
pub fn get_server_port(state: State<'_, Shared>) -> u16 {
    state.server_port
}

#[tauri::command]
pub fn window_minimize(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.minimize();
    }
}

#[tauri::command]
pub fn window_maximize(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_maximized().unwrap_or(false) {
            let _ = w.unmaximize();
        } else {
            let _ = w.maximize();
        }
    }
}

#[tauri::command]
pub fn window_close(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.close();
    }
}

#[tauri::command]
pub fn set_window_buttons_visible(_visible: bool) -> bool {
    true
}

#[tauri::command]
pub fn toggle_mini_window() -> bool {
    false
}

#[tauri::command]
pub fn set_mini_lyrics(_enabled: bool) -> bool {
    true
}

#[tauri::command]
pub fn set_zoom_factor(_factor: f64) {}

#[tauri::command]
pub fn get_zoom_factor() -> f64 {
    1.0
}

#[tauri::command]
pub fn clear_web_frame_cache() -> bool {
    true
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

// ------------------------------------------------------------------- proxy

#[tauri::command]
pub fn set_app_proxy(proxy_url: String, state: State<'_, Shared>) -> bool {
    if let Ok(mut proxy) = state.proxy_url.lock() {
        *proxy = proxy_url;
    }
    true
}

#[tauri::command]
pub fn get_gpu_compositing() -> serde_json::Value {
    serde_json::json!({ "enabled": true, "backend": "webview2" })
}

// ------------------------------------------------------------ file picking
// (Dialogs are driven from the JS side through the dialog plugin; these
// commands do the filesystem work that has to happen natively.)

#[tauri::command]
pub fn import_audio_files(paths: Vec<String>) -> Vec<serde_json::Value> {
    crate::library::import_audio_files(&paths)
}

#[tauri::command]
pub fn rescan_local_metadata(paths: Option<Vec<String>>) -> bool {
    // Re-read tags for all library tracks (or the requested subset).
    let tracks = crate::library::load_library();
    let wanted: Option<std::collections::HashSet<String>> =
        paths.map(|p| p.into_iter().collect());
    for track in &tracks {
        let id = track.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if let Some(filter) = &wanted {
            if !filter.contains(&id) {
                continue;
            }
        }
        if let Some(path) = crate::library::find_track_by_id(&id) {
            let (title, artist, album, duration, _) = crate::library::read_tags(&path);
            let mut patch = serde_json::Map::new();
            if !title.is_empty() {
                patch.insert("title".into(), serde_json::json!(title));
            }
            if !artist.is_empty() {
                patch.insert("artist".into(), serde_json::json!(artist));
            }
            if !album.is_empty() {
                patch.insert("album".into(), serde_json::json!(album));
            }
            patch.insert("duration".into(), serde_json::json!(duration));
            crate::library::update_library_entry(&id, &patch);
        }
    }
    true
}

// ------------------------------------------------------------- discord RPC
// Not yet wired: Riff Rust v0.1 keeps the toggle state but reports
// disconnected. See PORTING.md.

#[tauri::command]
pub fn discord_rpc_update(_track_info: serde_json::Value, state: State<'_, Shared>) {
    let _ = state;
}

#[tauri::command]
pub fn discord_rpc_clear() {}

#[tauri::command]
pub fn discord_rpc_set_enabled(enabled: bool) -> bool {
    enabled
}

#[tauri::command]
pub fn discord_rpc_get_enabled() -> bool {
    false
}

#[tauri::command]
pub fn discord_rpc_get_connected() -> bool {
    false
}

#[tauri::command]
pub fn discord_rpc_reconnect() -> bool {
    false
}

// ------------------------------------------------------------------- cache

#[tauri::command]
pub fn get_cache_size() -> serde_json::Value {
    let audio = util::dir_size(&util::cache_dir());
    serde_json::json!({ "audio": audio, "web": 0 })
}

#[tauri::command]
pub fn clear_cache() -> serde_json::Value {
    let audio = util::clear_dir_contents(&util::cache_dir());
    serde_json::json!({ "success": true, "freedBytes": audio })
}

// ----------------------------------------------------------- data location

#[tauri::command]
pub fn get_data_dir() -> serde_json::Value {
    let dir = util::data_dir();
    serde_json::json!({
        "dir": dir.to_string_lossy(),
        "defaultDir": dir.to_string_lossy(),
        "isCustom": false,
        "platform": std::env::consts::OS,
    })
}

#[tauri::command]
pub fn set_data_dir(dir: String, move_files: bool) -> serde_json::Value {
    let from = util::data_dir();
    let to = PathBuf::from(&dir);
    let _ = fs::create_dir_all(&to);
    if move_files {
        const ENTRIES: [&str; 12] = [
            "tracks", "library", "metadata.json", "library.json", "startup.json",
            "custom-lyrics", "lyrics", "thumbnails", "local-covers", "bin", "themes", "plugins",
        ];
        for name in ENTRIES {
            let src = from.join(name);
            let dst = to.join(name);
            if src.exists() {
                if src.is_dir() {
                    let _ = copy_dir_recursive(&src, &dst);
                } else {
                    let _ = fs::copy(&src, &dst);
                }
            }
        }
    }
    // Persist the override for the next launch (mirrors Riff's config.json dataDir)
    let mut config = util::read_json_file(&util::config_path());
    config["dataDir"] = serde_json::json!(dir);
    let _ = util::write_json_file(&util::config_path(), &config);
    serde_json::json!({ "dir": dir, "success": true })
}

fn copy_dir_recursive(from: &PathBuf, to: &PathBuf) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            let _ = fs::copy(&src, &dst);
        }
    }
    Ok(())
}

#[tauri::command]
pub fn get_download_dir(state: State<'_, Shared>) -> serde_json::Value {
    let configured = state.download_dir.lock().map(|d| d.clone()).unwrap_or_default();
    let effective = if !configured.trim().is_empty() {
        configured.clone()
    } else {
        default_download_dir()
    };
    serde_json::json!({
        "dir": effective,
        "configured": configured,
        "defaultDir": default_download_dir(),
        "isCustom": !configured.trim().is_empty(),
    })
}

#[tauri::command]
pub fn set_download_dir(dir: Option<String>, reset: bool, state: State<'_, Shared>) -> serde_json::Value {
    let mut config = util::read_json_file(&util::config_path());
    let next = if reset {
        config.as_object_mut().map(|m| m.remove("downloadDir"));
        String::new()
    } else {
        let value = dir.unwrap_or_default();
        config["downloadDir"] = serde_json::json!(value);
        value
    };
    let _ = util::write_json_file(&util::config_path(), &config);
    if let Ok(mut d) = state.download_dir.lock() {
        *d = next;
    }
    serde_json::json!({ "success": true })
}

fn default_download_dir() -> String {
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return format!("{profile}\\Downloads");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/Downloads");
        }
    }
    "Downloads".into()
}

// ------------------------------------------------------------------ updates

#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub fn check_for_updates() -> serde_json::Value {
    // Riff Rust has no release feed wired up yet; see PORTING.md.
    serde_json::json!({ "available": false })
}

#[tauri::command]
pub fn update_action(_action: String) {}

#[tauri::command]
pub fn install_update_now() {}

// ------------------------------------------------------------------- misc

#[tauri::command]
pub fn set_tray_on_close(enabled: bool, state: State<'_, Shared>) {
    if let Ok(mut tray) = state.tray_on_close.lock() {
        *tray = enabled;
    }
}

#[tauri::command]
pub fn get_multi_instance(state: State<'_, Shared>) -> bool {
    state.multi_instance.lock().map(|v| *v).unwrap_or(false)
}

#[tauri::command]
pub fn set_multi_instance(v: bool, state: State<'_, Shared>) {
    if let Ok(mut m) = state.multi_instance.lock() {
        *m = v;
    }
}

#[tauri::command]
pub fn get_global_shortcuts(state: State<'_, Shared>) -> bool {
    state.global_shortcuts.lock().map(|v| *v).unwrap_or(true)
}

#[tauri::command]
pub fn set_global_shortcuts(v: bool, state: State<'_, Shared>) {
    if let Ok(mut m) = state.global_shortcuts.lock() {
        *m = v;
    }
}

#[tauri::command]
pub fn set_media_keys(v: bool, state: State<'_, Shared>) {
    if let Ok(mut m) = state.media_keys.lock() {
        *m = v;
    }
}

#[tauri::command]
pub fn media_keys_active(state: State<'_, Shared>) -> bool {
    state.media_keys.lock().map(|v| *v).unwrap_or(false)
}

#[tauri::command]
pub fn welcome_finish(_detail: serde_json::Value) {}

#[tauri::command]
pub fn get_launcher_info() -> serde_json::Value {
    serde_json::json!({ "installed": false, "supported": false, "version": "" })
}

#[tauri::command]
pub fn install_launcher() -> serde_json::Value {
    serde_json::json!({ "success": false, "error": "launcher integration is not part of Riff Rust v0.1" })
}

#[tauri::command]
pub fn log_error(level: String, message: String, stack: Option<String>) {
    let detail = stack.map(|s| format!("{message} | {s}")).unwrap_or(message);
    util::log_line(&level.to_uppercase(), &detail);
}

#[tauri::command]
pub fn open_logs_folder(_app: AppHandle) -> bool {
    let dir = util::logs_dir();
    let _ = fs::create_dir_all(&dir);
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
        true
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&dir).spawn();
        true
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
        true
    }
}

#[tauri::command]
pub fn open_external(u: String) {
    if !(u.starts_with("https://") || u.starts_with("http://") || u.starts_with("mailto:")) {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", &u]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&u).spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&u).spawn();
    }
}

#[tauri::command]
pub fn get_diagnostics() -> String {
    let dir = util::data_dir();
    format!(
        "Riff Rust {}\nOS {} {} {}\nData {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY,
        dir.to_string_lossy()
    )
}

#[tauri::command]
pub fn get_audio_output_info() -> serde_json::Value {
    serde_json::json!({ "name": "Default output", "platform": std::env::consts::OS })
}

// ---------------------------------------------------------------- storage

#[tauri::command]
pub fn storage_get_all() -> serde_json::Value {
    util::read_json_file(&util::storage_path())
}

#[tauri::command]
pub fn storage_save_batch(data: serde_json::Value) -> bool {
    util::write_json_file(&util::storage_path(), &data)
}

#[tauri::command]
pub fn storage_save_sync(data: serde_json::Value) -> bool {
    util::write_json_file(&util::storage_path(), &data)
}

#[tauri::command]
pub fn read_cache_file() -> String {
    fs::read_to_string(util::data_dir().join("web-cache.json")).unwrap_or_default()
}

#[tauri::command]
pub fn write_cache_file(data: String) -> bool {
    let path = util::data_dir().join("web-cache.json");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, data).is_ok()
}

#[tauri::command]
pub fn save_playlists_json(payload: serde_json::Value) -> serde_json::Value {
    let _ = util::write_json_file(&util::playlists_path(), &payload);
    serde_json::json!({ "success": true })
}

#[tauri::command]
pub fn show_playlist_context_menu(_playlist_id: String, _options: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "shown": false })
}

// ----------------------------------------------------------- account sync

#[tauri::command]
pub fn account_export(_payload: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "success": false, "error": "account export is not yet ported in Riff Rust v0.1" })
}

#[tauri::command]
pub fn account_import() -> serde_json::Value {
    serde_json::json!({ "success": false, "error": "account import is not yet ported in Riff Rust v0.1" })
}

// ------------------------------------------------------------- google / yt

#[tauri::command]
pub fn google_login() -> serde_json::Value {
    serde_json::json!({ "success": false, "error": "Google sign-in is not yet ported in Riff Rust v0.1" })
}

#[tauri::command]
pub fn google_logout() {}

#[tauri::command]
pub fn google_auth_status() -> serde_json::Value {
    serde_json::json!({ "authenticated": false })
}

#[tauri::command]
pub fn google_account_info() -> serde_json::Value {
    serde_json::json!({ "authenticated": false })
}

#[tauri::command]
pub fn google_import_library(_opts: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "success": false, "error": "Google sign-in is not yet ported in Riff Rust v0.1" })
}

#[tauri::command]
pub fn youtube_playback_login() -> serde_json::Value {
    serde_json::json!({ "success": false, "error": "YouTube playback login is not yet ported in Riff Rust v0.1" })
}

#[tauri::command]
pub fn youtube_playback_logout() {}

#[tauri::command]
pub fn youtube_playback_status() -> serde_json::Value {
    serde_json::json!({ "loggedIn": false })
}

// ------------------------------------------------------------------ themes

#[tauri::command]
pub fn list_themes() -> Vec<serde_json::Value> {
    let dir = util::data_dir().join("themes");
    let _ = fs::create_dir_all(&dir);
    let state = util::read_json_file(&util::themes_state_path());
    let enabled = state.as_object().cloned().unwrap_or_default();
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e.eq_ignore_ascii_case("css")).unwrap_or(false) {
                let id = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let is_enabled = enabled.get(&id).and_then(|v| v.as_bool()).unwrap_or(false);
                out.push(serde_json::json!({
                    "id": id,
                    "name": id,
                    "file": format!("{}.css", path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()),
                    "enabled": is_enabled,
                    "path": path.to_string_lossy(),
                }));
            }
        }
    }
    out
}

#[tauri::command]
pub fn import_themes(paths: Vec<String>) -> Vec<serde_json::Value> {
    let dir = util::data_dir().join("themes");
    let _ = fs::create_dir_all(&dir);
    let mut out = Vec::new();
    for p in paths {
        let src = PathBuf::from(&p);
        if src.extension().map(|e| e.eq_ignore_ascii_case("css")).unwrap_or(false) && src.is_file() {
            if let Some(name) = src.file_name() {
                let _ = fs::copy(&src, dir.join(name));
                let id = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                out.push(serde_json::json!({ "id": id, "name": id, "enabled": false }));
            }
        }
    }
    out
}

fn set_theme_flag(id: &str, on: bool) {
    let mut state = util::read_json_file(&util::themes_state_path());
    if !state.is_object() {
        state = serde_json::json!({});
    }
    state[id] = serde_json::json!(on);
    let _ = util::write_json_file(&util::themes_state_path(), &state);
}

#[tauri::command]
pub fn set_theme_enabled(id: String, on: bool) {
    set_theme_flag(&id, on);
}

#[tauri::command]
pub fn disable_all_themes() {
    let dir = util::data_dir().join("themes");
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(id) = path.file_stem().map(|s| s.to_string_lossy().to_string()) {
                set_theme_flag(&id, false);
            }
        }
    }
}

#[tauri::command]
pub fn remove_theme(id: String) {
    let _ = fs::remove_file(util::data_dir().join("themes").join(format!("{id}.css")));
    let mut state = util::read_json_file(&util::themes_state_path());
    if let Some(map) = state.as_object_mut() {
        map.remove(&id);
    }
    let _ = util::write_json_file(&util::themes_state_path(), &state);
}

// ----------------------------------------------------------------- plugins
// Plugins can be listed/managed but not executed yet — plugin execution needs
// a sandboxed JS runtime, which lands in a later milestone (see PORTING.md).

#[tauri::command]
pub fn list_plugins() -> Vec<serde_json::Value> {
    let dir = util::data_dir().join("plugins");
    let _ = fs::create_dir_all(&dir);
    let state = util::read_json_file(&util::plugins_state_path());
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e.eq_ignore_ascii_case("js")).unwrap_or(false) {
                let file = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let enabled = state.get(&file).and_then(|v| v.as_bool()).unwrap_or(false);
                let name = file.trim_end_matches(".js").to_string();
                out.push(serde_json::json!({
                    "file": file,
                    "name": name,
                    "enabled": enabled,
                    "path": path.to_string_lossy(),
                }));
            }
        }
    }
    out
}

#[tauri::command]
pub fn import_plugins(paths: Vec<String>) -> usize {
    let dir = util::data_dir().join("plugins");
    let _ = fs::create_dir_all(&dir);
    let mut count = 0;
    for p in paths {
        let src = PathBuf::from(&p);
        if src.extension().map(|e| e.eq_ignore_ascii_case("js")).unwrap_or(false) && src.is_file() {
            if let Some(name) = src.file_name() {
                if fs::copy(&src, dir.join(name)).is_ok() {
                    count += 1;
                }
            }
        }
    }
    count
}

#[tauri::command]
pub fn set_plugin_enabled(file: String, on: bool) {
    let mut state = util::read_json_file(&util::plugins_state_path());
    if !state.is_object() {
        state = serde_json::json!({});
    }
    state[&file] = serde_json::json!(on);
    let _ = util::write_json_file(&util::plugins_state_path(), &state);
}

#[tauri::command]
pub fn disable_all_plugins() {
    let dir = util::data_dir().join("plugins");
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(file) = entry.file_name().to_str() {
                set_plugin_enabled(file.to_string(), false);
            }
        }
    }
}

#[tauri::command]
pub fn remove_plugin(file: String) {
    let safe = file.replace(['\\', '/'], "_");
    let _ = fs::remove_file(util::data_dir().join("plugins").join(safe));
    let mut state = util::read_json_file(&util::plugins_state_path());
    if let Some(map) = state.as_object_mut() {
        map.remove(&file);
    }
    let _ = util::write_json_file(&util::plugins_state_path(), &state);
}

#[tauri::command]
pub fn open_plugins_folder(_app: AppHandle) {
    let dir = util::data_dir().join("plugins");
    let _ = fs::create_dir_all(&dir);
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&dir).spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
}

// ------------------------------------------------------------------- styles

#[tauri::command]
pub fn get_app_css() -> String {
    include_str!("../../src/renderer/styles.css").to_string()
}