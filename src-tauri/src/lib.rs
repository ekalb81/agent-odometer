pub mod claude_parser;
pub mod commands;
pub mod config;
pub mod config_events;
pub mod correlation;
pub mod git_outcomes;
pub mod model;
pub mod parser;
pub mod performance;
pub mod rates;
pub mod scan_cache;
pub mod scanner;
pub mod session_index;
pub mod store;
pub mod telemetry;
pub mod tray;
pub mod watcher;

use commands::{
    add_defender_exclusions, correlate_events, export_performance_data, get_bundled_rates,
    get_config, get_performance_status, get_rates, get_scan_status, get_session_details,
    list_external_events, list_sessions, open_task_in_chatgpt, record_frontend_performance,
    reveal_in_file_manager, scan_git_outcomes, sessions_in_ranges, set_config, set_rates,
    set_tray_totals, write_export,
};
use config::Config;
use std::sync::Arc;
use std::time::Instant;
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            get_session_details,
            sessions_in_ranges,
            get_scan_status,
            get_config,
            set_config,
            get_rates,
            get_bundled_rates,
            set_rates,
            reveal_in_file_manager,
            open_task_in_chatgpt,
            add_defender_exclusions,
            write_export,
            list_external_events,
            correlate_events,
            scan_git_outcomes,
            set_tray_totals,
            get_performance_status,
            record_frontend_performance,
            export_performance_data,
        ])
        .setup(move |app| {
            let setup_started = Instant::now();
            let config_started = Instant::now();
            let config_result = Config::load();
            let config_loaded = config_result.is_ok();
            let config = config_result.unwrap_or_else(|e| {
                tracing::warn!("failed to load config: {}; using defaults", e);
                Config::default()
            });
            state_for_setup.performance.configure(
                config.performance_tracking_enabled,
                config.performance_log_max_mb,
            );
            state_for_setup.performance.record_backend(
                "startup.config_load",
                config_started,
                config_loaded,
                Default::default(),
            );

            // Start the live watcher first so changes made during the initial
            // scan are not missed; store the handle so set_config can restart it.
            let watcher_started = Instant::now();
            let handle_result = watcher::start(
                app.handle().clone(),
                state_for_setup.clone(),
                config.session_roots.clone(),
                config.archive_roots.clone(),
                config.claude_session_roots.clone(),
                config.session_index_path.clone(),
            );
            state_for_setup.performance.record_backend(
                "startup.session_watcher",
                watcher_started,
                handle_result.is_ok(),
                Default::default(),
            );
            let handle = handle_result?;
            *state_for_setup.watcher.lock().unwrap() = Some(handle);

            let config_watcher_started = Instant::now();
            match config_events::start(app.handle().clone(), state_for_setup.clone()) {
                Ok(handle) => {
                    *state_for_setup.config_watcher.lock().unwrap() = Some(handle);
                    state_for_setup.performance.record_backend(
                        "startup.config_watcher",
                        config_watcher_started,
                        true,
                        Default::default(),
                    );
                }
                Err(error) => {
                    state_for_setup.performance.record_backend(
                        "startup.config_watcher",
                        config_watcher_started,
                        false,
                        Default::default(),
                    );
                    tracing::warn!("config watcher unavailable: {}", error);
                }
            }

            let tray_started = Instant::now();
            if let Err(error) = tray::start(app.handle(), &state_for_setup) {
                state_for_setup.performance.record_backend(
                    "startup.system_tray",
                    tray_started,
                    false,
                    Default::default(),
                );
                tracing::warn!("system tray unavailable: {}", error);
            } else {
                state_for_setup.performance.record_backend(
                    "startup.system_tray",
                    tray_started,
                    true,
                    Default::default(),
                );
            }

            // Bulk-load existing sessions on a background thread, emitting a
            // summary per parsed file. Keeping this out of setup means the
            // window is interactive immediately instead of after ~10s of
            // parsing on a large corpus.
            commands::spawn_scan(app.handle().clone(), state_for_setup.clone(), config);
            state_for_setup.performance.record_backend(
                "startup.setup",
                setup_started,
                true,
                Default::default(),
            );

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Arc<AppState>>();
                if state
                    .tray_available
                    .load(std::sync::atomic::Ordering::Acquire)
                    && window.hide().is_ok()
                {
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
