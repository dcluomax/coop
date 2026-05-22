//! Shared helper: resolve a user-supplied path against a workdir base while
//! preventing escape via `..`, absolute paths, or symlinks pointing outside.
//!
//! Used by `file_read` and `file_write`. Returning [`CoreError::Other`] for
//! every refusal keeps the tool errors uniform; the message includes enough
//! context for the model to retry with a valid in-workdir path.

use coopd_core::{CoreError, Result};
use std::path::{Component, Path, PathBuf};

/// Resolve `user_path` against `base`, refusing absolute paths, parent
/// components (`..`), and any post-canonicalization escape from `base`.
///
/// When `must_exist` is true the full target is canonicalized (and must
/// exist). When false (e.g. `file_write` creating a new file), the target's
/// parent dir is canonicalized — letting writes create new files inside the
/// workdir while still rejecting traversal.
///
/// # Errors
///
/// Returns [`CoreError::Other`] if the path is absolute, contains `..`,
/// canonicalization fails (e.g. parent dir missing when `must_exist=false`),
/// or the resolved real path is not inside the canonicalized `base`.
pub fn safe_resolve(base: &Path, user_path: &str, must_exist: bool) -> Result<PathBuf> {
    let pp = Path::new(user_path);
    if pp.is_absolute() {
        return Err(CoreError::Other(format!(
            "absolute paths are not allowed: {user_path}"
        )));
    }
    for c in pp.components() {
        match c {
            Component::ParentDir => {
                return Err(CoreError::Other(format!(
                    "path traversal (..) not allowed: {user_path}"
                )));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(CoreError::Other(format!(
                    "absolute paths are not allowed: {user_path}"
                )));
            }
            _ => {}
        }
    }

    let base_canon = base
        .canonicalize()
        .map_err(|e| CoreError::Other(format!("workdir canonicalize {}: {e}", base.display())))?;

    let joined = base.join(pp);

    let resolved = if must_exist {
        joined
            .canonicalize()
            .map_err(|e| CoreError::Other(format!("resolve {}: {e}", joined.display())))?
    } else {
        // For file_write: target may not exist, but the parent must, and
        // must lie within base after canonicalization. Symlinks in the
        // parent chain are followed by canonicalize() — if they escape,
        // we reject.
        let parent = joined
            .parent()
            .ok_or_else(|| CoreError::Other(format!("no parent for {}", joined.display())))?;
        let parent_canon = parent
            .canonicalize()
            .map_err(|e| CoreError::Other(format!("resolve parent {}: {e}", parent.display())))?;
        let file_name = joined
            .file_name()
            .ok_or_else(|| CoreError::Other(format!("no file name in {}", joined.display())))?;
        parent_canon.join(file_name)
    };

    if !resolved.starts_with(&base_canon) {
        return Err(CoreError::Other(format!(
            "path escapes workdir: {} (resolved: {})",
            user_path,
            resolved.display()
        )));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rejects_absolute() {
        let d = tempdir().unwrap();
        assert!(safe_resolve(d.path(), "/etc/passwd", true).is_err());
    }

    #[test]
    fn rejects_parent_traversal() {
        let d = tempdir().unwrap();
        assert!(safe_resolve(d.path(), "../../etc/passwd", false).is_err());
        assert!(safe_resolve(d.path(), "sub/../../escape", false).is_err());
    }

    #[test]
    fn allows_subpath_read() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "hi").unwrap();
        let p = safe_resolve(d.path(), "a.txt", true).unwrap();
        assert!(p.ends_with("a.txt"));
    }

    #[test]
    fn allows_new_file_write_in_subdir() {
        let d = tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let p = safe_resolve(d.path(), "sub/new.txt", false).unwrap();
        assert!(p.ends_with("sub/new.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let d = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "leaked").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), d.path().join("link")).unwrap();
        assert!(safe_resolve(d.path(), "link", true).is_err());
    }
}
