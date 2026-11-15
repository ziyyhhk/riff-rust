#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod discord_rpc;
mod discovery;
mod lyrics;
mod ytm_player;
mod library;
mod server;
mod state;
mod tools;
mod util;
mod ytm;

use std::sync::Arc;
use std::time::Duration;
use serde_json::json;
use tauri::webview::PageLoadEvent;
use tauri::{Emitter, Manager, RunEvent};

use crate::state::AppState;

/// Logical work area of a display (taskbar excluded), matching Electron's
/// `display.workArea` in DIP.
#[cfg(target_os = "windows")]
fn monitor_work_area(monitor: &tauri::Monitor) -> Option<(f64, f64, f64, f64)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    let scale = monitor.scale_factor().max(0.5);
    let point = POINT {
        x: monitor.position().x,
        y: monitor.position().y,
    };
    unsafe {
        let handle = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        if handle.is_null() {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(handle, &mut info) == 0 {
            return None;
        }
        let area = info.rcWork;
        Some((
            area.left as f64 / scale,
            area.top as f64 / scale,
            (area.right - area.left) as f64 / scale,
            (area.bottom - area.top) as f64 / scale,
        ))
    }
}

#[cfg(not(target_os = "windows"))]
fn monitor_work_area(monitor: &tauri::Monitor) -> Option<(f64, f64, f64, f64)> {
    let scale = monitor.scale_factor().max(0.5);
    let position = monitor.position();
    let size = monitor.size();
    Some((
        position.x as f64 / scale,
        position.y as f64 / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    ))
}

fn rect_overlap(area: (f64, f64, f64, f64), x: f64, y: f64, w: f64, h: f64) -> f64 {
    let x_overlap = (area.0 + area.2).min(x + w) - area.0.max(x);
    let y_overlap = (area.1 + area.3).min(y + h) - area.1.max(y);
    x_overlap.max(0.0) * y_overlap.max(0.0)
}

/// Match upstream's responsive initial window: 82% x 86% of the primary work
/// area, offset by 9% / 7%, with saved geometry restored and clamped to the
/// display that contains it (Electron's `screen.getDisplayMatching`).
fn startup_geometry(app: &tauri::App) -> (f64, f64, f64, f64, bool) {
    let saved = util::read_json_file(&util::window_state_path());
    let bounds = saved.get("bounds");
    let read = |key: &str| {
        bounds
            .and_then(|value| value.get(key))
            .and_then(|value| value.as_f64())
    };

    let primary_area = app
        .primary_monitor()
        .ok()
        .flatten()
        .as_ref()
        .and_then(monitor_work_area)
        .unwrap_or((0.0, 0.0, 1280.0, 800.0));

    // Choose the display with the greatest overlap, like getDisplayMatching.
    let target_area = match (read("x"), read("y"), read("width"), read("height")) {
        (Some(x), Some(y), Some(w), Some(h)) => app
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .filter_map(|monitor| {
                monitor_work_area(monitor)
                    .map(|area| (area, rect_overlap(area, x, y, w, h)))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(area, _)| area)
            .unwrap_or(primary_area),
        _ => primary_area,
    };

    let mut width = read("width").unwrap_or((primary_area.2 * 0.82).round());
    let mut height = read("height").unwrap_or((primary_area.3 * 0.86).round());
    let mut x = read("x").unwrap_or((primary_area.0 + primary_area.2 * 0.09).round());
    let mut y = read("y").unwrap_or((primary_area.1 + primary_area.3 * 0.07).round());

    // Upstream clamps size to the primary work area and position to the
    // matched display's work area.
    width = width.max(800.0).min(primary_area.2.max(800.0));
    height = height.max(540.0).min(primary_area.3.max(540.0));
    x = x
        .max(target_area.0)
        .min(target_area.0 + (target_area.2 - width).max(0.0));
    y = y
        .max(target_area.1)
        .min(target_area.1 + (target_area.3 - height).max(0.0));

    let maximized = saved
        .get("isMaximized")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    // Upstream persists isFullScreen but only restores maximized state on launch.
    (width, height, x, y, maximized)
}

fn current_window_bounds(window: &tauri::WebviewWindow) -> Option<serde_json::Value> {
    let scale = window.scale_factor().unwrap_or(1.0).max(0.5);
    let position = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    Some(json!({
        "x": (position.x as f64 / scale).round(),
        "y": (position.y as f64 / scale).round(),
        "width": (size.width as f64 / scale).round(),
        "height": (size.height as f64 / scale).round(),
    }))
}

/// Persist geometry in upstream's format. Writes are throttled during drag/resize;
/// `force` is used on close/exit so the final position always lands on disk.
fn persist_window_state(window: &tauri::WebviewWindow, force: bool) {
    let state = window.state::<Shared>();
    if state.mini.lock().map(|mini| mini.is_some()).unwrap_or(false) {
        return;
    }
    {
        let mut last = state
            .last_window_save
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !force && last.elapsed() < Duration::from_millis(400) {
            return;
        }
        *last = std::time::Instant::now();
    }

    let minimized = window.is_minimized().unwrap_or(false);
    let is_maximized = window.is_maximized().unwrap_or(false);
    let is_fullscreen = window.is_fullscreen().unwrap_or(false);
    // Reading the previous file is only needed when the live bounds are not the
    // normal window rect (maximized/fullscreen/minimized). Avoid the read on
    // every drag/resize tick.
    let bounds = if is_maximized || is_fullscreen || minimized {
        util::read_json_file(&util::window_state_path())
            .get("bounds")
            .cloned()
            .or_else(|| current_window_bounds(window))
    } else {
        current_window_bounds(window)
    };
    let Some(bounds) = bounds else {
        return;
    };
    let _ = util::write_json_file(
        &util::window_state_path(),
        &json!({
            "bounds": bounds,
            "isMaximized": is_maximized,
            "isFullScreen": is_fullscreen,
        }),
    );
}

fn main() {
    util::ensure_dirs();
    let cache_freed = util::prune_cache_limit();
    if cache_freed > 0 {
        util::log_line(
            "INFO",
            &format!("cache limit pruned {} bytes from tracks/", cache_freed),
        );
    }

    // Panics from any thread land in the app log instead of vanishing.
    std::panic::set_hook(Box::new(|info| {
        util::log_line("PANIC", &format!("{info}"));
    }));

    // Persisted custom download folder (config.json)
    let config = util::read_json_file(&util::config_path());
    let download_dir = config.get("downloadDir").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let storage_snapshot = util::read_json_file(&util::storage_path());

    // First run? (tutorial marker mirrors upstream's .rifftutorial; existence-only,
    // so app updates do not replay onboarding)
    let first_run = !util::data_dir().join(".rifftutorial").exists();
    util::log_line("INFO", &format!("first run: {first_run}"));

    // Embedded localhost API
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind embedded server");
    // Tokio's IOCP/mio registration expects the std listener in nonblocking mode.
    let _ = listener.set_nonblocking(true);
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token = util::random_token();
    let app_state = Arc::new(AppState::new(port, token.clone(), download_dir));
    server::serve(listener, app_state.clone());
    tools::ensure_tools(app_state.clone());
    util::log_line("INFO", &format!("embedded API listening on 127.0.0.1:{port}"));

    // Synchronous boot data for the renderer (read before any IPC round-trip)
    let boot = format!(
        "window.__RIFF_BOOT__ = {{ token: \"{}\", port: {}, storage: {} }};",
        token,
        port,
        serde_json::to_string(&storage_snapshot).unwrap_or_else(|_| "{}".into())
    );

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    commands::handle_shortcut(app, shortcut, event)
                })
                .build(),
        )
        .manage(app_state.clone())
        .invoke_handler(tauri::generate_handler![
            commands::get_server_port,
            commands::window_minimize,
            commands::window_maximize,
            commands::window_close,
            commands::set_window_buttons_visible,
            commands::toggle_mini_window,
            commands::set_mini_lyrics,
            commands::set_zoom_factor,
            commands::get_zoom_factor,
            commands::clear_web_frame_cache,
            commands::restart_app,
            commands::set_app_proxy,
            commands::get_gpu_compositing,
            commands::import_audio_files,
            commands::rescan_local_metadata,
            commands::discord_rpc_update,
            commands::discord_rpc_clear,
            commands::discord_rpc_set_enabled,
            commands::discord_rpc_get_enabled,
            commands::discord_rpc_get_connected,
            commands::discord_rpc_reconnect,
            commands::get_cache_size,
            commands::clear_cache,
            commands::get_data_dir,
            commands::set_data_dir,
            commands::get_download_dir,
            commands::set_download_dir,
            commands::get_app_version,
            commands::check_for_updates,
            commands::update_action,
            commands::install_update_now,
            commands::set_tray_on_close,
            commands::get_multi_instance,
            commands::set_multi_instance,
            commands::get_global_shortcuts,
            commands::set_global_shortcuts,
            commands::set_media_keys,
            commands::media_keys_active,
            commands::welcome_finish,
            commands::take_welcome_result,
            commands::get_launcher_info,
            commands::install_launcher,
            commands::log_error,
            commands::open_logs_folder,
            commands::open_external,
            commands::get_diagnostics,
            commands::get_audio_output_info,
            commands::storage_get_all,
            commands::storage_save_batch,
            commands::storage_save_sync,
            commands::read_cache_file,
            commands::write_cache_file,
            commands::save_playlists_json,
            commands::show_playlist_context_menu,
            commands::account_export,
            commands::account_import,
            commands::google_login,
            commands::google_logout,
            commands::google_auth_status,
            commands::google_account_info,
            commands::google_import_library,
            commands::youtube_playback_login,
            commands::youtube_playback_logout,
            commands::youtube_playback_status,
            commands::list_themes,
            commands::import_themes,
            commands::set_theme_enabled,
            commands::disable_all_themes,
            commands::remove_theme,
            commands::list_plugins,
            commands::import_plugins,
            commands::set_plugin_enabled,
            commands::disable_all_plugins,
            commands::remove_plugin,
            commands::open_plugins_folder,
            commands::get_app_css,
        ])
        .setup(move |app| {
            let start_url = if first_run { "welcome.html" } else { "index.html" };
            let bg = if first_run { tauri::window::Color(141, 94, 242, 255) } else { tauri::window::Color(0, 0, 0, 255) };
            let (win_w, win_h, win_x, win_y, win_maximized) = startup_geometry(app);
            let window = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App(start_url.into()))
                .title("Riff Rust")
                .inner_size(win_w, win_h)
                .position(win_x, win_y)
                .min_inner_size(800.0, 540.0)
                .decorations(false)
                .resizable(true)
                .visible(false)
                .maximized(win_maximized)
                .background_color(bg)
                .initialization_script(&boot)
                .on_page_load(|window, payload| {
                    if payload.event() == PageLoadEvent::Finished {
                        let _ = window.show();
                    }
                })
                .build()?;

            // If the page never finishes loading, do not leave an invisible app running.
            let fallback = window.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if fallback.is_visible().map(|visible| !visible).unwrap_or(false) {
                    let _ = fallback.show();
                }
            });

            // Tray icon + menu (Show / Quit)
            {
                let icon = tauri::image::Image::new_owned(include_bytes!("../icons/tray-32x32.rgba").to_vec(), 32, 32);
                let show_i = tauri::menu::MenuItem::with_id(app, "show", "Show Riff Rust", true, None::<&str>)?;
                let quit_i = tauri::menu::MenuItem::with_id(app, "quit", "Quit Riff Rust", true, None::<&str>)?;
                let menu = tauri::menu::Menu::with_items(app, &[&show_i, &quit_i])?;
                let _ = tauri::tray::TrayIconBuilder::new()
                    .icon(icon)
                    .tooltip("Riff Rust")
                    .menu(&menu)
                    .on_menu_event(|app, event| {
                        match event.id().as_ref() {
                            "show" => {
                                if let Some(window) = app.get_webview_window("main") {
                                    let _ = window.show();
                                    let _ = window.set_focus();
                                    let state = app.state::<Shared>();
                                    if state.set_window_visible(true) {
                                        let _ = window.emit("window-visibility", true);
                                    }
                                }
                            }
                            "quit" => {
                                let state = app.state::<Shared>();
                                if let Ok(mut quitting) = state.quitting.lock() {
                                    *quitting = true;
                                }
                                app.exit(0);
                            }
                            _ => {}
                        }
                    })
                    .build(app);
            }

            // Register media keys / custom global shortcuts (renderer confirms audio readiness later).
            let shared = app.state::<Shared>();
            commands::sync_hotkeys(app.handle(), &shared);
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Resized(_) => {
                let state = window.state::<Shared>();
                let visible = !window.is_minimized().unwrap_or(false);
                if state.set_window_visible(visible) {
                    let _ = window.emit("window-visibility", visible);
                }
                if let Some(webview) = window.app_handle().get_webview_window("main") {
                    persist_window_state(&webview, false);
                }
            }
            tauri::WindowEvent::Moved(_) => {
                if let Some(webview) = window.app_handle().get_webview_window("main") {
                    persist_window_state(&webview, false);
                }
            }
            tauri::WindowEvent::CloseRequested { api, .. } => {
                let state = window.state::<Shared>();
                if let Some(webview) = window.app_handle().get_webview_window("main") {
                    persist_window_state(&webview, true);
                }
                let tray_on_close = state.tray_on_close.lock().map(|v| *v).unwrap_or(true);
                let quitting = state.quitting.lock().map(|v| *v).unwrap_or(false);
                if tray_on_close && !quitting {
                    api.prevent_close();
                    let _ = window.hide();
                    if state.set_window_visible(false) {
                        let _ = window.emit("window-visibility", false);
                    }
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!());

    match result {
        Ok(app) => {
            app.run(|app_handle, event| {
                if let RunEvent::ExitRequested { .. } = event {
                    if let Some(window) = app_handle.get_webview_window("main") {
                        persist_window_state(&window, true);
                    }
                }
            });
        }
        Err(e) => {
            eprintln!("Riff Rust failed to start: {e}");
            std::process::exit(1);
        }
    }
}

type Shared = Arc<AppState>;
