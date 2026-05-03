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
//!   lex / ast    → root file only (no imports)
//!   hir / typeck → load_program + lower_program
//!   ir / bc / obj → + codegen + builder emit
//!   exe          → + link + (execv unless --no-run)

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use oxide::builder::{BuildOptions, EmitKind as BuilderEmitKind, OutputPath, build};
use oxide::config::{CompilerConfig, OptLevel};
use oxide::hir::lower_program;
use oxide::hir::pretty::pretty_print as hir_pretty;
use oxide::lexer::lex;
use oxide::loader::{BuilderHost, VfsHost, load_program};
use oxide::parser::parse;
use oxide::parser::pretty::pretty_print as ast_pretty;
use oxide::reporter::{
    SourceMap, emit, from_hir_error, from_lex_error, from_load_error, from_parse_error,
    from_typeck_error,
};
use oxide::session::Session;
use oxide::typeck::check;

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
    let stderr = std::io::stderr();
    let color = stderr.is_terminal();

    let exit = match args.emit {
        EmitArg::Lex => emit_lex(&args.path),
        EmitArg::Ast => emit_ast(&args.path),
        EmitArg::Hir | EmitArg::Typeck | EmitArg::Ir | EmitArg::Bc | EmitArg::Obj | EmitArg::Exe => {
            run_pipeline(&args, opt_level, &host, color)
        }
    };

    exit
}

/// Single-file lex emit: read root, lex, dump tokens (Eof filtered).
fn emit_lex(path: &Path) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("oxide: failed to read {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    let mut map = SourceMap::new();
    let file = map.add(path.to_path_buf(), src.clone());
    let tokens = lex(&src, file);

    let stderr = std::io::stderr();
    let color = stderr.is_terminal();
    let mut had_error = false;
    let mut printable = Vec::with_capacity(tokens.len());
    for t in &tokens {
        match &t.kind {
            oxide::lexer::TokenKind::Error(e) => {
                let diag = from_lex_error(e, file, t.span.clone());
                emit(&diag, &map, &mut std::io::stderr().lock(), color)
                    .expect("write stderr");
                had_error = true;
            }
            oxide::lexer::TokenKind::Eof => {}
            other => printable.push(other.clone()),
        }
    }
    println!("{printable:?}");
    if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Single-file AST emit: read root, lex, parse, pretty-print.
fn emit_ast(path: &Path) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("oxide: failed to read {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    let mut map = SourceMap::new();
    let file = map.add(path.to_path_buf(), src.clone());
    let tokens = lex(&src, file);
    let (module, parse_errs) = parse(&tokens, file);

    let stderr = std::io::stderr();
    let color = stderr.is_terminal();
    if !parse_errs.is_empty() {
        let mut out = stderr.lock();
        for err in &parse_errs {
            let diag = from_parse_error(err, file);
            emit(&diag, &map, &mut out, color).expect("write stderr");
        }
        return ExitCode::from(1);
    }

    print!("{}", ast_pretty(&module));
    ExitCode::SUCCESS
}

/// Full-pipeline path: load_program → lower_program → typeck →
/// (codegen + emit) → (link + run) depending on emit kind.
fn run_pipeline(
    args: &Args,
    opt_level: OptLevel,
    host: &VfsHost,
    color: bool,
) -> ExitCode {
    let mut map = SourceMap::new();
    let (files, root, load_errs) = load_program(host, &mut map, args.path.clone());

    if !load_errs.is_empty() {
        let mut out = std::io::stderr().lock();
        for err in &load_errs {
            for diag in from_load_error(err) {
                emit(&diag, &map, &mut out, color).expect("write stderr");
            }
        }
        return ExitCode::from(1);
    }

    let (hir, hir_errs) = lower_program(files, root);
    if !hir_errs.is_empty() {
        let mut out = std::io::stderr().lock();
        for err in &hir_errs {
            let diag = from_hir_error(err);
            emit(&diag, &map, &mut out, color).expect("write stderr");
        }
        return ExitCode::from(1);
    }

    if args.emit == EmitArg::Hir {
        print!("{}", hir_pretty(&hir, &map));
        return ExitCode::SUCCESS;
    }

    let (results, type_errs) = check(&hir);
    if !type_errs.is_empty() {
        let mut out = std::io::stderr().lock();
        for err in &type_errs {
            let diag = from_typeck_error(err, root, &results.tys);
            emit(&diag, &map, &mut out, color).expect("write stderr");
        }
        return ExitCode::from(1);
    }

    if args.emit == EmitArg::Typeck {
        print_typeck(&hir, &results);
        return ExitCode::SUCCESS;
    }

    // Codegen / builder.
    let module_name = args
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "oxide".into());

    let config = CompilerConfig {
        opt_level,
        ..CompilerConfig::default()
    };
    let sess = Session::new(host, config);

    let emit_kind = match args.emit {
        EmitArg::Ir => BuilderEmitKind::Ir,
        EmitArg::Bc => BuilderEmitKind::Bc,
        EmitArg::Obj => BuilderEmitKind::Obj,
        EmitArg::Exe => BuilderEmitKind::Exe,
        _ => unreachable!("non-builder emits handled above"),
    };

    let print_ir_to_stdout = matches!(args.emit, EmitArg::Ir) && args.output.is_none();

    // Default artifact path goes under `host.workdir()` (= `target/oxide-build/`)
    // so that produced binaries land in `target/` and inherit Cargo's gitignore.
    // Exe gets the bare module name (no extension on Unix); the intermediate `.o`
    // for Exe lives alongside as `<name>-<pid>.o`, so no collision.
    let exe_default_path = || -> PathBuf { host.workdir().join(&module_name) };

    let output = match (&args.output, args.emit, print_ir_to_stdout) {
        (Some(p), _, _) => OutputPath::Explicit(p.clone()),
        (None, EmitArg::Ir, true) => OutputPath::Explicit(
            host.workdir().join(format!("{module_name}.ll")),
        ),
        (None, EmitArg::Exe, _) => OutputPath::Explicit(exe_default_path()),
        (None, _, _) => OutputPath::Auto,
    };

    let opts = BuildOptions {
        emit: emit_kind,
        output,
        module_name,
        keep_intermediates: false,
        extra_link_args: Vec::new(),
    };

    let artifact = match build(&sess, &hir, &results, &opts) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("oxide: build failed: {e}");
            return ExitCode::from(1);
        }
    };

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
