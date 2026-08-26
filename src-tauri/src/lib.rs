mod commands;
mod config;
mod ffmpeg;
mod obs_client;
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
            app.manage(AppState::new(config));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::connect_obs,
            commands::list_media_sources,
            commands::read_file_bytes,
            commands::grab_replay,
            commands::generate_waveform,
            commands::export_trim,
            commands::push_to_obs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
