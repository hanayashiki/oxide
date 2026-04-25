//! Lex, parse, lower to HIR, typecheck, and emit LLVM IR.
//!
//! ```text
//! cargo run --example oxide-codegen-example -- -p 'fn add(a: i32, b: i32) -> i32 { a + b }'
//! cargo run --example oxide-codegen-example -- -f example-projects/basic/add.ox
//! ```

use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};

use oxide::codegen::codegen;
use oxide::hir::lower as hir_lower;
use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::reporter::{SourceMap, emit, from_hir_error, from_parse_error, from_typeck_error};
use oxide::typeck::check;

#[derive(Parser, Debug)]
#[command(name = "oxide-codegen-example", version, about)]
struct Args {
    /// Source string to compile.
    #[arg(short = 'p', long = "parse", conflicts_with = "file")]
    parse: Option<String>,

    /// Source file to compile.
    #[arg(short = 'f', long = "file", conflicts_with = "parse")]
    file: Option<PathBuf>,

    /// Target file to write the generated LLVM IR to. If not provided, the IR will be printed to stdout.
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// LLVM optimization level: 0, 1, 2, 3, s, z. `0` (default) skips
    /// the LLVM pass pipeline entirely.
    #[arg(short = 'O', long = "opt-level", value_name = "LEVEL", default_value = "0")]
    opt_level: String,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let (source, file_label) = match (&args.parse, &args.file) {
        (Some(s), None) => (s.clone(), PathBuf::from("<arg>")),
        (None, Some(p)) => match fs::read_to_string(p) {
            Ok(s) => (s, p.clone()),
            Err(e) => {
                eprintln!("failed to read {}: {}", p.display(), e);
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("provide -p '<source>' or -f <path>");
            return ExitCode::from(2);
        }
    };

    let tokens = lex(&source);
    let (ast, parse_errors) = parse(&tokens);

    let mut map = SourceMap::new();
    let file = map.add(file_label, source.clone());
    let stderr = std::io::stderr();
    let color = stderr.is_terminal();

    if !parse_errors.is_empty() {
        let mut out = stderr.lock();
        for err in &parse_errors {
            let diag = from_parse_error(err, file);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    let (hir, hir_errors) = hir_lower(&ast);
    if !hir_errors.is_empty() {
        let mut out = stderr.lock();
        for err in &hir_errors {
            let diag = from_hir_error(err, file);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    let (results, type_errors) = check(&hir);
    if !type_errors.is_empty() {
        let mut out = stderr.lock();
        for err in &type_errors {
            let diag = from_typeck_error(err, file, &results.tys);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    let ctx = Context::create();
    let module = codegen(&ctx, &hir, &results, "oxide");

    if args.opt_level != "0" {
        if let Err(e) = run_llvm_opt(&module, &args.opt_level) {
            eprintln!("optimization failed: {e}");
            return ExitCode::from(1);
        }
    }

    match &args.output {
        Some(path) => fs::write(path, module.print_to_string().to_string()).unwrap(),
        None => {
            print!("{}", module.print_to_string().to_string());
        }
    }

    ExitCode::SUCCESS
}

/// Run the LLVM new-pass-manager `default<O*>` pipeline at the given level.
/// Caller guarantees `level != "0"`.
fn run_llvm_opt(module: &inkwell::module::Module<'_>, level: &str) -> Result<(), String> {
    let pipeline = match level {
        "1" | "2" | "3" | "s" | "z" => format!("default<O{level}>"),
        other => return Err(format!("unknown opt level `{other}` (expected 0/1/2/3/s/z)")),
    };

    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("native target init failed: {e}"))?;
    let triple = TargetMachine::get_default_triple();
    let target =
        Target::from_triple(&triple).map_err(|e| format!("target lookup failed: {e}"))?;
    let cpu = TargetMachine::get_host_cpu_name().to_string();
    let features = TargetMachine::get_host_cpu_features().to_string();
    let machine = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| "failed to create TargetMachine".to_string())?;

    module
        .run_passes(&pipeline, &machine, PassBuilderOptions::create())
        .map_err(|e| e.to_string())
}
