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
    /// Line number in the source file (1-indexed), when the parser could prove
    /// where the declaration is written.
    ///
    /// `None` means "this dependency is real, but its location in the file is
    /// unknown": a multi-line TOML string, an escaped literal, a value that
    /// only exists after workspace inheritance, or a locator that simply did
    /// not find the text it was looking for. Line-based updaters must treat
    /// `None` as "do not rewrite" - a guessed line is a wrong-line rewrite
    /// waiting to happen, and the wave-1 findings record several of those.
    /// It is never a fabricated `1` or an out-of-range sentinel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<usize>,
    /// Original line text (for updating)
    #[serde(skip_serializing)]
    pub original_line: String,
    /// Local table key when it differs from `name` - i.e. a Cargo.toml
    /// `local_alias = { package = "real-name", ... }` rename. `None` when
    /// the manifest key already matches the upstream name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_key: Option<String>,
    /// Manifest section this dependency was declared in, verbatim as it appears
    /// in the source file: `"dependencies"` / `"devDependencies"` /
    /// `"peerDependencies"` / `"optionalDependencies"` for package.json,
    /// `"dependencies"` / `"dev-dependencies"` / `"build-dependencies"` (with a
    /// `target.<cfg>.` prefix where applicable) for Cargo.toml, and the
    /// requirements file or PEP 621 table for Python.
    ///
    /// Updaters must rewrite only the section a dependency was read from. The
    /// same package routinely appears in several sections at deliberately
    /// different specs (an npm `peerDependencies` range is wide on purpose);
    /// writing the resolved spec into all of them is a semantic change nobody
    /// asked for. `None` means "unknown", and an updater seeing `None` falls
    /// back to the old all-sections behaviour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
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
    /// The latest version this dependency can move to while staying within the
    /// spirit of its constraint.
    ///
    /// For a bounded spec that is the maximum version the spec accepts. For an
    /// unbounded floor (`>=`, `>`) it is the newest release in the major series
    /// the dependency is on, because every published version satisfies such a
    /// spec and the unrestricted answer would just repeat `latest`. See
    /// `DependencyResolver::calculate_in_range` for the full reasoning.
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

    /// Check whether an update run with the given severity filter will actually
    /// rewrite this dependency. Mirrors the filtering in each crate's `FileUpdater`,
    /// so callers can display exactly what gets written.
    ///
    /// - `include_minor`: false = patch only, true = patch + minor
    /// - `force`: true = all severities, written at the absolute latest version
    pub fn will_update(&self, include_minor: bool, force: bool) -> bool {
        let spec = if force {
            self.force_spec.as_ref()
        } else {
            match self.severity {
                Some(UpdateSeverity::Patch) => self.target_spec.as_ref(),
                Some(UpdateSeverity::Minor) if include_minor => self.target_spec.as_ref(),
                _ => None,
            }
        };

        spec.is_some_and(VersionSpec::is_rewritable)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn check(severity: UpdateSeverity) -> DependencyCheck {
        let target = Version::new(7, 0, 0);
        DependencyCheck {
            dependency: Dependency {
                name: "dirs".to_string(),
                version_spec: VersionSpec::Caret(Version::new(6, 0, 0)),
                source_file: PathBuf::from("Cargo.toml"),
                line_number: Some(1),
                original_line: "dirs = \"6.0.0\"".to_string(),
                manifest_key: None,
                section: Some("dependencies".to_string()),
            },
            installed: Some(Version::new(6, 0, 0)),
            in_range: None,
            latest: target.clone(),
            target: Some(target.clone()),
            target_spec: Some(VersionSpec::Caret(target.clone())),
            severity: Some(severity),
            force_spec: Some(VersionSpec::Caret(target)),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        }
    }

    #[test]
    fn major_update_is_skipped_unless_forced() {
        let major = check(UpdateSeverity::Major);
        assert!(!major.will_update(false, false));
        assert!(!major.will_update(true, false));
        assert!(major.will_update(false, true));
    }

    #[test]
    fn minor_update_needs_include_minor() {
        let minor = check(UpdateSeverity::Minor);
        assert!(!minor.will_update(false, false));
        assert!(minor.will_update(true, false));
    }

    #[test]
    fn patch_update_always_applies() {
        let patch = check(UpdateSeverity::Patch);
        assert!(patch.will_update(false, false));
    }

    #[test]
    fn unrewritable_spec_never_updates() {
        let mut major = check(UpdateSeverity::Major);
        major.force_spec = Some(VersionSpec::Any);
        assert!(!major.will_update(true, true));
    }
}
