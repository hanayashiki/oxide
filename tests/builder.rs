//! End-to-end tests for `oxide::builder`.
//!
//! Each test drives lex → parse → HIR lower → typeck → builder build,
//! using a `VfsHost` with a per-test workdir under
//! `target/test-artifacts/builder/<test_name>/`. Object files and
//! executables land on real disk via the host's `to_real` boundary;
//! `cargo clean` reclaims them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use inkwell::context::Context;

use oxide::builder::{
    BuildArtifact, BuildError, BuildOptions, EmitKind, OutputPath, build,
};
use oxide::codegen::codegen;
use oxide::config::{CompilerConfig, LinkerChoice};
use oxide::hir::lower;
use oxide::lexer::lex;
use oxide::loader::VfsHost;
use oxide::parser::parse;
use oxide::reporter::SourceMap;
use oxide::session::Session;
use oxide::typeck::check;

const FIXTURE_RETURN_42: &str = "fn main() -> i32 { 42 }";

/// Per-test workdir under `target/test-artifacts/builder/<name>/`.
/// Created fresh on entry so old runs don't pollute the assertions.
fn workdir_for(name: &str) -> PathBuf {
    let dir = PathBuf::from("target/test-artifacts/builder").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir {}: {e}", dir.display()));
    dir
}

fn host_with_workdir(workdir: PathBuf) -> VfsHost {
    VfsHost::new(HashMap::new()).with_workdir(workdir)
}

/// Lower `src` through to HIR + TypeckResults, panicking on any
/// error (test fixtures must compile clean).
fn drive_pipeline(
    src: &str,
) -> (
    SourceMap,
    oxide::hir::HirProgram,
    oxide::typeck::TypeckResults,
) {
    let mut map = SourceMap::new();
    let file = map.add(PathBuf::from("<test>"), src.to_string());
    let tokens = lex(src, file);
    let (ast, parse_errs) = parse(&tokens, file);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:#?}");
    let (hir, hir_errs) = lower(&ast);
    assert!(hir_errs.is_empty(), "hir errors: {hir_errs:#?}");
    let (typeck, type_errs) = check(&hir);
    assert!(type_errs.is_empty(), "type errors: {type_errs:#?}");
    (map, hir, typeck)
}

fn run_build(
    src: &str,
    workdir: PathBuf,
    config: CompilerConfig,
    opts: BuildOptions,
) -> Result<BuildArtifact, BuildError> {
    let (_map, hir, typeck) = drive_pipeline(src);
    let host = host_with_workdir(workdir);
    let sess = Session::new(&host, config);
    build(&sess, &hir, &typeck, &opts)
}

#[test]
fn exe_returns_42() {
    let workdir = workdir_for("exe_returns_42");
    let config = CompilerConfig::default();
    let opts = BuildOptions::new("exe_returns_42");

    let artifact = run_build(FIXTURE_RETURN_42, workdir, config, opts).expect("build failed");
    assert_eq!(artifact.kind, EmitKind::Exe);

    let exe = artifact
        .primary_output_real
        .as_ref()
        .expect("real exe path missing");
    let status = Command::new(exe)
        .status()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    assert_eq!(status.code(), Some(42), "exit code mismatch: {status:?}");
}

#[test]
fn obj_smoke() {
    let workdir = workdir_for("obj_smoke");
    let config = CompilerConfig::default();
    let opts = BuildOptions {
        emit: EmitKind::Obj,
        output: OutputPath::Auto,
        module_name: "obj_smoke".into(),
        keep_intermediates: false,
        extra_link_args: Vec::new(),
    };

    let artifact = run_build(FIXTURE_RETURN_42, workdir, config, opts).expect("build failed");
    assert_eq!(artifact.kind, EmitKind::Obj);

    let obj = artifact
        .primary_output_real
        .as_ref()
        .expect("real obj path missing");
    let bytes = std::fs::read(obj).unwrap_or_else(|e| panic!("read {}: {e}", obj.display()));
    assert!(bytes.len() > 4, "obj suspiciously small: {} bytes", bytes.len());
    assert!(
        is_known_object_magic(&bytes),
        "unrecognized object magic: {:02x?}",
        &bytes[..bytes.len().min(8)]
    );
}

/// Mach-O (macOS), ELF (Linux), or PE/COFF (Windows) magic. The host
/// triple selects which we'll see; we accept any that's plausibly a
/// native object.
fn is_known_object_magic(b: &[u8]) -> bool {
    if b.len() < 4 {
        return false;
    }
    // Mach-O 64-bit (BE/LE), 32-bit (BE/LE)
    let macho = matches!(
        &b[..4],
        [0xfe, 0xed, 0xfa, 0xce] | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf] | [0xcf, 0xfa, 0xed, 0xfe]
    );
    // ELF
    let elf = &b[..4] == b"\x7fELF";
    // COFF — anonymous object header on Windows starts with machine
    // type bytes; the simplest sniff is "is not Mach-O / ELF and is
    // > 20 bytes" but to keep this strict we look for common machine
    // types (IMAGE_FILE_MACHINE_AMD64=0x8664, I386=0x14c, ARM64=0xaa64).
    let coff = matches!(&b[..2], [0x64, 0x86] | [0x4c, 0x01] | [0x64, 0xaa]);
    macho || elf || coff
}

#[test]
fn ir_matches_codegen_only() {
    // EmitKind::Ir at OptLevel::None must equal codegen(...).print_to_string()
    // byte-for-byte (modulo the trailing target-stamping that emit-time adds).
    // We compare ignoring the lines that the builder injects (target triple
    // + data layout) since those aren't present in the bare codegen output.
    let workdir = workdir_for("ir_matches_codegen_only");
    let config = CompilerConfig::default();
    let opts = BuildOptions {
        emit: EmitKind::Ir,
        output: OutputPath::Explicit(workdir.join("out.ll")),
        module_name: "ir_matches_codegen_only".into(),
        keep_intermediates: false,
        extra_link_args: Vec::new(),
    };

    let artifact = run_build(FIXTURE_RETURN_42, workdir.clone(), config, opts).expect("build failed");
    let ir = std::fs::read_to_string(artifact.primary_output_real.unwrap()).unwrap();

    // Sanity: the builder-emitted IR contains the function body.
    assert!(ir.contains("define"), "missing define: {ir}");
    assert!(ir.contains("ret i32 42"), "missing 42 return: {ir}");

    // Sanity: target triple was stamped onto the module.
    assert!(
        ir.contains("target triple"),
        "expected `target triple` line in builder IR, got:\n{ir}"
    );

    // The builder's IR should be identical to direct codegen except for
    // the stamped target lines. Strip them and compare bodies.
    let (_map, hir, typeck) = drive_pipeline(FIXTURE_RETURN_42);
    let ctx = Context::create();
    let raw_module = codegen(&ctx, &hir, &typeck, "ir_matches_codegen_only");
    let raw_ir = raw_module.print_to_string().to_string();

    let strip_target = |s: &str| -> String {
        s.lines()
            .filter(|l| !l.starts_with("target triple") && !l.starts_with("target datalayout"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        strip_target(&ir),
        strip_target(&raw_ir),
        "builder IR diverged from raw codegen IR (target lines stripped)"
    );
}

#[test]
fn linker_not_found() {
    let workdir = workdir_for("linker_not_found");
    let config = CompilerConfig {
        linker: Some(LinkerChoice::Custom(PathBuf::from(
            "/nonexistent/oxide-test-cc",
        ))),
        ..CompilerConfig::default()
    };
    let opts = BuildOptions::new("linker_not_found");

    let err = run_build(FIXTURE_RETURN_42, workdir, config, opts)
        .expect_err("expected LinkerNotFound, got Ok");
    assert!(
        matches!(err, BuildError::LinkerNotFound { .. }),
        "expected LinkerNotFound, got {err:?}"
    );
}

#[test]
fn linker_failed_on_undefined_symbol() {
    // Reference an extern that no library will provide; cc will exit
    // non-zero with an "undefined symbol" message. Catches the
    // stderr-pipe contract.
    let src = r#"
        extern "C" { fn oxide_test_definitely_not_a_real_symbol() -> i32; }
        fn main() -> i32 { oxide_test_definitely_not_a_real_symbol() }
    "#;
    let workdir = workdir_for("linker_failed_on_undefined_symbol");
    let config = CompilerConfig::default();
    let opts = BuildOptions::new("linker_failed_on_undefined_symbol");

    let err = run_build(src, workdir, config, opts)
        .expect_err("expected LinkerFailed, got Ok");

    let BuildError::LinkerFailed { stderr, .. } = err else {
        panic!("expected LinkerFailed, got {err:?}");
    };
    assert!(
        stderr.contains("oxide_test_definitely_not_a_real_symbol"),
        "linker stderr did not mention the undefined symbol; got:\n{stderr}"
    );
}

#[test]
fn aot_matches_jit() {
    // The AOT exit code must match the JIT return value for the same
    // fixture. One fixture only — broader cross-check is JIT's job.
    let workdir = workdir_for("aot_matches_jit");
    let config = CompilerConfig::default();
    let opts = BuildOptions::new("aot_matches_jit");

    let artifact = run_build(FIXTURE_RETURN_42, workdir, config, opts).expect("build failed");
    let exe = artifact.primary_output_real.expect("real exe path missing");
    let status = Command::new(&exe).status().expect("spawn aot exe");
    let aot_code = status.code().expect("no exit code") as u32;

    let jit_code: i32 = unsafe { jit_return::<i32>(FIXTURE_RETURN_42, "main") };

    assert_eq!(
        aot_code as i32, jit_code,
        "AOT exit code {aot_code} != JIT return {jit_code}"
    );
}

/// Local, minimal JIT runner. We don't depend on `tests/common` here
/// because the builder integration test stands alone.
unsafe fn jit_return<R: Copy + 'static>(src: &str, entry: &str) -> R {
    use inkwell::OptimizationLevel;
    use inkwell::execution_engine::JitFunction;

    let (_map, hir, typeck) = drive_pipeline(src);
    let ctx = Context::create();
    let module = codegen(&ctx, &hir, &typeck, "jit");
    let ee = module
        .create_jit_execution_engine(OptimizationLevel::None)
        .expect("create JIT execution engine");
    let func: JitFunction<'_, unsafe extern "C" fn() -> R> = unsafe {
        ee.get_function(entry)
            .unwrap_or_else(|e| panic!("get_function({entry}): {e:?}"))
    };
    unsafe { func.call() }
}

#[allow(dead_code)]
fn _suppress_path_unused_warning(_p: &Path) {}
