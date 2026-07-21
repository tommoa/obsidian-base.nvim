//! Vault-relative path validation and containment checks.

use std::path::{Component, Path, PathBuf};

use crate::error::{Result, WorkerError};

pub fn relative_vault_path(value: &str) -> Result<String> {
    if value.is_empty() || value.contains('\0') {
        return Err(WorkerError::new(
            "invalid_path",
            "path must be a non-empty string",
        ));
    }
    let normalized = value.replace('\\', "/");
    let bytes = normalized.as_bytes();
    if normalized.starts_with('/')
        || (cfg!(windows)
            && bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(1) == Some(&b':'))
    {
        return outside();
    }
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return outside();
                }
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return outside();
    }
    Ok(parts.join("/"))
}

pub fn lexical_contained(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = relative_vault_path(relative)?;
    let path = root.join(relative);
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return outside();
    }
    Ok(path)
}

pub fn canonical_contained(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = lexical_contained(root, relative)?;
    let resolved = path.canonicalize().map_err(WorkerError::io)?;
    // Lexical validation rejects `..`; canonical validation also rejects symlinks escaping root.
    if resolved == root || resolved.starts_with(root) {
        Ok(resolved)
    } else {
        outside()
    }
}

fn outside<T>() -> Result<T> {
    Err(WorkerError::new(
        "path_outside_vault",
        "path must remain within the vault",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_lexically_without_allowing_escape() {
        assert_eq!(relative_vault_path("a/./b/../c.md").unwrap(), "a/c.md");
        assert_eq!(relative_vault_path("a\\b.md").unwrap(), "a/b.md");
        assert_eq!(
            relative_vault_path("../x").unwrap_err().code,
            "path_outside_vault"
        );
        assert_eq!(
            relative_vault_path("/x").unwrap_err().code,
            "path_outside_vault"
        );
        assert_eq!(relative_vault_path("").unwrap_err().code, "invalid_path");
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_drive_prefixes() {
        assert_eq!(
            relative_vault_path("C:outside.md").unwrap_err().code,
            "path_outside_vault"
        );
        assert_eq!(
            relative_vault_path("C:/outside.md").unwrap_err().code,
            "path_outside_vault"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn permits_colons_in_posix_filenames() {
        assert_eq!(relative_vault_path("a:note.md").unwrap(), "a:note.md");
    }
}
