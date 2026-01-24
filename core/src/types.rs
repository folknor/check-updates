use crate::version::{Version, VersionSpec};
use std::path::PathBuf;

/// A dependency as parsed from a file (generic across ecosystems)
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Package name (normalized)
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

/// Package information from a registry (generic across ecosystems)
#[derive(Debug, Clone)]
pub struct PackageInfo {
    /// Package name
    pub name: String,
    /// All available versions (sorted ascending)
    pub versions: Vec<Version>,
    /// Latest version (may include pre-releases based on settings)
    pub latest: Version,
    /// Latest stable version (no pre-release)
    pub latest_stable: Option<Version>,
}

/// Severity of an update
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateSeverity {
    Major,
    Minor,
    Patch,
}

/// Result of checking a dependency for updates
#[derive(Debug, Clone)]
pub struct DependencyCheck {
    /// The original dependency
    pub dependency: Dependency,
    /// Currently installed version (from lock file)
    pub installed: Option<Version>,
    /// Latest version within the constraint
    pub in_range: Option<Version>,
    /// Absolute latest version
    pub latest: Version,
    /// The target version for display (in_range if available, else latest)
    pub target: Option<Version>,
    /// The VersionSpec to write when updating to target
    pub target_spec: Option<VersionSpec>,
    /// The severity of the update (based on installed → target)
    pub severity: Option<UpdateSeverity>,
    /// The VersionSpec to write when force updating to latest
    pub force_spec: Option<VersionSpec>,
}

impl DependencyCheck {
    /// Check if this dependency has any update available
    pub fn has_update(&self) -> bool {
        self.target.is_some()
    }

    /// Check if there's a newer version available beyond the target
    pub fn has_newer_available(&self) -> bool {
        match &self.target {
            Some(target) => self.latest > *target,
            None => false,
        }
    }

    /// Get the current version (installed or from spec)
    pub fn current_version(&self) -> Option<&Version> {
        self.installed
            .as_ref()
            .or_else(|| self.dependency.version_spec.base_version())
    }
}
