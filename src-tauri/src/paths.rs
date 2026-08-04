//! Path normalization shared by every surface that decides whether two paths
//! name the same file.
//!
//! Windows hands the same file to us under several spellings: `notify` may
//! deliver a verbatim (`\\?\`) prefix when long-path support is active while
//! configuration carries the plain form, separators mix when a slash literal
//! is joined onto a backslash home directory, and case is insignificant.
//! Callers that compare or key on paths must agree on all of that, so the
//! normalization lives here rather than being open-coded per module — it was
//! previously written out six times, and a UNC correction reached only one of
//! them.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Strips a Windows verbatim prefix while preserving the root it names.
///
/// `\\?\UNC\server\share` is the verbatim spelling of `\\server\share`, so
/// stripping the prefix generically leaves `UNC\server\share` — which no
/// longer names a share and, worse, parses as a *relative* path. The UNC form
/// therefore converts back to its plain spelling instead.
///
/// Non-Windows paths and paths without a verbatim prefix are returned
/// borrowed; only the UNC conversion allocates.
pub(crate) fn strip_verbatim_prefix(path: &Path) -> Cow<'_, Path> {
    if let Some(text) = path.to_str() {
        if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
            return Cow::Owned(PathBuf::from(format!(r"\\{stripped}")));
        }
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return Cow::Borrowed(Path::new(stripped));
        }
    }
    Cow::Borrowed(path)
}

/// A case- and separator-insensitive identity key for a path.
///
/// `trim_trailing` exists because the instruction inventory keys directories,
/// where a trailing separator is a spelling difference, while the session
/// stores key files, where it never occurs. The two behaviours are kept
/// distinct rather than unified: the session key is persisted in the history
/// store, so silently changing it would orphan existing rows.
pub(crate) fn normalized_path_key(path: &Path, trim_trailing: bool) -> String {
    let stripped = strip_verbatim_prefix(path);
    let value = stripped.to_string_lossy();
    let mut normalized = value.replace('\\', "/");
    if trim_trailing {
        normalized = normalized.trim_end_matches('/').to_owned();
    }
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::{normalized_path_key, strip_verbatim_prefix};
    use std::path::Path;

    #[test]
    fn verbatim_prefix_normalization_preserves_disk_and_unc_roots() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\sessions\thread.jsonl")).as_ref(),
            Path::new(r"C:\sessions\thread.jsonl")
        );
        // The regression this helper exists for: a generic strip would yield
        // `UNC\server\share\...`, which names nothing and reads as relative.
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\sessions\thread.jsonl"))
                .as_ref(),
            Path::new(r"\\server\share\sessions\thread.jsonl")
        );
    }

    #[test]
    fn paths_without_a_verbatim_prefix_are_returned_unchanged() {
        for path in [
            r"C:\sessions\thread.jsonl",
            r"\\server\share\thread.jsonl",
            "/home/dev/sessions/thread.jsonl",
        ] {
            assert_eq!(
                strip_verbatim_prefix(Path::new(path)).as_ref(),
                Path::new(path)
            );
        }
    }

    /// The verbatim and plain spellings of one UNC location must key alike,
    /// or the same file is tracked as two.
    #[test]
    fn verbatim_and_plain_unc_spellings_share_one_key() {
        assert_eq!(
            normalized_path_key(Path::new(r"\\?\UNC\server\share\a\b.jsonl"), false),
            normalized_path_key(Path::new(r"\\server\share\a\b.jsonl"), false)
        );
    }

    #[test]
    fn keys_normalize_separators_and_optionally_trailing_separators() {
        assert_eq!(
            normalized_path_key(Path::new(r"C:\a\b"), false),
            normalized_path_key(Path::new("C:/a/b"), false)
        );
        assert_eq!(normalized_path_key(Path::new("C:/a/b/"), true), {
            let expected = "C:/a/b";
            if cfg!(windows) {
                expected.to_ascii_lowercase()
            } else {
                expected.to_string()
            }
        });
        // Without trimming, the trailing separator is preserved — the session
        // stores rely on this key staying stable across releases.
        assert!(normalized_path_key(Path::new("C:/a/b/"), false).ends_with('/'));
    }
}
