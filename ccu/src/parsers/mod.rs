pub mod cargo_lock;
pub mod cargo_toml;

pub use cargo_lock::CargoLockParser;
pub use cargo_toml::CargoTomlParser;

// Re-export Dependency from core
pub use check_updates_core::Dependency;

use std::path::PathBuf;

/// Trait for dependency file parsers
pub trait DependencyParser {
    /// Parse a file and return all dependencies found
    fn parse(&self, path: &PathBuf) -> anyhow::Result<Vec<Dependency>>;

    /// Check if this parser can handle the given file
    fn can_parse(&self, path: &PathBuf) -> bool;
}
