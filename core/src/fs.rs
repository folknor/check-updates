//! Crash-safe manifest replacement, shared by every updater.
//!
//! ccu, pcu and ncu each rewrite a user's manifest in place. A plain
//! `fs::write` truncates the file first and fills it afterwards, so a crash,
//! a signal or a full disk between those two steps leaves an empty or partial
//! `Cargo.toml`/`pyproject.toml`/`package.json` behind. Writing to a sibling
//! temporary file and renaming it over the original makes the swap atomic on
//! every supported platform, because the rename is same-directory.
//!
//! One copy of this lives here on purpose. The three tools used to carry one
//! each, written from the same description; they agreed by luck, and the next
//! edit to one of them would have silently unpicked that.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Why [`write_atomically`] could not replace the file.
#[derive(Debug, Error)]
pub enum AtomicWriteError {
    #[error("failed to write temporary file {path}")]
    Temp {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to replace {path}")]
    Rename {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Replace the contents of `path` with `bytes` through a sibling temporary
/// file and a rename.
///
/// The original file's permission bits are copied onto the temporary file
/// before the rename where they can be read, so a `0o640` manifest stays
/// `0o640`. On any failure the temporary file is removed and the original is
/// left untouched.
///
/// Known behaviour: when `path` is a *symlink*, the rename replaces the link
/// itself with a regular file rather than writing through it. Rare for a
/// manifest, and accepted in exchange for never truncating one.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), AtomicWriteError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("manifest");
    let tmp_path = dir.join(format!(
        ".{file_name}.check-updates-{}.tmp",
        std::process::id()
    ));

    let result = (|| {
        fs::write(&tmp_path, bytes).map_err(|source| AtomicWriteError::Temp {
            path: tmp_path.clone(),
            source,
        })?;

        // Preserve the original file's permissions where we can read them.
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(&tmp_path, meta.permissions());
        }

        fs::rename(&tmp_path, path).map_err(|source| AtomicWriteError::Rename {
            path: path.to_path_buf(),
            source,
        })
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn replaces_contents_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.toml");
        fs::write(&path, "old").unwrap();

        write_atomically(&path, b"new").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n != "manifest.toml")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_failed_rename_cleans_up_and_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        // The target's parent is a regular file, so the rename cannot succeed.
        let blocker = dir.path().join("not-a-dir");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("manifest.toml");

        let err = write_atomically(&path, b"new").unwrap_err();
        assert!(matches!(err, AtomicWriteError::Temp { .. }), "{err}");
        assert_eq!(fs::read_to_string(&blocker).unwrap(), "x");
    }

    #[test]
    #[cfg(unix)]
    fn preserves_permission_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.toml");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        write_atomically(&path, b"new").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "mode was {mode:o}");
    }
}
