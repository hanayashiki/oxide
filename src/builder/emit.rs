//! Module → file emit. Each function takes a VFS output path,
//! converts it via `host.to_real`, then hands the real path to inkwell
//! / LLVM. The OS boundary is the `to_real` call; `module.print_to_*`
//! and `tm.write_to_file` see only real `&Path`s.

use std::fs;
use std::path::{Path, PathBuf};

use inkwell::module::Module;
use inkwell::targets::{FileType, TargetMachine};

use crate::loader::{BuilderHost, MaterializeError};

use super::BuildError;

/// Write textual LLVM IR (`.ll`) to a VFS path. Caller has already
/// verified the module and run any opt passes.
pub(super) fn write_ir(
    host: &dyn BuilderHost,
    module: &Module<'_>,
    out_vfs: &Path,
) -> Result<PathBuf, BuildError> {
    let real = materialize(host, out_vfs)?;
    ensure_parent_dir(&real)?;
    module
        .print_to_file(&real)
        .map_err(|e| BuildError::Emit { path: real.clone(), msg: e.to_string() })?;
    Ok(real)
}

/// Write LLVM bitcode (`.bc`) to a VFS path.
pub(super) fn write_bc(
    host: &dyn BuilderHost,
    module: &Module<'_>,
    out_vfs: &Path,
) -> Result<PathBuf, BuildError> {
    let real = materialize(host, out_vfs)?;
    ensure_parent_dir(&real)?;
    if module.write_bitcode_to_path(&real) {
        Ok(real)
    } else {
        Err(BuildError::Emit {
            path: real,
            msg: "write_bitcode_to_path returned false".into(),
        })
    }
}

/// Write a native object file (`.o` / `.obj`) to a VFS path.
pub(super) fn write_obj(
    host: &dyn BuilderHost,
    machine: &TargetMachine,
    module: &Module<'_>,
    out_vfs: &Path,
) -> Result<PathBuf, BuildError> {
    let real = materialize(host, out_vfs)?;
    ensure_parent_dir(&real)?;
    machine
        .write_to_file(module, FileType::Object, &real)
        .map_err(|e| BuildError::Emit { path: real.clone(), msg: e.to_string() })?;
    Ok(real)
}

fn materialize(host: &dyn BuilderHost, vfs: &Path) -> Result<PathBuf, BuildError> {
    host.to_real(vfs).map_err(|e| match e {
        MaterializeError::NotMaterializable { vfs_path } => BuildError::Emit {
            path: vfs_path,
            msg: "VFS path is not materializable (mounted, in-memory only)".into(),
        },
        MaterializeError::Io { path, source } => BuildError::Io { path, source },
    })
}

fn ensure_parent_dir(real: &Path) -> Result<(), BuildError> {
    if let Some(parent) = real.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| BuildError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }
    Ok(())
}
