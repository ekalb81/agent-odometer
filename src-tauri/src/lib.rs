pub mod commands;
pub mod config;
pub mod model;
pub mod parser;
pub mod rates;
pub mod scanner;
pub mod session_index;
pub mod store;
pub mod watcher;

use commands::{
    get_bundled_rates, get_config, get_rates, list_sessions, open_task_in_chatgpt,
    reveal_in_file_manager, set_config, set_rates,
};
use config::Config;
use std::sync::atomic::Ordering;
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
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_sessions,
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

            // Bulk-load existing sessions before the watcher starts.
            let found = scanner::initial_scan(&config.session_roots, &config.archive_roots);
            for (id, session) in found {
                state_for_setup.sessions.insert(id, session);
            }

            // Overlay thread names from the session index, if present.
            let names = session_index::read(&config.session_index_path);
            session_index::apply(&state_for_setup.sessions, &names);

            state_for_setup.scanned.store(true, Ordering::Release);

            tracing::info!(
                "initial scan complete: {} sessions loaded, {} thread names from index",
                state_for_setup.sessions.len(),
                names.len()
            );

            // Start the live watcher and store the handle in state so set_config can restart it.
            let handle = watcher::start(
                app.handle().clone(),
                state_for_setup.clone(),
                config.session_roots.clone(),
                config.archive_roots.clone(),
                config.session_index_path.clone(),
            )?;
            *state_for_setup.watcher.lock().unwrap() = Some(handle);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
