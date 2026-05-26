/// Scans the configured session root directories and returns paths to all JSONL files.
/// Phase 3 will implement the full directory walk.
pub fn scan_session_roots(
    _roots: &[std::path::PathBuf],
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    unimplemented!("Phase 3")
}
