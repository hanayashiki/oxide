//! Compiler session — bundles cross-cutting state for a single
//! compilation: the filesystem host, configuration, and source map.
//!
//! Threaded as `&Session` to layers that read state, `&mut Session`
//! during load (since the loader pushes newly-discovered files into
//! the source map). Future cross-cutting concerns (interner,
//! diagnostic sink, feature flags) join the bundle without API churn
//! at every consumer.

use crate::config::CompilerConfig;
use crate::loader::BuilderHost;
use crate::reporter::SourceMap;

pub struct Session<'h> {
    pub host: &'h dyn BuilderHost,
    pub config: CompilerConfig,
    pub source_map: SourceMap,
}

impl<'h> Session<'h> {
    pub fn new(host: &'h dyn BuilderHost, config: CompilerConfig) -> Self {
        Self {
            host,
            config,
            source_map: SourceMap::new(),
        }
    }

    /// Convenience constructor for tests: default `CompilerConfig`,
    /// fresh `SourceMap`.
    pub fn for_test(host: &'h dyn BuilderHost) -> Self {
        Self::new(host, CompilerConfig::default())
    }
}
