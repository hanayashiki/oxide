//! Compiler configuration. Plain data, no behavior.
//!
//! `CompilerConfig` is read from a project-level config file (e.g.
//! `oxide.toml`) by the driver. Layers that need configuration take a
//! `&CompilerConfig` parameter directly. The struct is serde-ready
//! behind the optional `serde` feature.
//!
//! Today the only field is `target_triple`; codegen will start
//! consuming it in a follow-up. New fields land here as the compiler
//! grows configuration surface.

use target_lexicon::Triple;

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
pub struct CompilerConfig {
    /// LLVM target triple. `None` ⇒ codegen falls back to the host
    /// triple via `TargetMachine::get_default_triple()`. Validated
    /// at config-load time so typos surface as a parse error rather
    /// than a cryptic LLVM crash later.
    pub target_triple: Option<Triple>,
}
