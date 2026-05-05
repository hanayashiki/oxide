//! `oxide` — the Oxide compiler driver.
//!
//! ```text
//! oxide main.ox                        # compile + execv (default emit=exe)
//! oxide main.ox --no-run               # compile only; print exe path to stderr
//! oxide main.ox --emit ir              # textual LLVM IR to stdout
//! oxide main.ox --emit obj -o a.o      # object file
//! oxide main.ox -- arg1 arg2           # trailing args pass to the program
//! ```
//!
//! Pipeline stages by `--emit`:
//!   lex / ast    → root file only (no imports — `Builder::from_inline`)
//!   hir / typeck → load_program + lower_program (`Builder::from_root`)
//!   ir / bc / obj → + codegen + builder emit (`Builder::emit_artifact`)
//!   exe          → + link + (execv unless --no-run)
//!
//! Architecture: this driver owns no orchestration. It parses CLI
//! options, hands them to a `CliEmit` tapper that prints + tracks an
//! exit code, and runs them through `oxide::builder::Builder` —
//! exactly the pattern integration tests use.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use oxide::builder::{
    BuildOptions, Builder, EmitKind as BuilderEmitKind, Flow, OutputPath, Phase, TapAst, TapHir,
    TapLex, TapLoad, TapMono, TapTypeck, Tapper,
};
use oxide::config::{CompilerConfig, OptLevel};
use oxide::hir::pretty::pretty_print as hir_pretty;
use oxide::loader::{BuilderHost, VfsHost};
use oxide::parser::pretty::pretty_print as ast_pretty;
use oxide::reporter::{
    emit, from_hir_error, from_lex_error, from_load_error, from_mono_error, from_parse_error,
    from_typeck_error,
};
use oxide::session::Session;

#[derive(Parser, Debug)]
#[command(name = "oxide", version, about = "Oxide compiler driver")]
struct Args {
    /// Source file to compile. The root of the import graph.
    path: PathBuf,

    /// What to emit. Default `exe` produces a linked executable and
    /// runs it via execv unless `--no-run` is set.
    #[arg(long = "emit", value_enum, default_value_t = EmitArg::Exe)]
    emit: EmitArg,

    /// Output path. Defaults to `target/oxide-build/<stem>` so the
    /// produced artifact lands under Cargo's gitignored target dir.
    /// `exe` keeps the bare module name; the intermediate `.o` lives
    /// alongside as `<stem>-<pid>.o` (no collision).
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// LLVM optimization level. Ignored (with a warning) for
    /// `lex/ast/hir/typeck`.
    #[arg(short = 'O', long = "opt-level", value_name = "LEVEL", default_value = "0")]
    opt_level: String,

    /// Build only — don't execv the resulting binary. Prints the
    /// produced path to stderr and exits 0.
    #[arg(long = "no-run")]
    no_run: bool,

    /// Trailing arguments after `--` pass through to the program when
    /// `--emit exe` runs the binary.
    #[arg(last = true)]
    program_args: Vec<String>,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum EmitArg {
    Lex,
    Ast,
    Hir,
    Typeck,
    Ir,
    Bc,
    Obj,
    Exe,
}

impl EmitArg {
    fn is_codegen(self) -> bool {
        matches!(self, Self::Ir | Self::Bc | Self::Obj | Self::Exe)
    }

    /// The phase the Builder needs to drive to. For codegen emits we
    /// only use it as a "halt before running codegen" signal; the
    /// actual driver is `emit_artifact`.
    fn target_phase(self) -> Phase {
        match self {
            Self::Lex => Phase::Lex,
            Self::Ast => Phase::Ast,
            Self::Hir => Phase::Hir,
            Self::Typeck => Phase::Typeck,
            Self::Ir | Self::Bc | Self::Obj | Self::Exe => Phase::Mono,
        }
    }
}

fn parse_opt_level(s: &str) -> Result<OptLevel, String> {
    Ok(match s {
        "0" => OptLevel::None,
        "1" => OptLevel::O1,
        "2" => OptLevel::O2,
        "3" => OptLevel::O3,
        "s" => OptLevel::Os,
        "z" => OptLevel::Oz,
        other => return Err(format!("unknown opt level `{other}` (expected 0/1/2/3/s/z)")),
    })
}

fn main() -> ExitCode {
    let args = Args::parse();

    let opt_level = match parse_opt_level(&args.opt_level) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("oxide: {e}");
            return ExitCode::from(2);
        }
    };

    if !args.emit.is_codegen() && opt_level != OptLevel::None {
        eprintln!("oxide: -O ignored for --emit {:?}", args.emit);
    }

    // Production host: empty mount table, disk fallthrough handles
    // everything. Workdir defaults to `target/oxide-build` for builder
    // intermediates (.o files for Exe).
    let host = VfsHost::new(HashMap::new());
    let color = io::stderr().is_terminal();

    let module_name = args
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "oxide".into());

    let mut emit_st = CliEmit::new(args.emit, color);

    if args.emit.is_codegen() {
        run_codegen(&args, &host, opt_level, &module_name, &mut emit_st)
    } else {
        run_inspect(&args, &host, opt_level, &mut emit_st)
    }
}

/// Drive `--emit lex|ast|hir|typeck` through the Builder. The CliEmit
/// tapper prints the requested artifact (or the diagnostics that
/// blocked it).
fn run_inspect(
    args: &Args,
    host: &VfsHost,
    opt_level: OptLevel,
    emit_st: &mut CliEmit,
) -> ExitCode {
    let target = args.emit.target_phase();
    let config = CompilerConfig {
        opt_level,
        ..CompilerConfig::default()
    };

    {
        let sess = Session::new(host, config);
        match args.emit {
            EmitArg::Lex | EmitArg::Ast => {
                let src = match std::fs::read_to_string(&args.path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("oxide: failed to read {}: {e}", args.path.display());
                        return ExitCode::from(1);
                    }
                };
                let mut b =
                    Builder::from_inline(sess, args.path.clone(), src, emit_st);
                b.run(target);
            }
            _ => {
                let mut b = Builder::from_root(sess, args.path.clone(), emit_st);
                b.run(target);
            }
        }
    }

    if emit_st.had_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Drive `--emit ir|bc|obj|exe` through the Builder. Produces an
/// artifact on disk; for `Exe` either execs it (default) or prints the
/// path (`--no-run`). For `--emit ir` without `-o`, the produced file
/// is read back and printed to stdout to match historical behaviour.
fn run_codegen(
    args: &Args,
    host: &VfsHost,
    opt_level: OptLevel,
    module_name: &str,
    emit_st: &mut CliEmit,
) -> ExitCode {
    let print_ir_to_stdout = args.emit == EmitArg::Ir && args.output.is_none();

    let exe_default_path = || -> PathBuf { host.workdir().join(module_name) };

    let output = match (&args.output, args.emit, print_ir_to_stdout) {
        (Some(p), _, _) => OutputPath::Explicit(p.clone()),
        (None, EmitArg::Ir, true) => {
            OutputPath::Explicit(host.workdir().join(format!("{module_name}.ll")))
        }
        (None, EmitArg::Exe, _) => OutputPath::Explicit(exe_default_path()),
        (None, _, _) => OutputPath::Auto,
    };

    let emit_kind = match args.emit {
        EmitArg::Ir => BuilderEmitKind::Ir,
        EmitArg::Bc => BuilderEmitKind::Bc,
        EmitArg::Obj => BuilderEmitKind::Obj,
        EmitArg::Exe => BuilderEmitKind::Exe,
        _ => unreachable!("non-codegen emits handled in run_inspect"),
    };

    let opts = BuildOptions {
        emit: emit_kind,
        output,
        module_name: module_name.to_string(),
        keep_intermediates: false,
        extra_link_args: Vec::new(),
    };

    let config = CompilerConfig {
        opt_level,
        ..CompilerConfig::default()
    };

    let artifact = {
        let sess = Session::new(host, config);
        let mut b = Builder::from_root(sess, args.path.clone(), emit_st);
        match b.emit_artifact(&opts) {
            Ok(a) => a,
            Err(_) => {
                // CliEmit has already printed phase diagnostics via
                // its tapper callbacks; just propagate the failure.
                return ExitCode::from(1);
            }
        }
    };

    if emit_st.had_errors {
        return ExitCode::from(1);
    }

    if print_ir_to_stdout {
        let real = artifact
            .primary_output_real
            .as_ref()
            .expect("ir output materialized");
        match std::fs::read_to_string(real) {
            Ok(s) => print!("{s}"),
            Err(e) => {
                eprintln!("oxide: read {}: {e}", real.display());
                return ExitCode::from(1);
            }
        }
        return ExitCode::SUCCESS;
    }

    if args.emit == EmitArg::Exe {
        let exe_real = artifact
            .primary_output_real
            .as_ref()
            .expect("exe output materialized")
            .clone();
        if args.no_run {
            eprintln!("oxide: built {}", exe_real.display());
            return ExitCode::SUCCESS;
        }
        return run_exe(&exe_real, &args.program_args);
    }

    ExitCode::SUCCESS
}

/// CLI's `Tapper`. Prints the requested artifact or the diagnostics
/// that prevented it; returns `Flow::Halt` on the first phase with
/// errors so the rest of the pipeline doesn't run on poison input
/// (matching today's "print one phase's diagnostics and exit"
/// behaviour). `had_errors` flips at each error site so `main` can
/// translate to a non-zero exit code after the Builder returns.
struct CliEmit {
    target: EmitArg,
    color: bool,
    had_errors: bool,
}

impl CliEmit {
    fn new(target: EmitArg, color: bool) -> Self {
        Self {
            target,
            color,
            had_errors: false,
        }
    }
}

impl Tapper for CliEmit {
    fn on_lex(&mut self, t: TapLex<'_>) -> Flow {
        // Print only when --emit lex is the target; for higher emits
        // we just observe (lex errors there flow through on_load via
        // load_program's error reporting).
        if self.target != EmitArg::Lex {
            return Flow::Continue;
        }

        let mut out = io::stderr().lock();
        let mut had_lex_error = false;
        let mut printable = Vec::with_capacity(t.tokens.len());
        for tok in t.tokens {
            match &tok.kind {
                oxide::lexer::TokenKind::Error(e) => {
                    let diag = from_lex_error(e, t.root, tok.span.clone());
                    emit(&diag, t.source_map, &mut out, self.color).expect("write stderr");
                    had_lex_error = true;
                }
                oxide::lexer::TokenKind::Eof => {}
                other => printable.push(other.clone()),
            }
        }
        println!("{printable:?}");
        if had_lex_error {
            self.had_errors = true;
            Flow::Halt
        } else {
            Flow::Continue
        }
    }

    fn on_ast(&mut self, t: TapAst<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut out = io::stderr().lock();
            for err in t.errors {
                let diag = from_parse_error(err, t.root);
                emit(&diag, t.source_map, &mut out, self.color).expect("write stderr");
            }
            self.had_errors = true;
            return Flow::Halt;
        }
        if self.target == EmitArg::Ast {
            print!("{}", ast_pretty(t.ast));
        }
        Flow::Continue
    }

    fn on_load(&mut self, t: TapLoad<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut out = io::stderr().lock();
            for err in t.errors {
                for diag in from_load_error(err) {
                    emit(&diag, t.source_map, &mut out, self.color).expect("write stderr");
                }
            }
            self.had_errors = true;
            return Flow::Halt;
        }
        Flow::Continue
    }

    fn on_hir(&mut self, t: TapHir<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut out = io::stderr().lock();
            for err in t.errors {
                let diag = from_hir_error(err);
                emit(&diag, t.source_map, &mut out, self.color).expect("write stderr");
            }
            self.had_errors = true;
            return Flow::Halt;
        }
        if self.target == EmitArg::Hir {
            print!("{}", hir_pretty(t.hir, t.source_map));
        }
        Flow::Continue
    }

    fn on_typeck(&mut self, t: TapTypeck<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut out = io::stderr().lock();
            for err in t.errors {
                let diag = from_typeck_error(err, t.root, &t.results.tys);
                emit(&diag, t.source_map, &mut out, self.color).expect("write stderr");
            }
            self.had_errors = true;
            return Flow::Halt;
        }
        if self.target == EmitArg::Typeck {
            print_typeck(t.hir, t.results);
        }
        Flow::Continue
    }

    fn on_mono(&mut self, t: TapMono<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut out = io::stderr().lock();
            for err in t.errors {
                let diag = from_mono_error(err, t.root, t.hir, &t.results.tys);
                emit(&diag, t.source_map, &mut out, self.color).expect("write stderr");
            }
            self.had_errors = true;
            return Flow::Halt;
        }
        Flow::Continue
    }
}

/// Print typeck signatures + per-expr types in the format the old
/// `oxide-typeck-example` used. Per-expr dump only for fn 0 to avoid
/// ambiguity (we don't track expr → fn ownership precisely).
fn print_typeck(hir: &oxide::hir::HirProgram, results: &oxide::typeck::TypeckResults) {
    for (fid, sig) in results.fn_sigs.iter_enumerated() {
        let f = &hir.fns[fid];
        let params: Vec<String> = f
            .params
            .iter()
            .zip(&sig.params)
            .map(|(&lid, &ty)| {
                format!(
                    "{}[Local({})]: {}",
                    hir.locals[lid].name,
                    lid.index(),
                    results.tys.render(ty)
                )
            })
            .collect();
        println!(
            "Fn[{}] {}({}) -> {}",
            fid.index(),
            f.name,
            params.join(", "),
            results.tys.render(sig.ret),
        );
        for (eid, &ty) in results.expr_tys.iter_enumerated() {
            if fid.index() == 0 {
                println!("  HExprId({}) : {}", eid.index(), results.tys.render(ty));
            }
        }
    }
}

/// Run the produced executable. On Unix, exec replaces the current
/// process so the program's exit code becomes oxide's. On Windows,
/// spawn + wait + propagate the status code (no execv equivalent).
#[cfg(unix)]
fn run_exe(exe: &Path, program_args: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(exe).args(program_args).exec();
    eprintln!("oxide: failed to exec {}: {err}", exe.display());
    ExitCode::from(1)
}

#[cfg(not(unix))]
fn run_exe(exe: &Path, program_args: &[String]) -> ExitCode {
    match std::process::Command::new(exe).args(program_args).status() {
        Ok(status) => match status.code() {
            Some(c) if (0..=255).contains(&c) => ExitCode::from(c as u8),
            Some(_) => ExitCode::from(1),
            None => ExitCode::from(1),
        },
        Err(e) => {
            eprintln!("oxide: failed to spawn {}: {e}", exe.display());
            ExitCode::from(1)
        }
    }
}
