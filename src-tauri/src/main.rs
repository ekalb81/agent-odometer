// Prevents additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if odometer_lib::turn_receipts::try_run_cli() {
        return;
    }
    odometer_lib::run();
}
