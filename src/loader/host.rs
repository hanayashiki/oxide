//! Filesystem abstraction for the loader and builder.
//!
//! `BuilderHost` is the seam between the compiler and the world
//! outside it — disk in production, an in-memory map in tests. The
//! compiler internally only ever sees VFS paths; real disk paths
//! exist only at the OS boundary, where `to_real` materializes a
//! VFS path into something `open(2)` / `Command::new` can use.
//!
//! Read side splits into `resolve` (canonicalize a raw import string
//! against the importing file) and `read` (load source text). Write
//! side is implicit: callers construct a VFS path under
//! `host.workdir()` and hand it to `host.to_real` immediately before
//! the syscall that needs a real path.
//!
//! The host carries no compiler configuration; that lives in
//! `CompilerConfig` and flows alongside.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum ResolveError {
    /// `import "<raw>";` did not resolve to any known file.
    NotFound { raw: String },
}

#[derive(Debug)]
pub enum MaterializeError {
    /// The VFS path corresponds to in-memory mounted content that has
    /// no on-disk realization. v0 hosts surface this for *source*
    /// paths; future DWARF emit will turn these into a tmpfile dump.
    NotMaterializable { vfs_path: PathBuf },
    /// Lower-level IO error while preparing the real path (e.g.,
    /// failed to create the workdir).
    Io { path: PathBuf, source: io::Error },
}

pub trait BuilderHost {
    /// Resolve a raw import path against the importing file's canonical
    /// path. Returns the canonical VFS path of the imported file.
    fn resolve(&self, importing: &Path, raw: &str) -> Result<PathBuf, ResolveError>;

    /// Read the source text of a canonical-path file.
    fn read(&self, canonical: &Path) -> Result<String, io::Error>;

    /// Convert a VFS path into a real disk path suitable for an OS
    /// syscall (`open(2)`, `Command::new`, inkwell write methods).
    /// The compiler calls this immediately before crossing the OS
    /// boundary; the returned path is not stored or threaded back.
    ///
    /// Default implementation treats every VFS path as already-real —
    /// suitable for hosts whose mount table is empty (production) or
    /// whose mounted entries are reads-only (the v0 in-memory host).
    /// Hosts with materializable mounts override this.
    fn to_real(&self, vfs_path: &Path) -> Result<PathBuf, MaterializeError> {
        Ok(vfs_path.to_path_buf())
    }

    /// Workspace directory under which the builder places intermediate
    /// artifacts (`.o` files for `EmitKind::Exe`, etc.). VFS-shaped;
    /// the builder calls `to_real` on paths derived from here before
    /// writing.
    fn workdir(&self) -> &Path;
}

/// Overlay host: a mount table that overrides disk fallthrough.
/// Production usage hands an empty mount table and every read hits
/// `std::fs`; tests mount synthetic sources at chosen VFS paths
/// (typically used by the snapshot fixture splitter — multiple
/// `/// path` segments populate the map). Mount-wins-on-collision:
/// if the same path is both mounted and on disk, the mount is
/// returned.
///
/// `to_real` for mounted entries errors with `NotMaterializable` in
/// v0 (no caller needs source materialization yet; DWARF will be
/// the first). Unmounted paths pass through `to_real` unchanged so
/// the builder can write intermediates to a real workdir like
/// `target/oxide-build`.
pub struct VfsHost {
    files: HashMap<PathBuf, String>,
    workdir: PathBuf,
}

/// Bundled stdlib, baked into the binary via `include_str!`. Each
/// entry is `(import-name, source)`; the import name is also the
/// canonical VFS path the file mounts at. Resolution checks this
/// table before relative-path resolution per `spec/14_MODULES.md:206-211`.
const STDLIB_FILES: &[(&str, &str)] = &[
    ("stdio.ox", include_str!("../../stdlib/stdio.ox")),
    ("stdlib.ox", include_str!("../../stdlib/stdlib.ox")),
    ("string.ox", include_str!("../../stdlib/string.ox")),
];

impl VfsHost {
    /// Build a VFS host with the bundled stdlib pre-mounted plus
    /// `files` layered on top (user mounts win on collision so tests
    /// can shadow stdlib entries). Workdir defaults to
    /// `target/oxide-build`; override via `with_workdir`.
    pub fn new(files: HashMap<PathBuf, String>) -> Self {
        let mut merged: HashMap<PathBuf, String> = STDLIB_FILES
            .iter()
            .map(|(name, src)| (PathBuf::from(name), (*src).to_string()))
            .collect();
        merged.extend(files);
        Self {
            files: merged,
            workdir: PathBuf::from("target/oxide-build"),
        }
    }

    /// Build a VFS host without the bundled stdlib pre-mounted.
    /// Useful for resolver-isolation tests that want to exercise the
    /// disk-fallthrough or not-found paths without stdlib in the way.
    #[cfg(test)]
    pub fn new_bare(files: HashMap<PathBuf, String>) -> Self {
        Self {
            files,
            workdir: PathBuf::from("target/oxide-build"),
        }
    }

    /// Override the build workdir. The builder writes intermediate
    /// `.o` files (for `EmitKind::Exe`) under this directory.
    pub fn with_workdir(mut self, workdir: impl Into<PathBuf>) -> Self {
        self.workdir = workdir.into();
        self
    }
}

/// Returns true when `raw` matches a bundled stdlib name. Resolution
/// short-circuits to `PathBuf::from(raw)` (bare-name key) without
/// joining against the importing file's directory.
fn is_stdlib_name(raw: &str) -> bool {
    STDLIB_FILES.iter().any(|(name, _)| *name == raw)
}

impl BuilderHost for VfsHost {
    fn resolve(&self, importing: &Path, raw: &str) -> Result<PathBuf, ResolveError> {
        // Stdlib hardcode table wins over relative resolution per
        // spec/14_MODULES.md:206-211. Collision policy: a user file
        // named `stdio.ox` next to main.ox is shadowed by the bundled
        // stdlib; rename the local file to disambiguate.
        if is_stdlib_name(raw) {
            return Ok(PathBuf::from(raw));
        }
        let base = importing.parent().unwrap_or(Path::new(""));
        let candidate = lexical_normalize(&base.join(raw));
        if self.files.contains_key(&candidate) || candidate.exists() {
            Ok(candidate)
        } else {
            Err(ResolveError::NotFound { raw: raw.to_string() })
        }
    }

    fn read(&self, canonical: &Path) -> Result<String, io::Error> {
        if let Some(s) = self.files.get(canonical) {
            return Ok(s.clone());
        }
        fs::read_to_string(canonical)
    }

    fn to_real(&self, vfs_path: &Path) -> Result<PathBuf, MaterializeError> {
        if self.files.contains_key(vfs_path) {
            Err(MaterializeError::NotMaterializable {
                vfs_path: vfs_path.to_path_buf(),
            })
        } else {
            Ok(vfs_path.to_path_buf())
        }
    }

    fn workdir(&self) -> &Path {
        &self.workdir
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

    #[test]
    fn to_real_passes_unmounted_paths_through() {
        let h = host(&[("/main.ox", "")]);
        let real = h.to_real(Path::new("target/foo.o")).unwrap();
        assert_eq!(real, PathBuf::from("target/foo.o"));
    }

    #[test]
    fn to_real_errors_on_mounted_source() {
        let h = host(&[("/main.ox", "fn main() {}")]);
        let err = h.to_real(Path::new("/main.ox")).unwrap_err();
        assert!(matches!(err, MaterializeError::NotMaterializable { .. }));
    }

    #[test]
    fn workdir_defaults_to_target_oxide_build() {
        let h = host(&[]);
        assert_eq!(h.workdir(), Path::new("target/oxide-build"));
    }

    #[test]
    fn workdir_can_be_overridden() {
        let h = host(&[]).with_workdir("/tmp/custom-workdir");
        assert_eq!(h.workdir(), Path::new("/tmp/custom-workdir"));
    }

    fn artifacts_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from("target/test-artifacts/loader-host").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir test artifacts");
        dir
    }

    #[test]
    fn read_falls_through_to_disk_when_unmounted() {
        let dir = artifacts_dir("read_falls_through_to_disk_when_unmounted");
        let p = dir.join("note.txt");
        std::fs::write(&p, "from disk").expect("write fixture");
        let h = host(&[]); // empty mount table
        let s = h.read(&p).expect("disk read");
        assert_eq!(s, "from disk");
    }

    #[test]
    fn read_mount_wins_over_disk_collision() {
        let dir = artifacts_dir("read_mount_wins_over_disk_collision");
        let p = dir.join("note.txt");
        std::fs::write(&p, "from disk").expect("write fixture");
        let mut map = HashMap::new();
        map.insert(p.clone(), "from mount".to_string());
        let h = VfsHost::new(map);
        let s = h.read(&p).expect("mount read");
        assert_eq!(s, "from mount");
    }

    #[test]
    fn resolve_falls_through_to_disk_when_unmounted() {
        let dir = artifacts_dir("resolve_falls_through_to_disk_when_unmounted");
        let main_path = dir.join("main.ox");
        let util_path = dir.join("util.ox");
        std::fs::write(&main_path, "").expect("write main fixture");
        std::fs::write(&util_path, "").expect("write util fixture");
        let h = host(&[]); // empty mount table; everything from disk
        let r = h.resolve(&main_path, "./util.ox").expect("disk resolve");
        assert_eq!(r, util_path);
    }

    #[test]
    fn resolve_not_found_after_disk_check() {
        let dir = artifacts_dir("resolve_not_found_after_disk_check");
        let main_path = dir.join("main.ox");
        std::fs::write(&main_path, "").expect("write main fixture");
        let h = host(&[]);
        let err = h
            .resolve(&main_path, "./does-not-exist.ox")
            .unwrap_err();
        let ResolveError::NotFound { raw } = err;
        assert_eq!(raw, "./does-not-exist.ox");
    }
}
