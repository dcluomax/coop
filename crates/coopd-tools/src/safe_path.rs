//! Lexical validation shared by descriptor-relative file tools.
//!
//! [`file_read`](crate::file_read) and [`file_write`](crate::file_write) open
//! paths through a capability directory, which enforces confinement during the
//! actual filesystem operation. This module rejects malformed paths before
//! that descriptor-relative resolution.

use coopd_core::{CoreError, Result};
use std::path::{Component, Path};

/// Validate that a user path is non-empty, relative, and never traverses up.
///
/// # Errors
///
/// Returns an error for absolute paths, platform prefixes, or `..` components.
pub fn validate_relative_path(user_path: &str) -> Result<()> {
    let path = Path::new(user_path);
    if user_path.is_empty() || path.is_absolute() {
        return Err(CoreError::Other(format!(
            "absolute or empty paths are not allowed: {user_path}"
        )));
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err(CoreError::Other(format!(
                "path traversal not allowed: {user_path}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_and_current_dir_paths() {
        assert!(validate_relative_path("a.txt").is_ok());
        assert!(validate_relative_path("./nested/a.txt").is_ok());
    }

    #[test]
    fn rejects_empty_absolute_and_parent_paths() {
        assert!(validate_relative_path("").is_err());
        assert!(validate_relative_path("/etc/passwd").is_err());
        assert!(validate_relative_path("../../etc/passwd").is_err());
        assert!(validate_relative_path("sub/../../escape").is_err());
    }
}
