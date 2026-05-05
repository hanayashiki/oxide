//! Centralized build pipeline driver.
//!
//! `Builder` owns a `Session`, an `Inputs` (multi-file `Root` or
//! single-file `Inline`), and a borrowed `Tapper`. As each phase runs,
//! the Builder hands references to the just-produced output to the
//! tapper for the duration of one method call, then drops them. The
//! tapper persists whatever it needs (typically by appending to a
//! `String`) inside the callback.
//!
//! Pipeline shape:
//!   Inline mode:  Lex → Ast → Load(synthetic) → Hir → Typeck → Mono → Codegen → Emit
//!   Root mode:                Load             → Hir → Typeck → Mono → Codegen → Emit
//!
//! `Phase::Lex` / `Phase::Ast` are valid only in `Inputs::Inline` —
//! single-file inspection (`--emit lex|ast`) bypasses the loader DFS.
//! Multi-file builds always go through `load_program`.
//!
//! Errors are surfaced to the tapper alongside outputs (`(out, errs)`
//! in each `TapXxx`). Phases run unconditionally — recoverable, with
//! poison nodes flowing downstream. The tapper decides whether to
//! halt via `Flow::Halt`. `Builder::codegen` and
//! `Builder::emit_artifact` additionally bail before invoking codegen
//! when any prior phase produced errors (codegen on poison HIR is
//! unsafe).

use std::path::PathBuf;

use index_vec::IndexVec;
use inkwell::context::Context;
use inkwell::module::Module;

use crate::codegen;
use crate::config::OptLevel;
use crate::hir::{HirError, HirProgram, lower_program};
use crate::lexer::{Token, lex};
use crate::loader::{LoadError, LoadedFile, load_program};
use crate::mono::{MonoError, MonoResults, monomorphize_with_limit};
use crate::parser::ast;
use crate::parser::{ParseError, parse};
use crate::reporter::{FileId, SourceMap};
use crate::session::Session;
use crate::typeck::{TypeError, TypeckResults, check};

use super::{
    BuildArtifact, BuildError, BuildOptions, EmitKind, OutputPath, emit, link, target,
};

/// Pipeline phases the Builder can drive to. Ordering matters — `Ord`
/// is derived so `up_to >= Phase::Hir` makes sense.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Phase {
    Lex,
    Ast,
    Load,
    Hir,
    Typeck,
    Mono,
}

/// Tapper return value: keep going or stop the pipeline before the
/// next phase.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Flow {
    Continue,
    Halt,
}

/// Per-phase argument bundles. Every field is a `&` borrow scoped to
/// the callback. The tapper copies/serializes into owned data
/// (String, Vec, etc.) before the call returns.

pub struct TapLex<'a> {
    pub tokens: &'a [Token],
    pub source_map: &'a SourceMap,
    pub root: FileId,
}

pub struct TapAst<'a> {
    pub ast: &'a ast::Module,
    pub errors: &'a [ParseError],
    pub source_map: &'a SourceMap,
    pub root: FileId,
}

pub struct TapLoad<'a> {
    pub loaded: &'a IndexVec<FileId, LoadedFile>,
    pub root: FileId,
    pub errors: &'a [LoadError],
    pub source_map: &'a SourceMap,
}

pub struct TapHir<'a> {
    pub hir: &'a HirProgram,
    pub errors: &'a [HirError],
    pub source_map: &'a SourceMap,
    pub root: FileId,
}

pub struct TapTypeck<'a> {
    pub hir: &'a HirProgram,
    pub results: &'a TypeckResults,
    pub errors: &'a [TypeError],
    pub source_map: &'a SourceMap,
    pub root: FileId,
}

pub struct TapMono<'a> {
    pub hir: &'a HirProgram,
    pub results: &'a TypeckResults,
    pub mono: &'a MonoResults,
    pub errors: &'a [MonoError],
    pub source_map: &'a SourceMap,
    pub root: FileId,
}

pub struct TapCodegen<'a, 'ctx> {
    pub module: &'a Module<'ctx>,
    pub source_map: &'a SourceMap,
}

/// Phase-boundary callback. Default impls are no-ops, so a tapper
/// overrides only what it cares about.
pub trait Tapper {
    fn on_lex(&mut self, _: TapLex<'_>) -> Flow {
        Flow::Continue
    }
    fn on_ast(&mut self, _: TapAst<'_>) -> Flow {
        Flow::Continue
    }
    fn on_load(&mut self, _: TapLoad<'_>) -> Flow {
        Flow::Continue
    }
    fn on_hir(&mut self, _: TapHir<'_>) -> Flow {
        Flow::Continue
    }
    fn on_typeck(&mut self, _: TapTypeck<'_>) -> Flow {
        Flow::Continue
    }
    fn on_mono(&mut self, _: TapMono<'_>) -> Flow {
        Flow::Continue
    }
    fn on_codegen(&mut self, _: TapCodegen<'_, '_>) -> Flow {
        Flow::Continue
    }
}

/// All-defaults tapper. Used by callers that don't observe phases
/// (e.g., `tests/builder.rs`'s AOT integration tests).
pub struct NoopTapper;
impl Tapper for NoopTapper {}

/// What the Builder is compiling.
#[derive(Clone, Debug)]
pub enum Inputs {
    /// Multi-file mode: production CLI + multi-file fixtures. Imports
    /// followed via `load_program`.
    Root(PathBuf),
    /// Single-file mode for `--emit lex|ast` and unit fixtures: a
    /// synthetic one-element loaded list is produced; the loader DFS
    /// is not invoked. Any `import` items in the source are *ignored*,
    /// matching today's `emit_lex`/`emit_ast` behaviour.
    Inline { path: PathBuf, src: String },
}

/// Returned by `codegen()` (and via `BuildError::Aborted` from
/// `emit_artifact`) when a prior phase produced errors or the tapper
/// halted the pipeline. The tapper has already observed the offending
/// phase's outputs + errors; this enum is just a "couldn't proceed"
/// signal that names the failing phase.
#[derive(Debug)]
pub enum BuildAborted {
    /// Load (or, in Inline mode, parse) errors blocked downstream.
    Load,
    Hir,
    Typeck,
    Mono,
    /// Tapper returned `Flow::Halt` at this phase.
    TapperHalt(Phase),
}

pub struct Builder<'h, 't, T: Tapper + ?Sized> {
    sess: Session<'h>,
    inputs: Inputs,
    tapper: &'t mut T,
    /// Cascade-depth ceiling for monomorphization. Production default
    /// is 256 (mirrors rustc's `recursion_limit`); tests that exercise
    /// divergent-mono diagnostics dial this down via
    /// `with_mono_depth_limit` so the rendered chain stays compact.
    mono_depth_limit: u32,
}

const DEFAULT_MONO_DEPTH_LIMIT: u32 = 256;

impl<'h, 't, T: Tapper + ?Sized> Builder<'h, 't, T> {
    /// Multi-file Builder rooted at `root` (a real path the host can
    /// read, or a VFS path for tests using a mounted fixture).
    pub fn from_root(sess: Session<'h>, root: PathBuf, tapper: &'t mut T) -> Self {
        Self {
            sess,
            inputs: Inputs::Root(root),
            tapper,
            mono_depth_limit: DEFAULT_MONO_DEPTH_LIMIT,
        }
    }

    /// Single-file Builder. Suitable for `--emit lex|ast` and any unit
    /// inspection that doesn't need the loader. `import` items in
    /// `src` are ignored.
    pub fn from_inline(
        sess: Session<'h>,
        path: PathBuf,
        src: String,
        tapper: &'t mut T,
    ) -> Self {
        Self {
            sess,
            inputs: Inputs::Inline { path, src },
            tapper,
            mono_depth_limit: DEFAULT_MONO_DEPTH_LIMIT,
        }
    }

    /// Override the monomorphization cascade depth limit. Tests
    /// exercising the `DivergentMonomorphization` (E0278) diagnostic
    /// dial this down (e.g. to 8) so the rendered cascade chain stays
    /// readable. Production default is 256.
    pub fn with_mono_depth_limit(mut self, limit: u32) -> Self {
        self.mono_depth_limit = limit;
        self
    }

    pub fn session(&self) -> &Session<'h> {
        &self.sess
    }

    pub fn source_map(&self) -> &SourceMap {
        &self.sess.source_map
    }

    /// Drive phases up through `up_to`, calling the matching `on_*`
    /// at each boundary. Stops early if a callback returns
    /// `Flow::Halt`. Returns the last phase that completed.
    ///
    /// `Phase::Lex` / `Phase::Ast` are valid only for `Inputs::Inline`;
    /// requesting them in `Root` mode panics.
    pub fn run(&mut self, up_to: Phase) -> Phase {
        let r = self.drive(up_to);
        r.last
    }

    /// Run through `Phase::Mono` and then codegen. Returns
    /// `Err(BuildAborted)` if any prior phase produced errors **or**
    /// the tapper halted — codegen on poison HIR is unsafe. On
    /// success, fires `on_codegen(&Module)` and returns the Module by
    /// value so the caller can JIT it, print IR, or feed
    /// [`emit_artifact`].
    pub fn codegen<'ctx>(
        &mut self,
        ctx: &'ctx Context,
        module_name: &str,
    ) -> Result<Module<'ctx>, BuildAborted> {
        let r = self.drive(Phase::Mono);
        check_clean(&r)?;
        let hir = r.hir.expect("hir present after clean drive");
        let mut typeck = r.typeck.expect("typeck present after clean drive");
        let mono = r.mono.expect("mono present after clean drive");
        let module = codegen::codegen(ctx, &hir, &mut typeck, &mono, module_name);
        let _ = self.tapper.on_codegen(TapCodegen {
            module: &module,
            source_map: &self.sess.source_map,
        });
        Ok(module)
    }

    /// CLI's "build everything" path: drive all phases, codegen, opt,
    /// emit, and (for `EmitKind::Exe`) link. Bails with
    /// `BuildError::Aborted(_)` if any phase produced errors or the
    /// tapper halted.
    pub fn emit_artifact(&mut self, opts: &BuildOptions) -> Result<BuildArtifact, BuildError> {
        let machine = target::resolve(&self.sess.config)?;

        let r = self.drive(Phase::Mono);
        if let Err(a) = check_clean(&r) {
            return Err(BuildError::Aborted(a));
        }
        let hir = r.hir.expect("hir present after clean drive");
        let mut typeck = r.typeck.expect("typeck present after clean drive");
        let mono = r.mono.expect("mono present after clean drive");

        let ctx = Context::create();
        let module = codegen::codegen(&ctx, &hir, &mut typeck, &mono, &opts.module_name);
        let _ = self.tapper.on_codegen(TapCodegen {
            module: &module,
            source_map: &self.sess.source_map,
        });
        target::stamp_module(&module, &machine);

        if self.sess.config.opt_level != OptLevel::None {
            target::run_opt_passes(&module, &machine, self.sess.config.opt_level)?;
        }

        let host = self.sess.host;

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
                    &self.sess.config,
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

    /// The shared driver: runs phases up to `up_to`, threads outputs
    /// through local variables, and surfaces each phase's `(output,
    /// errors)` to the tapper. Returns a `RunResult` carrying the
    /// last-completed phase, halt indicator, per-phase
    /// errors-present flags, and the `Mono`-tail outputs (so codegen /
    /// emit_artifact can pick them up without re-running the
    /// pipeline).
    fn drive(&mut self, up_to: Phase) -> RunResult {
        if matches!(self.inputs, Inputs::Root(_))
            && (up_to == Phase::Lex || up_to == Phase::Ast)
        {
            panic!(
                "Phase::{:?} is not valid in Inputs::Root mode (single-file inspection requires from_inline)",
                up_to
            );
        }

        let mut r = RunResult::new();

        // Stage 1: lex / parse (Inline only) and load → produces
        // `(loaded, root)` for downstream phases.
        let inputs = self.inputs.clone();
        let (loaded, root) = match inputs {
            Inputs::Inline { path, src } => {
                let file = self.sess.source_map.add(path.clone(), src.clone());
                r.root = Some(file);

                // Phase::Lex
                let tokens = lex(&src, file);
                let flow = self.tapper.on_lex(TapLex {
                    tokens: &tokens,
                    source_map: &self.sess.source_map,
                    root: file,
                });
                r.last = Phase::Lex;
                if flow == Flow::Halt {
                    r.halted = Some(Phase::Lex);
                    return r;
                }
                if up_to == Phase::Lex {
                    return r;
                }

                // Phase::Ast
                let (ast_module, parse_errs) = parse(&tokens, file);
                r.parse_errors_present = !parse_errs.is_empty();
                let flow = self.tapper.on_ast(TapAst {
                    ast: &ast_module,
                    errors: &parse_errs,
                    source_map: &self.sess.source_map,
                    root: file,
                });
                r.last = Phase::Ast;
                if flow == Flow::Halt {
                    r.halted = Some(Phase::Ast);
                    return r;
                }
                if up_to == Phase::Ast {
                    return r;
                }

                // Phase::Load (synthetic one-element list, no imports)
                let loaded_file = LoadedFile {
                    file,
                    path,
                    ast: ast_module,
                    direct_imports: Vec::new(),
                };
                let loaded: IndexVec<FileId, LoadedFile> =
                    IndexVec::from_vec(vec![loaded_file]);
                let load_errs: Vec<LoadError> = Vec::new();
                let flow = self.tapper.on_load(TapLoad {
                    loaded: &loaded,
                    root: file,
                    errors: &load_errs,
                    source_map: &self.sess.source_map,
                });
                r.last = Phase::Load;
                if flow == Flow::Halt {
                    r.halted = Some(Phase::Load);
                    return r;
                }
                if up_to == Phase::Load {
                    return r;
                }

                (loaded, file)
            }
            Inputs::Root(root_path) => {
                let (loaded, root, load_errs) =
                    load_program(self.sess.host, &mut self.sess.source_map, root_path);
                r.load_errors_present = !load_errs.is_empty();
                r.root = Some(root);
                let flow = self.tapper.on_load(TapLoad {
                    loaded: &loaded,
                    root,
                    errors: &load_errs,
                    source_map: &self.sess.source_map,
                });
                r.last = Phase::Load;
                if flow == Flow::Halt {
                    r.halted = Some(Phase::Load);
                    return r;
                }
                if up_to == Phase::Load {
                    return r;
                }

                // Root file unreadable → load returned an empty file
                // list and an IO error. `lower_program` asserts
                // non-empty, so stop here. The tapper has already
                // observed the load errors.
                if loaded.is_empty() {
                    return r;
                }

                (loaded, root)
            }
        };

        // Stage 2: HIR.
        let (hir, hir_errs) = lower_program(loaded, root);
        r.hir_errors_present = !hir_errs.is_empty();
        let flow = self.tapper.on_hir(TapHir {
            hir: &hir,
            errors: &hir_errs,
            source_map: &self.sess.source_map,
            root,
        });
        r.last = Phase::Hir;
        if flow == Flow::Halt {
            r.halted = Some(Phase::Hir);
            r.hir = Some(hir);
            return r;
        }
        if up_to == Phase::Hir {
            r.hir = Some(hir);
            return r;
        }

        // Stage 3: Typeck.
        let (mut typeck, typeck_errs) = check(&hir);
        r.typeck_errors_present = !typeck_errs.is_empty();
        let flow = self.tapper.on_typeck(TapTypeck {
            hir: &hir,
            results: &typeck,
            errors: &typeck_errs,
            source_map: &self.sess.source_map,
            root,
        });
        r.last = Phase::Typeck;
        if flow == Flow::Halt {
            r.halted = Some(Phase::Typeck);
            r.hir = Some(hir);
            r.typeck = Some(typeck);
            return r;
        }
        if up_to == Phase::Typeck {
            r.hir = Some(hir);
            r.typeck = Some(typeck);
            return r;
        }

        // Stage 4: Mono.
        let (mono, mono_errs) =
            monomorphize_with_limit(&hir, &mut typeck, self.mono_depth_limit);
        r.mono_errors_present = !mono_errs.is_empty();
        let _ = self.tapper.on_mono(TapMono {
            hir: &hir,
            results: &typeck,
            mono: &mono,
            errors: &mono_errs,
            source_map: &self.sess.source_map,
            root,
        });
        r.last = Phase::Mono;
        r.hir = Some(hir);
        r.typeck = Some(typeck);
        r.mono = Some(mono);

        r
    }
}

/// Internal pipeline outcome. Not exposed because the tapper has
/// already seen each phase's outputs; the only fields needing to
/// escape `drive` are the `Mono`-tail products consumed by codegen.
struct RunResult {
    last: Phase,
    halted: Option<Phase>,
    parse_errors_present: bool,
    load_errors_present: bool,
    hir_errors_present: bool,
    typeck_errors_present: bool,
    mono_errors_present: bool,
    root: Option<FileId>,
    hir: Option<HirProgram>,
    typeck: Option<TypeckResults>,
    mono: Option<MonoResults>,
}

impl RunResult {
    fn new() -> Self {
        Self {
            last: Phase::Lex,
            halted: None,
            parse_errors_present: false,
            load_errors_present: false,
            hir_errors_present: false,
            typeck_errors_present: false,
            mono_errors_present: false,
            root: None,
            hir: None,
            typeck: None,
            mono: None,
        }
    }
}

/// Map a `RunResult` into `Result<(), BuildAborted>` — the
/// "is this clean enough for codegen?" check.
fn check_clean(r: &RunResult) -> Result<(), BuildAborted> {
    if let Some(p) = r.halted {
        return Err(BuildAborted::TapperHalt(p));
    }
    // Parse errors (Inline mode) flow through Load conceptually —
    // they prevent a clean loaded list.
    if r.load_errors_present || r.parse_errors_present {
        return Err(BuildAborted::Load);
    }
    if r.hir_errors_present {
        return Err(BuildAborted::Hir);
    }
    if r.typeck_errors_present {
        return Err(BuildAborted::Typeck);
    }
    if r.mono_errors_present {
        return Err(BuildAborted::Mono);
    }
    Ok(())
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
