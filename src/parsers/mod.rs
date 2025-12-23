pub mod conda;
pub mod lockfiles;
pub mod pyproject;
pub mod requirements;

pub use conda::CondaParser;
pub use lockfiles::LockfileParser;
pub use pyproject::PyProjectParser;
pub use requirements::RequirementsParser;

use crate::version::VersionSpec;
use std::path::PathBuf;

/// A dependency as parsed from a file
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Package name (normalized to lowercase)
    pub name: String,
    /// Version specification as parsed
    pub version_spec: VersionSpec,
    /// Source file this dependency was found in
    pub source_file: PathBuf,
    /// Line number in the source file (1-indexed)
    pub line_number: usize,
    /// Original line text (for updating)
    pub original_line: String,
}

/// Trait for dependency file parsers
pub trait DependencyParser {
    /// Parse a file and return all dependencies found
    fn parse(&self, path: &PathBuf) -> anyhow::Result<Vec<Dependency>>;

    /// Check if this parser can handle the given file
    fn can_parse(&self, path: &PathBuf) -> bool;
}
