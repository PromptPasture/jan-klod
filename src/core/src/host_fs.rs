//! Path-jailed workspace directory for `host-fs`.
//!
//! Paths are workspace-relative; absolute paths and `..` escapes rejected.
//! Default-deny: unconfigured means all ops return `Denied`.
//!
//! Caveat: lexical jailing only (no symlink escapes detected).

use std::path::{Component, Path, PathBuf};

/// Filesystem operation error (mirrors `host-fs.fs-error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// Path does not exist.
    NotFound,
    /// Path escapes workspace or no workspace configured.
    Denied,
    /// I/O error.
    Io,
}

/// One directory entry (mirrors `host-fs.entry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Entry name (not a full path).
    pub name: String,
    /// Is the entry a directory.
    pub is_dir: bool,
}

/// Path-jailed workspace root for serving `host-fs`.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Wrap `root` as a workspace (canonicalized for jail checks).
    ///
    /// # Errors
    /// [`FsError::Io`] if `root` cannot be canonicalized (e.g. missing).
    pub fn open(root: impl AsRef<Path>) -> Result<Self, FsError> {
        let root = root.as_ref().canonicalize().map_err(|_| FsError::Io)?;
        Ok(Self { root })
    }

    /// The canonicalized workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a workspace-relative path to an absolute path inside the root.
    ///
    /// # Errors
    /// [`FsError::Denied`] for absolute paths or `..` escapes.
    pub fn resolve(&self, requested: &str) -> Result<PathBuf, FsError> {
        if Path::new(requested).is_absolute() {
            return Err(FsError::Denied);
        }
        let candidate = normalize(&self.root.join(requested));
        if candidate.starts_with(&self.root) {
            Ok(candidate)
        } else {
            Err(FsError::Denied)
        }
    }

    /// Read a UTF-8 file.
    ///
    /// # Errors
    /// [`FsError::Denied`], [`FsError::NotFound`], or [`FsError::Io`].
    pub fn read(&self, path: &str) -> Result<String, FsError> {
        let full = self.resolve(path)?;
        match std::fs::read_to_string(&full) {
            Ok(contents) => Ok(contents),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(FsError::NotFound),
            Err(_) => Err(FsError::Io),
        }
    }

    /// Create or replace a UTF-8 file (creates parent directories).
    ///
    /// # Errors
    /// [`FsError::Denied`] or [`FsError::Io`].
    pub fn write(&self, path: &str, contents: &str) -> Result<(), FsError> {
        let full = self.resolve(path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|_| FsError::Io)?;
        }
        std::fs::write(&full, contents).map_err(|_| FsError::Io)
    }

    /// List a directory.
    ///
    /// # Errors
    /// [`FsError::Denied`], [`FsError::NotFound`], or [`FsError::Io`].
    pub fn list_dir(&self, path: &str) -> Result<Vec<Entry>, FsError> {
        let full = self.resolve(path)?;
        let read_dir = match std::fs::read_dir(&full) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(FsError::NotFound)
            }
            Err(_) => return Err(FsError::Io),
        };
        let mut entries = Vec::new();
        for item in read_dir {
            let item = item.map_err(|_| FsError::Io)?;
            entries.push(Entry {
                name: item.file_name().to_string_lossy().into_owned(),
                is_dir: item.file_type().is_ok_and(|t| t.is_dir()),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Whether `path` exists within the workspace.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok_and(|full| full.exists())
    }
}

/// Lexically normalize a path (fold `.` and `..`).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempdir_guard::TempDir, Workspace) {
        let dir = tempdir_guard::TempDir::new();
        let ws = Workspace::open(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn write_then_read_roundtrips() {
        let (_dir, ws) = workspace();
        ws.write("notes/todo.md", "buy milk").unwrap();
        assert_eq!(ws.read("notes/todo.md").unwrap(), "buy milk");
        assert!(ws.exists("notes/todo.md"));
    }

    #[test]
    fn reading_a_missing_file_is_not_found() {
        let (_dir, ws) = workspace();
        assert_eq!(ws.read("nope.txt"), Err(FsError::NotFound));
        assert!(!ws.exists("nope.txt"));
    }

    #[test]
    fn escapes_are_denied() {
        let (_dir, ws) = workspace();
        assert_eq!(ws.resolve("../outside"), Err(FsError::Denied));
        assert_eq!(ws.resolve("a/../../outside"), Err(FsError::Denied));
        assert_eq!(ws.resolve("/etc/passwd"), Err(FsError::Denied));
        assert_eq!(ws.read("../../etc/passwd"), Err(FsError::Denied));
    }

    #[test]
    fn inner_relative_paths_resolve_inside_the_root() {
        let (_dir, ws) = workspace();
        // Relative paths that stay inside are allowed (e.g., `a/../b`).
        assert!(ws.resolve("a/../b.txt").is_ok());
        ws.write("a/../b.txt", "x").unwrap();
        assert!(ws.exists("b.txt"));
    }

    #[test]
    fn list_dir_reports_entries_sorted() {
        let (_dir, ws) = workspace();
        ws.write("z.txt", "1").unwrap();
        ws.write("a.txt", "2").unwrap();
        let names: Vec<String> = ws
            .list_dir(".")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["a.txt".to_string(), "z.txt".to_string()]);
    }

    /// Self-cleaning temp dir (minimal, no dev-deps).
    mod tempdir_guard {
        use std::path::{Path, PathBuf};

        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir(PathBuf);
        impl TempDir {
            pub fn new() -> Self {
                // PID + counter for collision-free parallel test isolation.
                let base = std::env::temp_dir().join(format!(
                    "jk-fs-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&base).unwrap();
                Self(base)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
