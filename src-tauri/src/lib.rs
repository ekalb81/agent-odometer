pub mod claude_parser;
pub mod commands;
pub mod config;
pub mod model;
pub mod parser;
pub mod rates;
pub mod scan_cache;
pub mod scanner;
pub mod session_index;
pub mod store;
pub mod watcher;

use commands::{
    get_bundled_rates, get_config, get_rates, get_scan_status, get_session_details, list_sessions,
    open_task_in_chatgpt, reveal_in_file_manager, sessions_in_range, set_config, set_rates,
};
use config::Config;
use std::sync::Arc;
use store::AppState;
use tracing_subscriber::EnvFilter;

pub fn run() {
    // Init tracing once. A second call would panic, so guard against hot-reload.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let state: Arc<AppState> = Arc::new(AppState::new());
    let state_for_setup = state.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            get_session_details,
            sessions_in_range,
            get_scan_status,
            get_config,
            set_config,
            get_rates,
            get_bundled_rates,
            set_rates,
            reveal_in_file_manager,
            open_task_in_chatgpt,
        ])
        .setup(move |app| {
            let config = Config::load().unwrap_or_else(|e| {
                tracing::warn!("failed to load config: {}; using defaults", e);
                Config::default()
            });

            // Start the live watcher first so changes made during the initial
            // scan are not missed; store the handle so set_config can restart it.
            let handle = watcher::start(
                app.handle().clone(),
                state_for_setup.clone(),
                config.session_roots.clone(),
                config.archive_roots.clone(),
                config.claude_session_roots.clone(),
                config.session_index_path.clone(),
            )?;
            *state_for_setup.watcher.lock().unwrap() = Some(handle);

            // Bulk-load existing sessions on a background thread, emitting a
            // summary per parsed file. Keeping this out of setup means the
            // window is interactive immediately instead of after ~10s of
            // parsing on a large corpus.
            commands::spawn_scan(app.handle().clone(), state_for_setup.clone(), config);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
