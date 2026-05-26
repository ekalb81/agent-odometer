pub mod commands;
pub mod config;
pub mod model;
pub mod parser;
pub mod rates;
pub mod scanner;
pub mod store;
pub mod watcher;

use commands::{get_config, get_rates, list_sessions, reveal_in_file_manager, set_config, set_rates};
use config::Config;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use store::AppState;
use tauri::Manager;
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
            set_rates,
            reveal_in_file_manager,
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
            state_for_setup.scanned.store(true, Ordering::Release);

            tracing::info!(
                "initial scan complete: {} sessions loaded",
                state_for_setup.sessions.len()
            );

            // Start the live watcher and keep the handle alive for the app's lifetime.
            let handle = watcher::start(
                app.handle().clone(),
                state_for_setup.clone(),
                config.session_roots.clone(),
                config.archive_roots.clone(),
            )?;
            app.manage(handle);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
