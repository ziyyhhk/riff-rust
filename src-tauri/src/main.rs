#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod library;
mod server;
mod state;
mod util;

use std::sync::Arc;
use tauri::{Manager, RunEvent};

use crate::state::AppState;

fn main() {
    util::ensure_dirs();

    // Panics from any thread land in the app log instead of vanishing.
    std::panic::set_hook(Box::new(|info| {
        util::log_line("PANIC", &format!("{info}"));
    }));

    // Persisted custom download folder (config.json)
    let config = util::read_json_file(&util::config_path());
    let download_dir = config.get("downloadDir").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let storage_snapshot = util::read_json_file(&util::storage_path());

    // First run? (tutorial marker mirrors upstream's .rifftutorial)
    let first_run = std::fs::read_to_string(util::data_dir().join(".rifftutorial"))
        .map(|s| s.trim() != env!("CARGO_PKG_VERSION"))
        .unwrap_or(true);
    util::log_line("INFO", &format!("first run: {first_run}"));

    // Embedded localhost API
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind embedded server");
    // Tokio's IOCP/mio registration expects the std listener in nonblocking mode.
    let _ = listener.set_nonblocking(true);
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token = util::random_token();
    let app_state = Arc::new(AppState::new(port, token.clone(), download_dir));
    server::serve(listener, app_state.clone());
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
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App(start_url.into()))
                .title("Riff Rust")
                .inner_size(1280.0, 800.0)
                .min_inner_size(800.0, 540.0)
                .decorations(false)
                .resizable(true)
                .center()
                .background_color(bg)
                .initialization_script(&boot)
                .build()?;

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
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Shared>();
                let tray_on_close = state.tray_on_close.lock().map(|v| *v).unwrap_or(true);
                let quitting = state.quitting.lock().map(|v| *v).unwrap_or(false);
                if tray_on_close && !quitting {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!());

    match result {
        Ok(app) => {
            app.run(|_app_handle, event| {
                if let RunEvent::ExitRequested { .. } = event {
                    // keep it simple: allow exit
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
