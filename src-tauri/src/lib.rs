mod commands;
mod config;
mod ffmpeg;
mod obs_client;
mod overlay_server;
mod state;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch just shows/focuses the existing window.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            let mut config = config::load(&handle).unwrap_or_default();

            // Tray icon: left-click shows the window; the window's X hides
            // to tray, so this menu is the real way to quit.
            let show_item =
                tauri::menu::MenuItem::with_id(app, "show", "Show ReplayTrim", true, None::<&str>)?;
            let quit_item =
                tauri::menu::MenuItem::with_id(app, "quit", "Quit ReplayTrim", true, None::<&str>)?;
            let tray_menu = tauri::menu::Menu::with_items(app, &[&show_item, &quit_item])?;
            fn show_main(app: &tauri::AppHandle) {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            tauri::tray::TrayIconBuilder::with_id("replaytrim-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("ReplayTrim")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // Enable launch-with-Windows once by default; after that the
            // Settings checkbox is in charge.
            if !config.autostart_configured {
                use tauri_plugin_autostart::ManagerExt;
                let _ = app.autolaunch().enable();
                config.autostart_configured = true;
                let _ = config::save(&handle, &config);
            }

            // Autostarted launches begin hidden in the tray.
            if std::env::args().any(|a| a == "--minimized") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            let clips_file = config::clips_path(&handle)
                .unwrap_or_else(|_| std::path::PathBuf::from("clips.json"));
            let clips = config::load_clips(&clips_file);
            let state = AppState::new(config.clone(), clips, clips_file);
            app.manage(state.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(e) = overlay_server::spawn(handle, state, config.overlay_port).await {
                    eprintln!("failed to start overlay server: {e}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::get_autostart,
            commands::set_autostart,
            commands::connect_obs,
            commands::list_media_sources,
            commands::list_scenes,
            commands::create_obs_source,
            commands::create_obs_overlay,
            commands::ensure_ready,
            commands::check_target_exists,
            commands::instant_replay,
            commands::overlay_command,
            commands::read_file_bytes,
            commands::grab_replay,
            commands::generate_waveform,
            commands::export_trim,
            commands::push_to_obs,
            commands::toggle_source_visible,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
