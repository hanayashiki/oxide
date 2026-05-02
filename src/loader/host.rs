//! Filesystem abstraction for the loader.
//!
//! `BuilderHost` is the seam between the loader (which discovers
//! files, parses them, and builds the file graph) and the world
//! outside the compiler — disk in production, an in-memory map in
//! tests. The trait deliberately stays small: two methods covering
//! "where do bytes come from" and "how do I name files."
//!
//! Resolution and reading are split because the loader needs to dedup
//! by canonical path before reading: two different relative spellings
//! that point at the same file collapse to one read and one parse.
//!
//! The host carries no compiler configuration; that lives in
//! `CompilerConfig` and flows alongside.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum ResolveError {
    /// `import "<raw>";` did not resolve to any known file.
    NotFound { raw: String },
}

pub trait BuilderHost {
    /// Resolve a raw import path against the importing file's canonical
    /// path. Returns the canonical path of the imported file.
    fn resolve(&self, importing: &Path, raw: &str) -> Result<PathBuf, ResolveError>;

    /// Read the source text of a canonical-path file.
    fn read(&self, canonical: &Path) -> Result<String, io::Error>;
}

/// In-memory host backed by a `HashMap<PathBuf, String>`. Used by
/// snapshot tests via the fixture splitter — multiple `/// path`
/// segments populate the map and the loader walks them as if they
/// were on disk.
pub struct VfsHost {
    files: HashMap<PathBuf, String>,
}

impl VfsHost {
    pub fn new(files: HashMap<PathBuf, String>) -> Self {
        Self { files }
    }
}

impl BuilderHost for VfsHost {
    fn resolve(&self, importing: &Path, raw: &str) -> Result<PathBuf, ResolveError> {
        let base = importing.parent().unwrap_or(Path::new(""));
        let candidate = lexical_normalize(&base.join(raw));
        if self.files.contains_key(&candidate) {
            Ok(candidate)
        } else {
            Err(ResolveError::NotFound { raw: raw.to_string() })
        }
    }

    fn read(&self, canonical: &Path) -> Result<String, io::Error> {
        self.files.get(canonical).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("VfsHost: no file at {}", canonical.display()),
            )
        })
    }
}

/// Collapse `.` / `..` segments without touching the filesystem. VFS
/// paths aren't on disk, so `fs::canonicalize` would error; we
/// normalize lexically instead. The result is a canonical-form path
/// suitable for visited-set dedup.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop only if the last component is a normal directory.
                // Leading `..` (or `..` past a root) stays as-is.
                let popped = matches!(
                    out.components().next_back(),
                    Some(Component::Normal(_))
                );
                if popped {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(entries: &[(&str, &str)]) -> VfsHost {
        VfsHost::new(
            entries
                .iter()
                .map(|(k, v)| (PathBuf::from(k), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn resolve_relative_sibling() {
        let h = host(&[("/main.ox", ""), ("/util.ox", "")]);
        let r = h.resolve(Path::new("/main.ox"), "./util.ox").unwrap();
        assert_eq!(r, PathBuf::from("/util.ox"));
    }

    #[test]
    fn resolve_collapses_dot_segments() {
        let h = host(&[("/a/main.ox", ""), ("/a/util.ox", "")]);
        let r = h
            .resolve(Path::new("/a/main.ox"), "./././util.ox")
            .unwrap();
        assert_eq!(r, PathBuf::from("/a/util.ox"));
    }

    #[test]
    fn resolve_collapses_dotdot_segments() {
        let h = host(&[("/a/main.ox", ""), ("/util.ox", "")]);
        let r = h.resolve(Path::new("/a/main.ox"), "../util.ox").unwrap();
        assert_eq!(r, PathBuf::from("/util.ox"));
    }

    #[test]
    fn resolve_not_found_returns_error() {
        let h = host(&[("/main.ox", "")]);
        let err = h.resolve(Path::new("/main.ox"), "./missing.ox").unwrap_err();
        let ResolveError::NotFound { raw } = err;
        assert_eq!(raw, "./missing.ox");
    }

    #[test]
    fn read_returns_source() {
        let h = host(&[("/main.ox", "fn main() {}")]);
        let s = h.read(Path::new("/main.ox")).unwrap();
        assert_eq!(s, "fn main() {}");
    }

    #[test]
    fn read_missing_returns_io_error() {
        let h = host(&[]);
        let err = h.read(Path::new("/missing.ox")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
