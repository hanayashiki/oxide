//! Linker invocation. The only file in the builder that touches
//! `std::process::Command`. Translates VFS paths to real paths via
//! `host.to_real` immediately before spawning, then hands real
//! `PathBuf`s to `cc` / `link.exe`.

use std::path::{Path, PathBuf};
use std::process::Command;

use target_lexicon::{Environment, Triple};

use crate::config::{CompilerConfig, LinkerChoice};
use crate::loader::BuilderHost;

use super::BuildError;

/// Resolved linker — the program to spawn plus the argv shape it
/// expects.
pub(super) struct Linker {
    pub cmd: PathBuf,
    pub kind: LinkerKind,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub(super) enum LinkerKind {
    /// Cc-style (`cc`, `clang`, `gcc`, MinGW cc): `-o <out> <obj>
    /// -l<lib>... <args>`.
    Cc,
    /// Microsoft `link.exe`: `/OUT:<out> <obj> <lib>.lib... <args>`.
    LinkExe,
}

/// Pick a linker based on the configured override, falling back to the
/// triple's environment. MSVC → `link.exe`; everything else → `cc`,
/// per `spec/06_LLVM_CODEGEN.md:354-359` (delegate to cc so the host's
/// crt0 / libSystem / glibc paths are picked up for free).
pub(super) fn pick_linker(triple: &Triple, choice: &Option<LinkerChoice>) -> Linker {
    match choice {
        Some(LinkerChoice::Cc) => Linker { cmd: "cc".into(), kind: LinkerKind::Cc },
        Some(LinkerChoice::LinkExe) => Linker { cmd: "link.exe".into(), kind: LinkerKind::LinkExe },
        Some(LinkerChoice::Custom(p)) => Linker {
            cmd: p.clone(),
            // Custom binaries follow cc convention by default; users
            // who need link.exe argv shape pass `LinkerChoice::LinkExe`
            // and let us treat the cmd as link.exe verbatim.
            kind: LinkerKind::Cc,
        },
        None => {
            if matches!(triple.environment, Environment::Msvc) {
                Linker { cmd: "link.exe".into(), kind: LinkerKind::LinkExe }
            } else {
                Linker { cmd: "cc".into(), kind: LinkerKind::Cc }
            }
        }
    }
}

/// Build the argv (sans program name) for the chosen linker.
pub(super) fn build_argv(
    linker: &Linker,
    obj_real: &Path,
    exe_real: &Path,
    libs: &[String],
    config_args: &[String],
    extra_args: &[String],
) -> Vec<String> {
    let mut argv = Vec::new();
    match linker.kind {
        LinkerKind::Cc => {
            argv.push("-o".into());
            argv.push(exe_real.display().to_string());
            argv.push(obj_real.display().to_string());
            for lib in libs {
                argv.push(format!("-l{lib}"));
            }
        }
        LinkerKind::LinkExe => {
            argv.push(format!("/OUT:{}", exe_real.display()));
            argv.push(obj_real.display().to_string());
            for lib in libs {
                argv.push(format!("{lib}.lib"));
            }
        }
    }
    argv.extend(config_args.iter().cloned());
    argv.extend(extra_args.iter().cloned());
    argv
}

/// Spawn the linker, capturing stderr. Maps PATH-miss to
/// `LinkerNotFound`, non-zero exit to `LinkerFailed`.
pub(super) fn link_executable(
    host: &dyn BuilderHost,
    config: &CompilerConfig,
    obj_vfs: &Path,
    exe_vfs: &Path,
    extra_args: &[String],
) -> Result<PathBuf, BuildError> {
    let triple = config
        .target_triple
        .clone()
        .unwrap_or_else(host_triple_or_unknown);
    let linker = pick_linker(&triple, &config.linker);

    let obj_real = host
        .to_real(obj_vfs)
        .map_err(|e| BuildError::Io {
            path: obj_vfs.to_path_buf(),
            source: std::io::Error::other(format!("{e:?}")),
        })?;
    let exe_real = host
        .to_real(exe_vfs)
        .map_err(|e| BuildError::Io {
            path: exe_vfs.to_path_buf(),
            source: std::io::Error::other(format!("{e:?}")),
        })?;

    let argv = build_argv(
        &linker,
        &obj_real,
        &exe_real,
        &config.link_libs,
        &config.link_args,
        extra_args,
    );

    let output = Command::new(&linker.cmd).args(&argv).output();
    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(BuildError::LinkerNotFound {
                tried: vec![linker.cmd.display().to_string()],
            });
        }
        Err(e) => {
            return Err(BuildError::Io {
                path: exe_real,
                source: e,
            });
        }
    };

    if !output.status.success() {
        return Err(BuildError::LinkerFailed {
            linker: linker.cmd.display().to_string(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(exe_real)
}

/// Best-effort host triple parse. If `target_lexicon` can't parse the
/// LLVM-format triple string we fall back to `Triple::unknown()` —
/// the linker default-pick still works (any non-MSVC environment maps
/// to `cc`, which is the right answer for the unknown case anyway).
fn host_triple_or_unknown() -> Triple {
    use inkwell::targets::TargetMachine;
    use std::str::FromStr;

    let s = TargetMachine::get_default_triple().as_str().to_string_lossy().into_owned();
    Triple::from_str(&s).unwrap_or_else(|_| Triple::unknown())
}
