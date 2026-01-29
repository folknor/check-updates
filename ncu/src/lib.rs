pub mod cli;
pub mod detector;
pub mod npm;
pub mod parsers;
pub mod updater;

pub use check_updates_core::{
    Dependency, DependencyCheck, DependencyResolver, PackageInfo, TableRenderer, UpdateSeverity,
    Version, VersionError, VersionSpec,
};
