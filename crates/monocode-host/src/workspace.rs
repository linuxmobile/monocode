//! Confining remote file access to a registered project.
//!
//! A paired desktop may read and write files on this machine, so every path it
//! sends is checked against the project it named. Two properties matter and
//! both are easy to get wrong:
//!
//! - A path must not escape the project root. `..` is resolved before the
//!   comparison, so `proj/../../etc/passwd` is rejected rather than merely
//!   looking odd.
//! - A path must not reach `.git` at any depth. Git's own config and hooks can
//!   run commands, so writing there would turn a file write into code
//!   execution.
//!
//! The checks are repeated after resolving symlinks, because a link inside the
//! project can point outside it.

use std::path::{Component, Path, PathBuf};

/// Why a path was refused. Distinct variants so a caller can report precisely
/// what was wrong instead of a generic failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// Not a string, too long, or contains a NUL byte.
    Malformed,
    /// Resolves outside the project root.
    Outside,
    /// Refers to the root itself where a child was required.
    IsRoot,
    /// Passes through a `.git` directory.
    Git,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "Invalid workspace path",
            Self::Outside | Self::IsRoot => "Path is outside the workspace",
            Self::Git => "Path is outside the workspace",
        })
    }
}

/// The longest path a remote client may send.
const MAX_PATH: usize = 4096;

/// Resolves `input` against `root` without touching the filesystem.
///
/// The result is lexically normalised, so a `..` that would leave the root is
/// detected even when the intermediate directories do not exist.
pub fn resolve(root: &Path, input: &str, allow_root: bool) -> Result<PathBuf, PathError> {
    if input.len() > MAX_PATH || input.contains('\0') {
        return Err(PathError::Malformed);
    }
    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };
    let normalised = normalise(&candidate);
    let root = normalise(root);
    if !within(&root, &normalised) {
        return Err(PathError::Outside);
    }
    if !allow_root && normalised == root {
        return Err(PathError::IsRoot);
    }
    if touches_git(&root, &normalised) {
        return Err(PathError::Git);
    }
    Ok(normalised)
}

/// Resolves `input` and then follows symlinks, re-checking the real location.
///
/// A missing final component is allowed: creating a file is a normal operation
/// and its parent is what must exist and be inside the project.
pub fn existing(root: &Path, input: &str, allow_root: bool) -> Result<PathBuf, PathError> {
    let path = resolve(root, input, allow_root)?;
    let actual = match std::fs::canonicalize(&path) {
        Ok(actual) => actual,
        // The leaf may not exist yet; check the parent instead.
        Err(_) => {
            let parent = path.parent().ok_or(PathError::Outside)?;
            let real_parent = std::fs::canonicalize(parent).map_err(|_| PathError::Outside)?;
            real_parent.join(path.file_name().ok_or(PathError::Outside)?)
        }
    };
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| normalise(root));
    if !within(&root, &actual) {
        return Err(PathError::Outside);
    }
    if !allow_root && actual == root {
        return Err(PathError::IsRoot);
    }
    if touches_git(&root, &actual) {
        return Err(PathError::Git);
    }
    Ok(actual)
}

/// Resolves `..` and `.` without requiring the path to exist.
fn normalise(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping a normal component cancels it; popping a root or a
                // prefix would climb above the filesystem, so it is dropped and
                // the `within` check reports the escape.
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else if !matches!(parts.last(), Some(Component::RootDir)) {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

fn within(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

/// True when any component below the root is `.git`.
///
/// Only components below the root count: a project that legitimately lives
/// under a directory named `.git` (a worktree of one, say) must still work, and
/// that is decided by the root itself, not by the path being checked.
fn touches_git(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    relative
        .components()
        .any(|component| component.as_os_str().eq_ignore_ascii_case(".git"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/home/user/project")
    }

    #[test]
    fn a_plain_child_is_allowed() {
        let path = resolve(&root(), "src/main.rs", false).unwrap();
        assert_eq!(path, PathBuf::from("/home/user/project/src/main.rs"));
    }

    #[test]
    fn a_relative_climb_cannot_escape() {
        // The `..` is resolved before the comparison, so this is rejected
        // rather than merely looking suspicious.
        assert_eq!(
            resolve(&root(), "../../etc/passwd", false).unwrap_err(),
            PathError::Outside
        );
        assert_eq!(
            resolve(&root(), "src/../../outside", false).unwrap_err(),
            PathError::Outside
        );
    }

    #[test]
    fn an_absolute_path_outside_is_rejected() {
        assert_eq!(
            resolve(&root(), "/etc/passwd", false).unwrap_err(),
            PathError::Outside
        );
    }

    #[test]
    fn a_leading_dotdot_that_stays_inside_is_allowed() {
        let path = resolve(&root(), "src/../README.md", false).unwrap();
        assert_eq!(path, PathBuf::from("/home/user/project/README.md"));
    }

    #[test]
    fn the_root_itself_is_refused_unless_allowed() {
        assert_eq!(resolve(&root(), "", false).unwrap_err(), PathError::IsRoot);
        assert_eq!(resolve(&root(), ".", false).unwrap_err(), PathError::IsRoot);
        assert!(resolve(&root(), "", true).is_ok());
    }

    #[test]
    fn git_is_unreachable_at_any_depth() {
        for path in [".git/config", "src/.git/hooks/pre-commit", ".git"] {
            assert_eq!(
                resolve(&root(), path, false).unwrap_err(),
                PathError::Git,
                "{path} should be refused"
            );
        }
        // Case does not save it.
        assert_eq!(
            resolve(&root(), ".GIT/config", false).unwrap_err(),
            PathError::Git
        );
    }

    #[test]
    fn a_similar_name_is_not_git() {
        assert!(resolve(&root(), ".github/workflows/ci.yml", false).is_ok());
        assert!(resolve(&root(), ".gitignore", false).is_ok());
    }

    #[test]
    fn a_nul_byte_is_malformed() {
        assert_eq!(
            resolve(&root(), "src/\0main.rs", false).unwrap_err(),
            PathError::Malformed
        );
    }

    #[test]
    fn an_overlong_path_is_malformed() {
        let long = "a".repeat(MAX_PATH + 1);
        assert_eq!(
            resolve(&root(), &long, false).unwrap_err(),
            PathError::Malformed
        );
    }

    #[test]
    fn a_symlink_pointing_outside_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, project.join("link")).unwrap();

        // The lexical check passes; following the link must not.
        assert!(resolve(&project, "link/secret", false).is_ok());
        assert_eq!(
            existing(&project, "link/secret", false).unwrap_err(),
            PathError::Outside
        );
    }

    #[test]
    fn an_existing_file_inside_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let path = existing(dir.path(), "a.txt", false).unwrap();
        assert!(path.ends_with("a.txt"));
    }

    #[test]
    fn creating_a_new_file_checks_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        assert!(existing(dir.path(), "src/new.rs", false).is_ok());
        assert!(existing(dir.path(), "missing/new.rs", false).is_err());
    }
}
