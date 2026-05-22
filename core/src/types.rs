use crate::version::{Version, VersionSpec};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// A dependency as parsed from a file (generic across ecosystems)
#[derive(Debug, Clone, Serialize)]
pub struct Dependency {
    /// Upstream package name on the registry (crates.io / PyPI / npm).
    /// For renamed/aliased deps this is the real package, not the local key.
    pub name: String,
    /// Version specification as parsed
    #[serde(rename = "spec")]
    pub version_spec: VersionSpec,
    /// Source file this dependency was found in
    pub source_file: PathBuf,
    /// Line number in the source file (1-indexed)
    pub line_number: usize,
    /// Original line text (for updating)
    #[serde(skip_serializing)]
    pub original_line: String,
    /// Local table key when it differs from `name` - i.e. a Cargo.toml
    /// `local_alias = { package = "real-name", ... }` rename. `None` when
    /// the manifest key already matches the upstream name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_key: Option<String>,
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
    /// Publish date (ISO-8601 string) per version `original` string.
    /// Populated by registry clients; may be empty for any version the
    /// registry did not surface a date for.
    pub published_at: HashMap<String, String>,
}

/// Severity of an update
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSeverity {
    Major,
    Minor,
    Patch,
}

/// Result of checking a dependency for updates
#[derive(Debug, Clone, Serialize)]
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
    /// The severity of the update (based on installed -> target)
    pub severity: Option<UpdateSeverity>,
    /// The VersionSpec to write when force updating to latest
    pub force_spec: Option<VersionSpec>,
    /// ISO-8601 publish date of the installed version (registry data)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_released_at: Option<String>,
    /// ISO-8601 publish date of the target version (registry data)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_released_at: Option<String>,
    /// ISO-8601 publish date of the latest version (registry data)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_released_at: Option<String>,
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
