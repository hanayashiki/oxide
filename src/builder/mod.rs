//! Builder — drives the full compilation pipeline (load → lower →
//! typeck → mono → codegen → optional optimization → object emission
//! → linker invocation) via a stateful [`Builder`] struct that fires
//! a per-phase [`Tapper`] callback as each phase completes. The CLI
//! and snapshot tests both go through this same orchestrator;
//! everything else in this module is implementation detail.
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

mod driver;
mod emit;
mod link;
mod target;

pub use driver::{
    BuildAborted, Builder, Flow, Inputs, NoopTapper, Phase, TapAst, TapCodegen, TapHir, TapLex,
    TapLoad, TapMono, TapTypeck, Tapper,
};

use std::path::PathBuf;
use std::process::ExitStatus;

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

/// What [`Builder::emit_artifact`] produces on success.
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
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Linker binary not on `PATH` (or absolute path doesn't exist).
    LinkerNotFound { tried: Vec<String> },
    /// Linker spawned but exited non-zero.
    LinkerFailed {
        linker: String,
        status: ExitStatus,
        stderr: String,
    },
    /// Pipeline phase produced errors before codegen could run, or the
    /// tapper halted the pipeline. The tapper has already surfaced the
    /// underlying errors to the caller; this variant is purely a
    /// "couldn't proceed" signal naming the failing phase.
    Aborted(BuildAborted),
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
            Self::LinkerFailed {
                linker,
                status,
                stderr,
            } => {
                write!(
                    f,
                    "linker `{linker}` exited with status {status}\nstderr:\n{stderr}"
                )
            }
            Self::Aborted(a) => write!(f, "build aborted at phase: {a:?}"),
        }
    }
}

impl std::error::Error for BuildError {}
