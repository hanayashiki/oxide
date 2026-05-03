//! Target-machine setup and the LLVM optimization pipeline.
//!
//! `resolve` produces a configured `inkwell::TargetMachine` from the
//! session's `CompilerConfig`. `run_opt_passes` runs the new
//! pass-manager `default<O*>` pipeline against a verified module.
//! Both are pure-LLVM concerns; nothing here touches the filesystem
//! or the `BuilderHost`.

use inkwell::OptimizationLevel;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine,
};

use crate::config::{CompilerConfig, OptLevel};

use super::BuildError;

/// Initialize the native target backend and build a `TargetMachine`
/// for the configured triple. Falls back to the host triple when
/// `config.target_triple` is `None`.
pub(super) fn resolve(config: &CompilerConfig) -> Result<TargetMachine, BuildError> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| BuildError::Target(format!("native target init failed: {e}")))?;

    let triple = config
        .target_triple
        .as_ref()
        .map(|t| inkwell::targets::TargetTriple::create(&t.to_string()))
        .unwrap_or_else(TargetMachine::get_default_triple);

    let target = Target::from_triple(&triple)
        .map_err(|e| BuildError::Target(format!("Target::from_triple({triple}) failed: {e}")))?;

    // CPU/features default to the host when we're targeting the host
    // triple. For cross-compilation the user must pass `--target-cpu`
    // / features through `link_args` (out of scope for v0).
    let cpu = TargetMachine::get_host_cpu_name().to_string();
    let features = TargetMachine::get_host_cpu_features().to_string();

    let machine = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            inkwell_opt_level(config.opt_level),
            // PIC is the modern default for executables on macOS/Linux
            // (PIE-ready). Windows MSVC ignores RelocMode for the most
            // part; static is the historical default but PIC is fine.
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| BuildError::Target("create_target_machine returned None".into()))?;

    Ok(machine)
}

/// Stamp the LLVM module with the target's triple and data layout.
/// Required before `TargetMachine::write_to_file` — without these
/// LLVM warns and may emit code that doesn't match the target ABI.
pub(super) fn stamp_module(module: &Module<'_>, machine: &TargetMachine) {
    module.set_triple(&machine.get_triple());
    module.set_data_layout(&machine.get_target_data().get_data_layout());
}

/// Run the new pass-manager `default<O*>` pipeline at the configured
/// level. Skipped at `OptLevel::None` (caller checks).
pub(super) fn run_opt_passes(
    module: &Module<'_>,
    machine: &TargetMachine,
    level: OptLevel,
) -> Result<(), BuildError> {
    let pipeline = pipeline_string(level);
    module
        .run_passes(&pipeline, machine, PassBuilderOptions::create())
        .map_err(|e| BuildError::OptPipeline(e.to_string()))
}

/// Map oxide's serde-friendly `OptLevel` to inkwell's enum.
fn inkwell_opt_level(level: OptLevel) -> OptimizationLevel {
    // inkwell's enum has only None/Less/Default/Aggressive; we collapse
    // the size variants (Os/Oz) to Default for `create_target_machine`
    // — the actual O*s/O*z behavior is selected by the pipeline string
    // in `run_opt_passes`. The TargetMachine-level setting only
    // affects backend codegen, not the IR optimizer.
    match level {
        OptLevel::None => OptimizationLevel::None,
        OptLevel::O1 => OptimizationLevel::Less,
        OptLevel::O2 | OptLevel::Os | OptLevel::Oz => OptimizationLevel::Default,
        OptLevel::O3 => OptimizationLevel::Aggressive,
    }
}

fn pipeline_string(level: OptLevel) -> String {
    match level {
        OptLevel::None => String::new(), // caller skips
        OptLevel::O1 => "default<O1>".into(),
        OptLevel::O2 => "default<O2>".into(),
        OptLevel::O3 => "default<O3>".into(),
        OptLevel::Os => "default<Os>".into(),
        OptLevel::Oz => "default<Oz>".into(),
    }
}
