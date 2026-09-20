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
        let in_range =
            self.calculate_in_range(&dependency.version_spec, &package_info.versions, installed);

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

    /// Classify the move from `current` to `target`.
    ///
    /// `None` means "this is not an update", and nothing else. It used to also
    /// mean "this is an update we have no field to name it by": the three
    /// fields were compared independently, so `1.2.0-rc1` -> `1.2.0` had major,
    /// minor and patch all equal and fell through to `None`. That row had a
    /// real target, printed a blank severity column, and `will_update` refused
    /// to write it in every non-force mode - a real available update, displayed
    /// blank. The same hole swallowed `1.2.0` -> `1.2.0.post1`, `1.2.3` ->
    /// `1.2.3.4` and every local-segment move.
    ///
    /// So the ordering decides whether there is an update at all - one
    /// comparison, using the same `Ord` the rest of the pipeline uses - and the
    /// fields only choose the *name*. Anything that is newer without changing
    /// the triple (leaving a pre-release, gaining a post-release, a fourth
    /// release segment, a local segment) is a `Patch`: it is the smallest class
    /// we have, and classing it lower than it deserves would hide it, while
    /// classing it higher would only mean `-u` declines to write a de-risking
    /// move.
    ///
    /// An epoch bump is a `Major`. PEP 440 epochs exist precisely to restart a
    /// version series, so `1.0` -> `1!0.1` is the most disruptive move there is
    /// even though the release tuple went down.
    pub fn calculate_severity(
        current: Option<&Version>,
        target: Option<&Version>,
    ) -> Option<UpdateSeverity> {
        let current = current?;
        let target = target?;

        if target <= current {
            return None;
        }

        if target.epoch != current.epoch || target.major > current.major {
            Some(UpdateSeverity::Major)
        } else if target.minor > current.minor {
            Some(UpdateSeverity::Minor)
        } else {
            Some(UpdateSeverity::Patch)
        }
    }

    /// The latest version this dependency can move to while staying within the
    /// spirit of its declared constraint.
    ///
    /// For every bounded spec that is exactly "the maximum version the spec
    /// accepts", so the answer comes straight from
    /// [`VersionSpec::satisfies`].
    ///
    /// `Minimum` and `GreaterThan` need an extra rule, and it is deliberate.
    /// An unbounded spec is satisfied by every version ever published, so
    /// "the maximum satisfying version" degenerates to "the latest version" and
    /// the answer carries no information at all - `in_range` would simply
    /// duplicate `latest`, and [`DependencyCheck::has_newer_available`] would be
    /// permanently false, suppressing the "(x.y.z available)" hint precisely
    /// when there is something to hint at.
    ///
    /// So for those two variants "in range" means the newest release in the
    /// major series the dependency is actually on - `base.major`, raised to the
    /// installed major when the lock file has moved past the declared floor.
    /// A `>=2.28.0` dependency installed at 2.28.0 with 2.32.3 and 3.1.0
    /// published is offered 2.32.3, and told 3.1.0 exists.
    ///
    /// This is not a contradiction of `satisfies`. `satisfies` answers "may this
    /// version be used?", which 3.1.0 may; this answers "where should we move
    /// to?", which is a different question and the only one with a useful answer
    /// for an unbounded floor. Crossing a major boundary stays available through
    /// `--force`, where it is an explicit choice rather than a silent one.
    fn calculate_in_range(
        &self,
        spec: &VersionSpec,
        available_versions: &[Version],
        installed: Option<&Version>,
    ) -> Option<Version> {
        available_versions
            .iter()
            .filter(|v| {
                if !spec.satisfies(v) {
                    return false;
                }

                match spec {
                    VersionSpec::Minimum(base) | VersionSpec::GreaterThan(base) => {
                        let series = match installed {
                            Some(inst) => base.major.max(inst.major),
                            None => base.major,
                        };
                        v.major == series
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

    // An unbounded floor is satisfied by every version ever published, so
    // "maximum satisfying version" would just be `latest` and the field would
    // carry nothing. "In range" for `>=` therefore means the newest release in
    // the series the dependency is on: the user is offered 2.32.3 and told that
    // 3.1.0 exists, rather than being offered nothing and told nothing.
    #[test]
    fn unbounded_minimum_stays_in_its_major_series() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("requests", ">=2.28.0");
        let pkg_info = create_package_info("requests", &["2.28.0", "2.32.3", "3.1.0"]);

        let installed = Version::from_str("2.28.0").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "2.32.3");
        assert_eq!(result.severity, Some(UpdateSeverity::Minor));
        assert!(result.will_update(true, false));

        // The major release is still reachable, and still visible.
        assert!(result.has_newer_available());
        assert_eq!(result.latest.to_string(), "3.1.0");
        assert_eq!(result.force_spec.as_ref().unwrap().to_string(), ">=3.1.0");
    }

    // The floor rises with the lock file: a dependency declared `>=1.0.0` but
    // installed at 2.x is on the 2.x series, not the 1.x one its spec names.
    #[test]
    fn installed_major_raises_the_series_above_the_declared_floor() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("serde", ">=1.0.0");
        let pkg_info = create_package_info("serde", &["1.0.9", "2.1.0", "2.4.2", "3.0.0"]);

        let installed = Version::from_str("2.1.0").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        assert_eq!(result.in_range.as_ref().unwrap().to_string(), "2.4.2");
    }

    // A move that leaves the triple untouched is still an update. Comparing
    // major, minor and patch independently classified it `None`, which printed
    // a blank severity column and stopped `-u` from ever writing it.
    #[test]
    fn updates_that_do_not_change_the_triple_are_still_classified() {
        let sev = |from: &str, to: &str| {
            DependencyResolver::calculate_severity(
                Some(&Version::from_str(from).unwrap()),
                Some(&Version::from_str(to).unwrap()),
            )
        };

        assert_eq!(sev("1.2.0-rc1", "1.2.0"), Some(UpdateSeverity::Patch));
        assert_eq!(sev("1.2.0", "1.2.0.post1"), Some(UpdateSeverity::Patch));
        assert_eq!(sev("1.2.3", "1.2.3.4"), Some(UpdateSeverity::Patch));
        assert_eq!(sev("1.0.0+cpu", "1.0.0+cu118"), Some(UpdateSeverity::Patch));

        assert_eq!(sev("1.2.3", "1.3.0"), Some(UpdateSeverity::Minor));
        assert_eq!(sev("1.2.3", "2.0.0"), Some(UpdateSeverity::Major));
        // An epoch reset is the most disruptive move there is, even though the
        // release tuple went down.
        assert_eq!(sev("2024.1", "1!0.1"), Some(UpdateSeverity::Major));
    }

    // `None` means "not an update" and nothing else. A target that is not newer
    // must not be classified by whichever field happens to be larger.
    #[test]
    fn a_target_that_is_not_newer_has_no_severity() {
        let sev = |from: &str, to: &str| {
            DependencyResolver::calculate_severity(
                Some(&Version::from_str(from).unwrap()),
                Some(&Version::from_str(to).unwrap()),
            )
        };

        assert_eq!(sev("1.2.3", "1.2.3"), None);
        assert_eq!(sev("1.2.3", "1.1.5"), None);
        assert_eq!(sev("1.2.0", "1.2.0-rc1"), None);
        assert_eq!(sev("1.4.0.post1", "1.4.0"), None);
    }

    // The whole point, end to end: a prerelease installed against a released
    // target produces a row that is written, not a blank column.
    #[test]
    fn prerelease_to_release_is_a_writable_update() {
        let resolver = DependencyResolver::new();
        let dep = create_test_dependency("flask", ">=1.2.0-rc1");
        let pkg_info = create_package_info("flask", &["1.2.0-rc1", "1.2.0"]);

        let installed = Version::from_str("1.2.0-rc1").unwrap();
        let result = resolver.resolve(&dep, &pkg_info, Some(&installed));

        assert_eq!(result.target.as_ref().unwrap().to_string(), "1.2.0");
        assert_eq!(result.severity, Some(UpdateSeverity::Patch));
        assert!(result.will_update(false, false));
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
