//! Builder — drives the back end from a typechecked `HirProgram`
//! through codegen, optional optimization, object emission, and (for
//! `EmitKind::Exe`) linker invocation. The entry point is
//! [`build`]; everything else in this module is implementation
//! detail.
//!
//! Path discipline: the compiler internally only sees VFS paths.
//! Real disk paths exist only at the OS boundary, where
//! `host.to_real` materializes a VFS path immediately before the
//! syscall that needs one. See `spec/14_MODULES.md` and the host
//! abstraction in `src/loader/host.rs`.
//!
//! Linker policy follows `spec/06_LLVM_CODEGEN.md:354-359` —
//! delegate to the system-resolved `cc` (or `link.exe` on MSVC), not
//! `lld` directly, so the host's C runtime + crt0 + libSystem /
//! glibc paths are picked up for free.

mod emit;
mod link;
mod target;

use std::path::PathBuf;
use std::process::ExitStatus;

use inkwell::context::Context;

use crate::config::OptLevel;
use crate::hir::HirProgram;
use crate::session::Session;
use crate::typeck::TypeckResults;

/// Top-level builder entry point. Lowers `hir` to LLVM IR, optionally
/// optimizes, and emits the requested artifact kind.
pub fn build(
    sess: &Session,
    hir: &HirProgram,
    typeck: &TypeckResults,
    opts: &BuildOptions,
) -> Result<BuildArtifact, BuildError> {
    let machine = target::resolve(&sess.config)?;

    let ctx = Context::create();
    let module = crate::codegen::codegen(&ctx, hir, typeck, &opts.module_name);
    target::stamp_module(&module, &machine);

    if sess.config.opt_level != OptLevel::None {
        target::run_opt_passes(&module, &machine, sess.config.opt_level)?;
    }

    let host = sess.host;

    match opts.emit {
        EmitKind::Ir => {
            let out_vfs = resolve_output_path(host, opts, "ll")?;
            let real = emit::write_ir(host, &module, &out_vfs)?;
            Ok(BuildArtifact {
                kind: EmitKind::Ir,
                primary_output: out_vfs,
                primary_output_real: Some(real),
                intermediate_obj: None,
            })
        }
        EmitKind::Bc => {
            let out_vfs = resolve_output_path(host, opts, "bc")?;
            let real = emit::write_bc(host, &module, &out_vfs)?;
            Ok(BuildArtifact {
                kind: EmitKind::Bc,
                primary_output: out_vfs,
                primary_output_real: Some(real),
                intermediate_obj: None,
            })
        }
        EmitKind::Obj => {
            let out_vfs = resolve_output_path(host, opts, "o")?;
            let real = emit::write_obj(host, &machine, &module, &out_vfs)?;
            Ok(BuildArtifact {
                kind: EmitKind::Obj,
                primary_output: out_vfs,
                primary_output_real: Some(real),
                intermediate_obj: None,
            })
        }
        EmitKind::Exe => {
            let exe_vfs = resolve_output_path(host, opts, exe_extension())?;
            let obj_vfs = host.workdir().join(format!(
                "{}-{}.o",
                opts.module_name,
                std::process::id()
            ));
            let obj_real = emit::write_obj(host, &machine, &module, &obj_vfs)?;
            let exe_real = link::link_executable(
                host,
                &sess.config,
                &obj_vfs,
                &exe_vfs,
                &opts.extra_link_args,
            )?;

            let intermediate_obj = if opts.keep_intermediates {
                Some(obj_vfs)
            } else {
                let _ = std::fs::remove_file(&obj_real);
                None
            };

            Ok(BuildArtifact {
                kind: EmitKind::Exe,
                primary_output: exe_vfs,
                primary_output_real: Some(exe_real),
                intermediate_obj,
            })
        }
    }
}

/// Decide a VFS output path from `BuildOptions::output`, defaulting to
/// `<workdir>/<module_name>.<ext>` when `Auto`.
fn resolve_output_path(
    host: &dyn crate::loader::BuilderHost,
    opts: &BuildOptions,
    ext: &str,
) -> Result<PathBuf, BuildError> {
    match &opts.output {
        OutputPath::Explicit(p) => Ok(p.clone()),
        OutputPath::Auto => {
            let mut p = host.workdir().to_path_buf();
            if ext.is_empty() {
                p.push(&opts.module_name);
            } else {
                p.push(format!("{}.{}", opts.module_name, ext));
            }
            Ok(p)
        }
    }
}

#[cfg(target_os = "windows")]
fn exe_extension() -> &'static str {
    "exe"
}

#[cfg(not(target_os = "windows"))]
fn exe_extension() -> &'static str {
    ""
}

/// What the builder emits.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EmitKind {
    /// Textual LLVM IR (`.ll`).
    Ir,
    /// LLVM bitcode (`.bc`).
    Bc,
    /// Native object file (`.o` / `.obj`).
    Obj,
    /// Linked executable.
    Exe,
}

/// Where the primary output goes. VFS-shaped; the host converts via
/// `to_real` at write time.
#[derive(Debug, Clone)]
pub enum OutputPath {
    /// Caller-supplied VFS path.
    Explicit(PathBuf),
    /// Derived as `<host.workdir()>/<module_name>.<ext>`.
    Auto,
}

/// Per-invocation build knobs. Distinct from [`crate::config::CompilerConfig`],
/// which holds project-pinned settings loaded from `oxide.toml`.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub emit: EmitKind,
    pub output: OutputPath,
    /// LLVM module name; passed through to `codegen()` and used as the
    /// stem of `Auto` output paths.
    pub module_name: String,
    /// When `emit == Exe`, retain the intermediate `.o` next to the
    /// executable instead of deleting it on success.
    pub keep_intermediates: bool,
    /// CLI-only linker arguments, appended *after* `config.link_args`.
    pub extra_link_args: Vec<String>,
}

impl BuildOptions {
    /// Reasonable defaults for a one-shot build: emit an executable
    /// under the host's workdir, no intermediates retained.
    pub fn new(module_name: impl Into<String>) -> Self {
        Self {
            emit: EmitKind::Exe,
            output: OutputPath::Auto,
            module_name: module_name.into(),
            keep_intermediates: false,
            extra_link_args: Vec::new(),
        }
    }
}

/// What `build` produces on success.
#[derive(Debug)]
pub struct BuildArtifact {
    pub kind: EmitKind,
    /// VFS path of the primary output (`.ll` / `.bc` / `.o` / exe).
    pub primary_output: PathBuf,
    /// Real disk path of the primary output, if the host had a real
    /// path to give us. Most callers want this for spawning the
    /// resulting binary.
    pub primary_output_real: Option<PathBuf>,
    /// Intermediate `.o`, present only when `kind == Exe` and
    /// `BuildOptions::keep_intermediates` was set.
    pub intermediate_obj: Option<PathBuf>,
}

#[derive(Debug)]
pub enum BuildError {
    /// Failure setting up the LLVM target (triple parse, target init,
    /// `create_target_machine`).
    Target(String),
    /// LLVM optimization pipeline failed.
    OptPipeline(String),
    /// Failure writing IR / bitcode / object to disk via inkwell.
    Emit { path: PathBuf, msg: String },
    /// Filesystem-level error (mkdir, materialize) outside inkwell.
    Io { path: PathBuf, source: std::io::Error },
    /// Linker binary not on `PATH` (or absolute path doesn't exist).
    LinkerNotFound { tried: Vec<String> },
    /// Linker spawned but exited non-zero.
    LinkerFailed {
        linker: String,
        status: ExitStatus,
        stderr: String,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Target(m) => write!(f, "target setup failed: {m}"),
            Self::OptPipeline(m) => write!(f, "LLVM optimization pipeline failed: {m}"),
            Self::Emit { path, msg } => {
                write!(f, "failed to emit {}: {msg}", path.display())
            }
            Self::Io { path, source } => {
                write!(f, "I/O error at {}: {source}", path.display())
            }
            Self::LinkerNotFound { tried } => {
                write!(f, "linker not found (tried: {})", tried.join(", "))
            }
            Self::LinkerFailed { linker, status, stderr } => {
                write!(
                    f,
                    "linker `{linker}` exited with status {status}\nstderr:\n{stderr}"
                )
            }
        }
    }
}

impl std::error::Error for BuildError {}
