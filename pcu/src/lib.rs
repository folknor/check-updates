pub mod cli;
pub mod detector;
pub mod global;
pub mod output;
pub mod parsers;
pub mod pypi;
pub mod python;
pub mod updater;
pub mod uv_python;

// Re-export core types for convenience
pub use check_updates_core::{
    Dependency, DependencyCheck, DependencyResolver, PackageInfo, TableRenderer, UpdateSeverity,
    Version, VersionError, VersionSpec,
};
