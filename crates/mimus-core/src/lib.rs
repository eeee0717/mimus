//! `mimus-core` - layout-preserving PDF translation primitives.

pub mod context;
pub mod engine;
pub mod error;
pub mod event;
pub mod il;
pub mod pass;
mod scan;
pub mod translate;
pub mod walk;
pub mod write;

pub use context::{Document, PassContext, PassSnapshotSink, PipelineConfig};
pub use error::{
    AssetReason, ErrorReason, ExitCategory, InputReason, InternalReason, IoReason, MimusError,
    Result, TranslationReason, UsageReason,
};

/// Core version used by the CLI as its version source of truth.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[must_use]
pub fn version() -> &'static str {
    VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_the_compiled_crate_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
