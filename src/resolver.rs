use crate::parsers::Dependency;
use crate::pypi::PackageInfo;
use crate::version::{Version, VersionSpec};

/// Result of checking a dependency
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateSeverity {
    Major,
    Minor,
    Patch,
}

/// Resolves dependencies and determines what updates are available
pub struct DependencyResolver;

impl DependencyResolver {
    pub fn new() -> Self {
        Self
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
        let in_range = self.calculate_in_range(
            &dependency.version_spec,
            &package_info.versions,
            installed,
        );

        // Determine the target version for display
        let current = installed.or_else(|| dependency.version_spec.base_version());

        let (target, target_spec) = self.calculate_target(
            &dependency.version_spec,
            &in_range,
            &latest,
            current,
        );

        // Calculate severity based on current → target
        let severity = self.calculate_severity(current, target.as_ref());

        // Calculate force spec (to absolute latest)
        let force_spec = self.calculate_force_spec(
            &dependency.version_spec,
            &latest,
            current,
        );

        DependencyCheck {
            dependency: dependency.clone(),
            installed: installed.cloned(),
            in_range,
            latest,
            target,
            target_spec,
            severity,
            force_spec,
        }
    }

    /// Calculate the target version and spec for display
    fn calculate_target(
        &self,
        current_spec: &VersionSpec,
        in_range: &Option<Version>,
        latest: &Version,
        current: Option<&Version>,
    ) -> (Option<Version>, Option<VersionSpec>) {
        let current = match current {
            Some(c) => c,
            None => return (None, None),
        };

        // Check if in_range is an update
        if let Some(ir) = in_range {
            if ir > current {
                let spec = current_spec.with_version(ir);
                return (Some(ir.clone()), Some(spec));
            }
        }

        // No in-range update, check if latest is an update
        if latest > current {
            let spec = current_spec.with_version(latest);
            return (Some(latest.clone()), Some(spec));
        }

        (None, None)
    }

    /// Calculate force spec (to absolute latest)
    fn calculate_force_spec(
        &self,
        current_spec: &VersionSpec,
        latest: &Version,
        current: Option<&Version>,
    ) -> Option<VersionSpec> {
        let current = current?;

        if latest > current {
            Some(current_spec.with_version(latest))
        } else {
            None
        }
    }

    /// Calculate the severity of an update
    fn calculate_severity(
        &self,
        current: Option<&Version>,
        target: Option<&Version>,
    ) -> Option<UpdateSeverity> {
        let current = current?;
        let target = target?;

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

    /// Calculate the latest version "in range" for the constraint
    fn calculate_in_range(
        &self,
        spec: &VersionSpec,
        available_versions: &[Version],
        installed: Option<&Version>,
    ) -> Option<Version> {
        available_versions
            .iter()
            .filter(|v| {
                // Must satisfy the spec
                if !spec.satisfies(v) {
                    return false;
                }

                // For unbounded specs (Minimum, GreaterThan), limit to same major
                match spec {
                    VersionSpec::Minimum(base) | VersionSpec::GreaterThan(base) => {
                        let target_major = if let Some(inst) = installed {
                            base.major.max(inst.major)
                        } else {
                            base.major
                        };
                        v.major == target_major
                    }
                    _ => true,
                }
            })
            .max()
            .cloned()
    }
}

impl Default for DependencyResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_in_range_update() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("requests", ">=2.28.0,<3.0.0");
        let pkg_info = create_package_info("requests", vec!["2.28.0", "2.32.3", "3.1.0"]);

        let installed = Version::from_str("2.28.0").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        // Target should be 2.32.3 (in-range update)
        assert!(result.target.is_some());
        assert_eq!(result.target.as_ref().unwrap().to_string(), "2.32.3");
        assert_eq!(result.severity, Some(UpdateSeverity::Minor));

        // Should have newer available (3.1.0)
        assert!(result.has_newer_available());
    }

    #[test]
    fn test_force_only_update() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("flask", "^2.0.0");
        let pkg_info = create_package_info("flask", vec!["2.0.0", "2.3.3", "3.0.0"]);

        // Installed at latest in-range (2.3.3)
        let installed = Version::from_str("2.3.3").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        // Target should be 3.0.0 (no in-range update, so force)
        assert!(result.target.is_some());
        assert_eq!(result.target.as_ref().unwrap().to_string(), "3.0.0");
        assert_eq!(result.severity, Some(UpdateSeverity::Major));

        // No newer available (target IS the latest)
        assert!(!result.has_newer_available());
    }

    #[test]
    fn test_no_update_needed() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("flask", ">=2.3.3");
        let pkg_info = create_package_info("flask", vec!["2.0.0", "2.3.3"]);

        let installed = Version::from_str("2.3.3").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        // No update needed
        assert!(result.target.is_none());
        assert!(!result.has_update());
    }
}
