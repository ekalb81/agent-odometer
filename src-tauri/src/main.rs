// Prevents additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if odometer_lib::turn_receipts::try_run_cli() {
        return;
    }
    // Read-only reporting subcommands (issue #47). Checked before the
    // desktop app starts, and only claims argv it recognises, so an
    // unrecognised argument still launches the UI as before.
    if odometer_lib::report_cli::try_run_cli() {
        return;
    }
    odometer_lib::run();
}
