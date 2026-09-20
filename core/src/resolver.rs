use crate::types::{Dependency, DependencyCheck, PackageInfo, UpdateSeverity};
use crate::version::{Version, VersionSpec};

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
        let in_range = self.calculate_in_range(&dependency.version_spec, &package_info.versions);

        // Determine the target version for display
        let current = installed.or_else(|| dependency.version_spec.base_version());

        let (target, target_spec) =
            self.calculate_target(&dependency.version_spec, &in_range, &latest, current);

        // Calculate severity based on current → target
        let severity = Self::calculate_severity(current, target.as_ref());

        // Calculate force spec (to absolute latest)
        let force_spec = self.calculate_force_spec(&dependency.version_spec, &latest, current);

        // Look up registry-published dates for the three versions a consumer
        // might care about. Missing entries stay as None so the JSON stays
        // tidy via skip_serializing_if.
        let lookup_date = |v: Option<&Version>| -> Option<String> {
            v.and_then(|ver| package_info.published_at.get(&ver.original).cloned())
        };
        let installed_released_at = lookup_date(installed);
        let target_released_at = lookup_date(target.as_ref());
        let latest_released_at = lookup_date(Some(&latest));

        DependencyCheck {
            dependency: dependency.clone(),
            installed: installed.cloned(),
            in_range,
            latest,
            target,
            target_spec,
            severity,
            force_spec,
            installed_released_at,
            target_released_at,
            latest_released_at,
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
            // No current version (no lockfile, no base_version for Complex specs).
            // Still report latest as target so the dependency shows up in review,
            // but with no spec (can't determine severity or offer a rewrite).
            None => return (Some(latest.clone()), None),
        };

        // Check if in_range is an update
        if let Some(ir) = in_range
            && ir > current
        {
            let spec = if current_spec.is_rewritable() {
                Some(current_spec.with_version(ir))
            } else {
                None
            };
            return (Some(ir.clone()), spec);
        }

        // No in-range update, check if latest is an update
        if latest > current {
            let spec = if current_spec.is_rewritable() {
                Some(current_spec.with_version(latest))
            } else {
                None
            };
            return (Some(latest.clone()), spec);
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

        if latest > current && current_spec.is_rewritable() {
            Some(current_spec.with_version(latest))
        } else {
            None
        }
    }

    /// Calculate the severity of an update
    pub fn calculate_severity(
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

    /// The latest available version that satisfies the constraint.
    ///
    /// There is exactly one definition of "in range" and it lives in
    /// [`VersionSpec::satisfies`]. This function only takes the maximum of what
    /// that predicate accepts.
    ///
    /// It used to add a second, undocumented rule on top: for `Minimum` and
    /// `GreaterThan` - two of thirteen variants - it discarded every candidate
    /// outside the major series of the base or installed version. That made
    /// `DependencyCheck.in_range` something other than what `types.rs` documents
    /// it to be, and made the field disagree with the predicate the same crate
    /// exposes: `>=2.28.0` is satisfied by 3.1.0 by any reading of the spec, in
    /// Cargo and in PEP 440 alike. A tool that reports otherwise is lying about
    /// the user's own constraint.
    ///
    /// Caution about major versions belongs to the severity axis, not to this
    /// one: a `>=` dependency whose in-range latest crosses a major boundary is
    /// classified `Major`, and `will_update` refuses it in every mode but
    /// `--force`. The behaviour that changes is that `-u`/`-um` no longer raise
    /// the floor of an unbounded spec to the newest same-major release; raising
    /// a floor was never required by the constraint, and doing it silently hid
    /// the real (major) update behind a minor-looking one.
    fn calculate_in_range(
        &self,
        spec: &VersionSpec,
        available_versions: &[Version],
    ) -> Option<Version> {
        available_versions
            .iter()
            .filter(|v| spec.satisfies(v))
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::str::FromStr;

    fn create_test_dependency(name: &str, spec_str: &str) -> Dependency {
        Dependency {
            name: name.to_string(),
            version_spec: VersionSpec::parse(spec_str).unwrap(),
            source_file: PathBuf::from("test.txt"),
            line_number: Some(1),
            original_line: format!("{name}=={spec_str}"),
            manifest_key: None,
            section: None,
        }
    }

    fn create_package_info(name: &str, versions: &[&str]) -> PackageInfo {
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
            published_at: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_in_range_update() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("requests", ">=2.28.0,<3.0.0");
        let pkg_info = create_package_info("requests", &["2.28.0", "2.32.3", "3.1.0"]);

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
        let pkg_info = create_package_info("flask", &["2.0.0", "2.3.3", "3.0.0"]);

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

    // A wildcard dependency with no lock entry still has a current version to
    // compare against, so it gets a target, a severity and a rewritable spec
    // instead of silently never updating.
    #[test]
    fn wildcard_without_a_lockfile_still_resolves() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("numpy", "==1.24.*");
        let pkg_info = create_package_info("numpy", &["1.24.0", "1.24.4", "1.26.0"]);

        let result = resolver.resolve(&dep, &pkg_info, None);

        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "1.24.4");
        assert_eq!(result.target.as_ref().unwrap().to_string(), "1.24.4");
        assert_eq!(result.severity, Some(UpdateSeverity::Patch));
        assert_eq!(result.target_spec.as_ref().unwrap().to_string(), "==1.24.*");
        assert!(result.will_update(false, false));
    }

    // One definition of "in range": whatever `satisfies` accepts. An unbounded
    // minimum is satisfied by the next major, and we say so - the major update
    // is then withheld by severity, not by pretending it is out of range.
    #[test]
    fn unbounded_minimum_is_in_range_across_majors() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("requests", ">=2.28.0");
        let pkg_info = create_package_info("requests", &["2.28.0", "2.32.3", "3.1.0"]);

        let installed = Version::from_str("2.28.0").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "3.1.0");
        assert_eq!(result.severity, Some(UpdateSeverity::Major));
        assert!(!result.will_update(true, false));
        assert!(result.will_update(false, true));
    }

    #[test]
    fn test_no_update_needed() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("flask", ">=2.3.3");
        let pkg_info = create_package_info("flask", &["2.0.0", "2.3.3"]);

        let installed = Version::from_str("2.3.3").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        // No update needed
        assert!(result.target.is_none());
        assert!(!result.has_update());
    }
}
