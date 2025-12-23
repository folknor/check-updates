use crate::parsers::Dependency;
use crate::pypi::PackageInfo;
use crate::version::{Version, VersionSpec};
use crate::cli::Args;

/// Result of checking a dependency
#[derive(Debug, Clone)]
pub struct DependencyCheck {
    /// The original dependency
    pub dependency: Dependency,
    /// Currently installed version (from lock file)
    pub installed: Option<Version>,
    /// Latest version within the constraint (same major)
    pub in_range: Option<Version>,
    /// Absolute latest version
    pub latest: Version,
    /// What the spec will be updated to (None means no update)
    pub update_to: Option<VersionSpec>,
}

impl DependencyCheck {
    /// Check if this dependency has any update available
    pub fn has_update(&self) -> bool {
        self.update_to.is_some()
    }

    /// Get the update severity (major, minor, patch)
    pub fn update_severity(&self) -> Option<UpdateSeverity> {
        let current = self.installed.as_ref().or(self.dependency.version_spec.base_version())?;
        let target = self.update_to.as_ref()?.base_version()?;

        if target.major > current.major {
            Some(UpdateSeverity::Major)
        } else if target.minor > current.minor {
            Some(UpdateSeverity::Minor)
        } else if target.patch > current.patch {
            Some(UpdateSeverity::Patch)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateSeverity {
    Major,
    Minor,
    Patch,
}

/// Resolves dependencies and determines what updates are available
pub struct DependencyResolver {
    args: Args,
}

impl DependencyResolver {
    pub fn new(args: Args) -> Self {
        Self { args }
    }

    /// Resolve a single dependency
    pub fn resolve(
        &self,
        dependency: &Dependency,
        package_info: &PackageInfo,
        installed: Option<&Version>,
    ) -> DependencyCheck {
        let latest = package_info.latest.clone();

        // Calculate "in range" - latest version that satisfies the constraint
        // For unbounded specs, this means same major version (considering installed version)
        let in_range = self.calculate_in_range(
            &dependency.version_spec,
            &package_info.versions,
            installed,
        );

        // Calculate latest version with same major (for -m flag)
        let latest_same_major = self.calculate_latest_same_major(
            &dependency.version_spec,
            &package_info.versions,
            installed,
        );

        // Calculate what version spec to update to
        let update_to = self.calculate_update_to(
            &dependency.version_spec,
            &in_range,
            &latest,
            &latest_same_major,
            installed,
        );

        DependencyCheck {
            dependency: dependency.clone(),
            installed: installed.cloned(),
            in_range,
            latest,
            update_to,
        }
    }

    /// Calculate the latest version with the same major version
    /// For unbounded specs, considers the installed version's major if higher
    fn calculate_latest_same_major(
        &self,
        spec: &VersionSpec,
        available_versions: &[Version],
        installed: Option<&Version>,
    ) -> Option<Version> {
        let base = spec.base_version()?;

        // For unbounded specs, use the installed version's major if it's higher
        let target_major = match spec {
            VersionSpec::Minimum(_) | VersionSpec::GreaterThan(_) => {
                if let Some(inst) = installed {
                    base.major.max(inst.major)
                } else {
                    base.major
                }
            }
            _ => base.major,
        };

        available_versions
            .iter()
            .filter(|v| v.major == target_major)
            .max()
            .cloned()
    }

    /// Calculate the latest version "in range" for the constraint
    /// For unbounded specs (>=X.Y.Z), considers installed version to determine target major
    fn calculate_in_range(
        &self,
        spec: &VersionSpec,
        available_versions: &[Version],
        installed: Option<&Version>,
    ) -> Option<Version> {
        // Filter versions based on the spec's constraints
        let max_major = spec.max_major();

        available_versions
            .iter()
            .filter(|v| {
                // First check if it satisfies the spec
                if !spec.satisfies(v) {
                    return false;
                }

                // For unbounded specs (Minimum), limit to same major version
                // but consider installed version if it's on a higher major
                match spec {
                    VersionSpec::Minimum(base) | VersionSpec::GreaterThan(base) => {
                        // If installed version exists and is on a higher major,
                        // use that major for "in range" calculation
                        let target_major = if let Some(inst) = installed {
                            base.major.max(inst.major)
                        } else {
                            base.major
                        };
                        v.major == target_major
                    }
                    _ => {
                        // For other specs, check against max_major if available
                        if let Some(max_maj) = max_major {
                            v.major <= max_maj
                        } else {
                            true
                        }
                    }
                }
            })
            .max()
            .cloned()
    }

    /// Calculate what the version spec should be updated to
    fn calculate_update_to(
        &self,
        current_spec: &VersionSpec,
        in_range: &Option<Version>,
        latest: &Version,
        latest_same_major: &Option<Version>,
        installed: Option<&Version>,
    ) -> Option<VersionSpec> {
        // Handle pinned versions specially
        if let VersionSpec::Pinned(current_version) = current_spec {
            if self.args.force_latest {
                // -f flag: update to absolute latest
                if current_version != latest {
                    return Some(VersionSpec::Pinned(latest.clone()));
                }
                return None;
            } else if self.args.minor {
                // -m flag: update to latest same major
                if let Some(target) = latest_same_major {
                    if current_version != target {
                        return Some(VersionSpec::Pinned(target.clone()));
                    }
                }
                return None;
            } else {
                // Default mode: pinned versions don't update
                return None;
            }
        }

        // For non-pinned specs
        let target_version = if self.args.force_latest {
            // -f flag: everything updates to absolute latest
            latest
        } else {
            // Default/-m mode: update to in_range
            in_range.as_ref()?
        };

        // Check if we need to update
        let needs_update = if let Some(base) = current_spec.base_version() {
            base != target_version
        } else {
            // Can't determine, assume update needed
            true
        };

        if !needs_update {
            return None;
        }

        // Don't suggest updates that would be downgrades from installed version
        if let Some(inst) = installed {
            if target_version < inst {
                return None;
            }
        }

        Some(current_spec.with_version(target_version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Args;
    use crate::parsers::Dependency;
    use crate::version::{Version, VersionSpec};
    use std::path::PathBuf;
    use std::str::FromStr;

    fn create_test_dependency(name: &str, spec_str: &str) -> Dependency {
        Dependency {
            name: name.to_string(),
            version_spec: VersionSpec::parse(spec_str).unwrap(),
            source_file: PathBuf::from("test.txt"),
            line_number: 1,
            original_line: format!("{}=={}", name, spec_str),
        }
    }

    fn create_package_info(name: &str, versions: Vec<&str>) -> PackageInfo {
        let version_objects: Vec<Version> = versions
            .iter()
            .map(|v| Version::from_str(v).unwrap())
            .collect();
        let latest = version_objects.last().unwrap().clone();

        PackageInfo {
            name: name.to_string(),
            versions: version_objects,
            latest: latest.clone(),
            latest_stable: Some(latest),
        }
    }

    #[test]
    fn test_default_mode_pinned_no_update() {
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("requests", "==2.28.0");
        let pkg_info = create_package_info("requests", vec!["2.28.0", "2.32.3", "3.1.0"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        // Default mode: pinned versions don't update
        assert!(result.update_to.is_none());
    }

    #[test]
    fn test_default_mode_range_updates_in_range() {
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("requests", ">=2.28.0,<3.0.0");
        let pkg_info = create_package_info("requests", vec!["2.28.0", "2.32.3", "3.1.0"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        // Should update to latest in range (2.32.3)
        assert!(result.update_to.is_some());
        assert_eq!(result.in_range.unwrap().to_string(), "2.32.3");
    }

    #[test]
    fn test_default_mode_unbounded_updates_same_major() {
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("requests", ">=2.28.0");
        let pkg_info = create_package_info("requests", vec!["2.28.0", "2.32.3", "3.1.0"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        // In range should be 2.32.3 (latest same major)
        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "2.32.3");
        // Should update to >=2.32.3
        assert!(result.update_to.is_some());
    }

    #[test]
    fn test_minor_flag_pinned_updates_same_major() {
        let args = Args {
            path: None,
            update: false,
            minor: true,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("numpy", "==1.24.0");
        let pkg_info = create_package_info("numpy", vec!["1.24.0", "1.26.0", "2.1.0"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        // -m flag: should update pinned to latest same major (1.26.0)
        assert!(result.update_to.is_some());
        if let Some(VersionSpec::Pinned(v)) = &result.update_to {
            assert_eq!(v.major, 1);
            assert_eq!(v.minor, 26);
        } else {
            panic!("Expected pinned version spec");
        }
    }

    #[test]
    fn test_force_latest_flag_all_update_to_latest() {
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: true,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("flask", "^2.0.0");
        let pkg_info = create_package_info("flask", vec!["2.0.0", "2.3.3", "3.0.0"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        // -f flag: should update to absolute latest (3.0.0)
        assert!(result.update_to.is_some());
        if let Some(spec) = &result.update_to {
            assert_eq!(spec.base_version().unwrap().to_string(), "3.0.0");
        }
    }

    #[test]
    fn test_caret_constraint_same_major() {
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("flask", "^2.0.0");
        let pkg_info = create_package_info("flask", vec!["2.0.0", "2.3.3", "3.0.0"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        // Caret means same major, in_range should be 2.3.3
        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "2.3.3");
    }

    #[test]
    fn test_no_update_when_already_latest() {
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("flask", ">=2.3.3");
        let pkg_info = create_package_info("flask", vec!["2.0.0", "2.3.3"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        // Already at latest in range, no update
        assert!(result.update_to.is_none());
    }

    #[test]
    fn test_unbounded_spec_with_higher_installed_version() {
        // Test case: spec is >=0.1.0 but installed is 1.25.0
        // Should suggest updating constraint to >=1.25.0 (not >=0.9.1 which would be wrong)
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("mcp", ">=0.1.0");
        // Available: 0.1.0, 0.5.0, 0.9.1, 1.0.0, 1.20.0, 1.25.0
        let pkg_info = create_package_info(
            "mcp",
            vec!["0.1.0", "0.5.0", "0.9.1", "1.0.0", "1.20.0", "1.25.0"],
        );

        // Installed version is 1.25.0 (from lock file)
        let installed = Version::from_str("1.25.0").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        // in_range should be 1.25.0 (latest in same major as installed)
        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "1.25.0");

        // Should suggest updating constraint from >=0.1.0 to >=1.25.0
        assert!(result.update_to.is_some());
        assert_eq!(result.update_to.unwrap().to_string(), ">=1.25.0");
    }

    #[test]
    fn test_unbounded_spec_with_higher_installed_but_newer_available() {
        // Test case: spec is >=0.1.0, installed is 1.20.0, latest is 1.25.0
        // Should suggest updating to >=1.25.0
        let args = Args {
            path: None,
            update: false,
            minor: false,
            force_latest: false,
            pre_release: false,
        };
        let resolver = DependencyResolver::new(args);

        let dep = create_test_dependency("mcp", ">=0.1.0");
        let pkg_info = create_package_info(
            "mcp",
            vec!["0.1.0", "0.5.0", "0.9.1", "1.0.0", "1.20.0", "1.25.0"],
        );

        // Installed version is 1.20.0
        let installed = Version::from_str("1.20.0").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        // in_range should be 1.25.0 (latest in major 1.x)
        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "1.25.0");

        // Should suggest updating to >=1.25.0
        assert!(result.update_to.is_some());
        assert_eq!(result.update_to.unwrap().to_string(), ">=1.25.0");
    }
}
