pub mod commands;
pub mod config;
pub mod model;
pub mod parser;
pub mod rates;
pub mod scanner;
pub mod store;
pub mod watcher;

use commands::{get_config, get_rates, list_sessions, reveal_in_file_manager, set_config, set_rates};
use store::AppState;
use tracing_subscriber::EnvFilter;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            get_config,
            set_config,
            get_rates,
            set_rates,
            reveal_in_file_manager,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
