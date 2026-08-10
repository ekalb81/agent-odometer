pub mod claude_parser;
pub mod commands;
pub mod config;
pub mod config_events;
pub mod correlation;
pub mod diagnostics;
pub mod gemini_parser;
pub mod git_outcomes;
pub mod harness_integration;
pub mod history_store;
pub mod instructions;
pub mod memory;
pub mod model;
pub mod parser;
pub mod paths;
pub mod performance;
pub mod project_identity;
pub mod provider;
pub mod quota;
pub mod quota_store;
pub mod rates;
pub mod scan_cache;
pub mod scanner;
pub mod session_index;
pub mod store;
pub mod telemetry;
pub mod tool_impact;
pub mod tray;
pub mod turn_receipts;
pub mod watcher;

use commands::{
    add_defender_exclusions, cancel_instruction_scan, check_quota_alerts,
    clear_session_project_override, compare_tool_impact, correlate_events, export_performance_data,
    get_bundled_rates, get_config, get_history_status, get_performance_status,
    get_provider_diagnostics, get_quota_config, get_quota_snapshots, get_rates, get_scan_status,
    get_session_details, get_subscription_usage, get_turn_receipt_status, list_external_events,
    list_instruction_files, list_providers, list_sessions, list_tool_impact_targets,
    merge_projects, open_instruction_file, open_task_in_chatgpt, read_instruction_file,
    reassign_session_project, record_frontend_performance, repair_turn_receipt_integrations,
    resolve_projects, resolve_working_directories, reveal_in_file_manager, scan_git_outcomes,
    sessions_in_ranges, set_config, set_project_alias, set_quota_config, set_rates,
    set_tray_totals, unmerge_project, write_export,
};
use config::Config;
use std::sync::Arc;
use std::time::Instant;
use store::AppState;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

/// This crate's sole global allocator (see `memory.rs`'s doc comment for why
/// there can only be one, and why this replaces the `#[cfg(test)]`-only
/// counting allocator earlier probes used to define locally). Behavior is
/// identical to the default system allocator unless heap tracking is
/// explicitly enabled via `memory::configure_heap_tracking` — off by
/// default, matching every other opt-in in this app.
///
/// Do not add another `#[global_allocator]` anywhere in this crate or its
/// tests: a binary may declare at most one, and a second one — even a
/// `#[cfg(test)]`-only one, which is exactly how this crate had two before
/// this instrumentation unified them — fails the build with a "duplicate
/// lang item" error that does not point back at this line. If a probe needs
/// exact heap-byte deltas, drive this allocator via
/// `memory::configure_heap_tracking` and its raw accessors instead.
#[global_allocator]
static GLOBAL_ALLOCATOR: memory::TrackingAllocator = memory::TrackingAllocator;

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
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            get_session_details,
            get_subscription_usage,
            sessions_in_ranges,
            list_tool_impact_targets,
            compare_tool_impact,
            get_scan_status,
            get_config,
            list_providers,
            resolve_working_directories,
            resolve_projects,
            set_project_alias,
            merge_projects,
            unmerge_project,
            reassign_session_project,
            clear_session_project_override,
            set_config,
            get_turn_receipt_status,
            repair_turn_receipt_integrations,
            get_rates,
            get_bundled_rates,
            set_rates,
            reveal_in_file_manager,
            open_task_in_chatgpt,
            add_defender_exclusions,
            write_export,
            list_external_events,
            list_instruction_files,
            cancel_instruction_scan,
            read_instruction_file,
            open_instruction_file,
            correlate_events,
            scan_git_outcomes,
            set_tray_totals,
            get_performance_status,
            record_frontend_performance,
            export_performance_data,
            get_provider_diagnostics,
            get_quota_snapshots,
            get_quota_config,
            set_quota_config,
            check_quota_alerts,
            get_history_status,
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
            memory::configure_heap_tracking(config.memory_heap_tracking_enabled);
            state_for_setup.performance.record_backend(
                "startup.config_load",
                config_started,
                config_loaded,
                Default::default(),
            );

            // Opens (and migrates, if needed) the durable history archive on
            // a background thread (#116). AppState starts `Pending`, so a
            // window can appear before a chained schema migration on an
            // existing install completes; `spawn_scan` and the ledger-backed
            // IPC commands wait on or check that readiness instead of
            // treating `Pending` like a permanently unavailable archive.
            // Dispatched after `performance.configure` above so every step's
            // instrumentation is captured even on a very fast (near-instant)
            // open.
            commands::spawn_history_open(app.handle().clone(), state_for_setup.clone());

            // Start the live watcher first so changes made during the initial
            // scan are not missed; store the handle so set_config can restart it.
            let (provider_sources, source_configuration_valid) = match config.provider_sources() {
                Ok(sources) => (sources, true),
                Err(error) => {
                    // Legacy configs could express overlapping ownership.
                    // Keep Settings reachable while declining all source
                    // access until the user saves a valid configuration.
                    tracing::error!(
                        "session source configuration is invalid; scanning is disabled: {}",
                        error
                    );
                    (provider::ProviderSourceSet::empty(), false)
                }
            };
            let watcher_started = Instant::now();
            let handle_result = watcher::start(
                app.handle().clone(),
                state_for_setup.clone(),
                provider_sources.clone(),
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
            match config_events::start(app.handle().clone(), state_for_setup.clone(), &config) {
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
            commands::spawn_scan(
                app.handle().clone(),
                state_for_setup.clone(),
                config,
                provider_sources,
                source_configuration_valid,
            );
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
