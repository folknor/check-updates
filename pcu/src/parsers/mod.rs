pub mod conda;
pub mod lockfiles;
pub mod pyproject;
pub mod requirements;

pub use conda::CondaParser;
pub use lockfiles::LockfileParser;
pub use pyproject::PyProjectParser;
pub use requirements::RequirementsParser;

// Re-export Dependency from core for use by parsers
pub use check_updates_core::Dependency;

use std::path::PathBuf;

/// Trait for dependency file parsers
pub trait DependencyParser {
    /// Parse a file and return all dependencies found
    fn parse(&self, path: &PathBuf) -> anyhow::Result<Vec<Dependency>>;

    /// Check if this parser can handle the given file
    fn can_parse(&self, path: &PathBuf) -> bool;
}
