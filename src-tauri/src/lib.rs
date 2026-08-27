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
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            let config = config::load(&handle).unwrap_or_default();
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
