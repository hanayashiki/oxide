//! Compiler configuration. Plain data, no behavior.
//!
//! `CompilerConfig` is read from a project-level config file (e.g.
//! `oxide.toml`) by the driver. Layers that need configuration take a
//! `&CompilerConfig` parameter directly. The struct is serde-ready
//! behind the optional `serde` feature.
//!
//! Fields here are *project-pinned* — the kind of knob a user records
//! once for the project. Per-invocation knobs (output path, emit kind)
//! live on `BuildOptions` in the builder layer.

use std::path::PathBuf;

use target_lexicon::Triple;

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
pub struct CompilerConfig {
    /// LLVM target triple. `None` ⇒ the builder falls back to the host
    /// triple via `TargetMachine::get_default_triple()`. Validated at
    /// config-load time so typos surface as a parse error rather than
    /// a cryptic LLVM crash later.
    pub target_triple: Option<Triple>,

    /// LLVM optimization level applied via the new pass manager.
    /// Default: `None` (no pipeline run, fastest compile).
    pub opt_level: OptLevel,

    /// Linker selection. `None` ⇒ pick by triple: `cc` on Unix /
    /// MinGW, `link.exe` on MSVC.
    pub linker: Option<LinkerChoice>,

    /// Verbatim arguments appended after the obj on the linker command
    /// line. Use for things like `-static`, `-Wl,...`.
    pub link_args: Vec<String>,

    /// Libraries to link. Rendered as `-l<name>` for cc-style linkers,
    /// `<name>.lib` for `link.exe`.
    pub link_libs: Vec<String>,
}

/// LLVM optimization level. Maps to `-O0/1/2/3/s/z` in cc and to the
/// `default<O*>` pipeline string consumed by the new pass manager.
/// Owned by oxide rather than re-exporting `inkwell::OptimizationLevel`
/// so `serde::Deserialize` works through the optional feature.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
pub enum OptLevel {
    #[default]
    None,
    O1,
    O2,
    O3,
    Os,
    Oz,
}

/// Which linker to invoke. `Custom` is an absolute or PATH-resolved
/// program name; the builder spawns it directly without modifying argv
/// shape — caller is responsible for passing a cc-compatible binary.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
pub enum LinkerChoice {
    /// `cc`: clang on macOS, gcc on most Linux distros, MinGW cc on
    /// Windows-GNU. Argv shape: `cc -o <out> <obj> -l<libs> <args>`.
    Cc,
    /// `link.exe`: Microsoft's linker on Windows MSVC. Argv shape:
    /// `link.exe /OUT:<out> <obj> <libs>.lib <args>`.
    LinkExe,
    /// User-provided binary; argv shape follows the cc convention by
    /// default unless `link_args` overrides it entirely.
    Custom(PathBuf),
}
