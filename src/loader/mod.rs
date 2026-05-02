//! Loader — discovers reachable `.ox` files starting from a root,
//! parses each, and feeds the set to HIR lowering.
//!
//! `load_program` lands in Step 5 of the module-system rollout. This
//! module currently just re-exports the host abstraction.

pub mod host;

pub use host::{BuilderHost, ResolveError, VfsHost};
