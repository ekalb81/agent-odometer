/// Starts a debounced file-system watcher on the given roots and emits Tauri events
/// when session files are created, modified, or removed.
/// Phase 3 will implement this using notify + notify-debouncer-full.
pub fn start_watcher(
    _roots: &[std::path::PathBuf],
    _app_handle: tauri::AppHandle,
) -> anyhow::Result<()> {
    unimplemented!("Phase 3")
}
