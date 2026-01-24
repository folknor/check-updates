pub mod cli;
pub mod cratesio;
pub mod detector;
pub mod parsers;
pub mod updater;

// Re-export core types for convenience
pub use check_updates_core::{
    Dependency, DependencyCheck, DependencyResolver, PackageInfo, TableRenderer, UpdateSeverity,
    Version, VersionError, VersionSpec,
};
