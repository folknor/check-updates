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

/// Why `-u` cannot write a row, whatever flags the user passes.
///
/// This is deliberately *not* the same question as "did the severity filter
/// exclude this row". A major update under plain `-u` is withheld on purpose
/// and `-uf` will write it; the variants here survive `--force`, because the
/// obstacle is the declaration itself, not the policy. `resolution-principles`
/// rule 3 requires such a row to be shown and to say so - dropping it, or
/// rendering it identically to a row `-uf` would happily write, is the failure
/// mode: the user is told nothing is available when in fact something is, and
/// the reason it cannot be written is invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateBlocker {
    /// The declared constraint parsed to [`VersionSpec::Complex`]: a real
    /// constraint the parser cannot model (an npm hyphen range, `1.x`, a
    /// space-separated AND, a `||` union, a multi-clause PEP 440 specifier).
    /// It is reported, never rewritten - a rewrite would have to discard the
    /// part that was not understood.
    UnmodellableSpec,
    /// The declaration names no version at all (`*`, or an empty spec). There
    /// is nothing to rewrite, and inventing a constraint the user did not write
    /// is a semantic change, not an update.
    UnconstrainedSpec,
    /// The spec is modellable, but no writable target spec was produced - the
    /// resolver could not express the target in the declaration's own form.
    NoWritableTarget,
}

impl UpdateBlocker {
    /// Short marker for the table's last column. Kept bracketed and lowercase
    /// so it cannot be mistaken for a severity.
    pub fn marker(self) -> &'static str {
        match self {
            UpdateBlocker::UnmodellableSpec => "[not updatable: spec]",
            UpdateBlocker::UnconstrainedSpec => "[not updatable: no constraint]",
            UpdateBlocker::NoWritableTarget => "[not updatable: no target]",
        }
    }

    /// One-line reason, printed once per distinct blocker below the table
    /// rather than on every row, so a table full of unmodellable npm ranges
    /// does not become a table full of repeated prose.
    pub fn explanation(self) -> &'static str {
        match self {
            UpdateBlocker::UnmodellableSpec => {
                "the declared constraint could not be modelled, so -u leaves it alone even with --force; update it by hand"
            }
            UpdateBlocker::UnconstrainedSpec => {
                "the declaration pins no version, so there is nothing for -u to rewrite"
            }
            UpdateBlocker::NoWritableTarget => {
                "no target could be expressed in this declaration's own syntax, so -u leaves it alone even with --force"
            }
        }
    }
}

/// Result of checking a dependency for updates
#[derive(Debug, Clone)]
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
    /// ISO-8601 publish date of the installed version (registry data).
    ///
    /// Omitted from the report when absent; the omission is implemented by the
    /// hand-written `Serialize` below rather than by a `skip_serializing_if`
    /// attribute, which a manual impl does not read.
    pub installed_released_at: Option<String>,
    /// ISO-8601 publish date of the target version (registry data). Omitted
    /// from the report when absent.
    pub target_released_at: Option<String>,
    /// ISO-8601 publish date of the latest version (registry data). Omitted
    /// from the report when absent.
    pub latest_released_at: Option<String>,
    /// True when the registry never answered for this dependency, so nothing
    /// below `dependency` and `installed` was actually resolved.
    ///
    /// Such a row exists because the alternative is worse: the CLIs used to
    /// drop a dependency whose fetch failed, so it was absent from the table
    /// *and* from the JSON `checks` array, and a consumer could not tell "up to
    /// date" from "we could not check". A failed check carries `target: None`,
    /// so it has no update, is never written by `-u`, and is counted as neither
    /// a skipped nor a blocked update - the only thing it claims is that it was
    /// asked about and got no answer.
    ///
    /// Consumers must not read `has_update: false` on such a row as "up to
    /// date". The report emits `latest: null` for it, because the in-memory
    /// `latest` is only a placeholder here (see [`DependencyCheck::unchecked`]),
    /// never a version the registry returned.
    pub check_failed: bool,
}

/// Hand-written rather than derived so the report carries `updatable` and
/// `blocked_reason`, which are computed from the other fields.
///
/// A derive cannot express that: serde has no "virtual field" attribute
/// outside `remote` derive. The alternative - storing the answer in a real
/// field - would add a member to a struct that is constructed in several
/// crates, and the blocker is a pure function of data already present, so
/// there is nothing to store. `updatable` is always emitted (a consumer
/// testing `row.updatable === false` must not have to distinguish false from
/// absent, which in JSON-consuming languages is the same falsy value);
/// `blocked_reason` appears only when there is one. A row with no target is
/// trivially `updatable: true` - nothing is being withheld from it.
///
/// `check_failed` is always emitted for the same reason `updatable` is, and it
/// is the field that qualifies every other one: when it is true, `latest` is
/// serialized as `null` rather than as the placeholder the struct carries, so a
/// consumer cannot mistake an unanswered lookup for a registry answer.
impl Serialize for DependencyCheck {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let blocker = self.update_blocker();
        let optional = usize::from(self.installed_released_at.is_some())
            + usize::from(self.target_released_at.is_some())
            + usize::from(self.latest_released_at.is_some())
            + usize::from(blocker.is_some());

        let mut state = serializer.serialize_struct("DependencyCheck", 10 + optional)?;
        state.serialize_field("dependency", &self.dependency)?;
        state.serialize_field("installed", &self.installed)?;
        state.serialize_field("in_range", &self.in_range)?;
        state.serialize_field("latest", &self.reported_latest())?;
        state.serialize_field("target", &self.target)?;
        state.serialize_field("target_spec", &self.target_spec)?;
        state.serialize_field("severity", &self.severity)?;
        state.serialize_field("force_spec", &self.force_spec)?;
        state.serialize_field("updatable", &blocker.is_none())?;
        state.serialize_field("check_failed", &self.check_failed)?;
        if let Some(blocker) = blocker {
            state.serialize_field("blocked_reason", &blocker)?;
        }
        if let Some(released_at) = &self.installed_released_at {
            state.serialize_field("installed_released_at", released_at)?;
        }
        if let Some(released_at) = &self.target_released_at {
            state.serialize_field("target_released_at", released_at)?;
        }
        if let Some(released_at) = &self.latest_released_at {
            state.serialize_field("latest_released_at", released_at)?;
        }
        state.end()
    }
}

impl DependencyCheck {
    /// A check for a dependency the registry never answered for.
    ///
    /// Every resolved field is left empty: there is no target, no severity and
    /// no spec to write, because nothing was resolved. `latest` cannot be left
    /// empty - it is not an `Option`, and the three CLI tables read it - so it
    /// is filled with the best version already in hand (the installed one, else
    /// the version the declaration names, else `0.0.0`) purely as a placeholder.
    /// It is never reported: [`DependencyCheck::reported_latest`] returns `None`
    /// for a failed check and the JSON carries `latest: null`.
    pub fn unchecked(dependency: &Dependency, installed: Option<&Version>) -> Self {
        let placeholder = installed
            .or_else(|| dependency.version_spec.base_version())
            .cloned()
            .unwrap_or_else(|| Version::new(0, 0, 0));

        Self {
            dependency: dependency.clone(),
            installed: installed.cloned(),
            in_range: None,
            latest: placeholder,
            target: None,
            target_spec: None,
            severity: None,
            force_spec: None,
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
            check_failed: true,
        }
    }

    /// The latest version as it may be reported to a user or a consumer:
    /// `None` when the lookup failed, because the `latest` field holds a
    /// placeholder in that case and printing it would invent a registry answer.
    pub fn reported_latest(&self) -> Option<&Version> {
        if self.check_failed {
            None
        } else {
            Some(&self.latest)
        }
    }

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

    /// Why `-u` cannot write this row at all, or `None` when some combination
    /// of flags would write it.
    ///
    /// Defined as "not writable under the most permissive flags": if
    /// `will_update(true, true)` is false, no lesser mode can be true, so the
    /// row is unactionable and the user is owed a reason. A row the severity
    /// filter merely excluded returns `None` here and stays in the existing
    /// "skipped, run -uf" count, which is a different and recoverable thing.
    ///
    /// Computed, not stored: `DependencyCheck` is built in several places and
    /// a blocker is a function of the spec and the resolved specs the check
    /// already carries, so there is nothing a field could record that this
    /// cannot derive.
    pub fn update_blocker(&self) -> Option<UpdateBlocker> {
        // A failed check is not a blocked update: nothing was withheld from
        // it, because nothing was resolved for it in the first place. It must
        // not land in the "cannot be written even with --force" count, which
        // the user is told to act on.
        if self.check_failed {
            return None;
        }

        // A row with no update to offer has nothing to be blocked about: "up
        // to date" is already an honest answer, and flagging it would bury the
        // rows that do show a target `-u` will silently refuse to write.
        if !self.has_update() || self.will_update(true, true) {
            return None;
        }

        Some(match &self.dependency.version_spec {
            VersionSpec::Complex(_) => UpdateBlocker::UnmodellableSpec,
            VersionSpec::Any => UpdateBlocker::UnconstrainedSpec,
            _ => UpdateBlocker::NoWritableTarget,
        })
    }

    /// True when at least one flag combination would rewrite this dependency.
    pub fn is_actionable(&self) -> bool {
        self.update_blocker().is_none()
    }

    /// Check if there's a newer version available beyond the target
    pub fn has_newer_available(&self) -> bool {
        if self.check_failed {
            return false;
        }
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
            check_failed: false,
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

    /// The distinction the whole entry is about: a major update is withheld by
    /// policy and `-uf` writes it, so it is not blocked; an unmodellable spec
    /// survives `-uf`, so it is.
    #[test]
    fn severity_filtering_is_not_a_blocker() {
        let major = check(UpdateSeverity::Major);
        assert!(!major.will_update(false, false));
        assert_eq!(major.update_blocker(), None);
        assert!(major.is_actionable());
    }

    #[test]
    fn complex_spec_is_blocked_as_unmodellable() {
        let mut complex = check(UpdateSeverity::Major);
        complex.dependency.version_spec = VersionSpec::Complex("1.2.3 - 2.3.4".to_string());
        complex.target_spec = None;
        complex.force_spec = None;

        assert!(complex.has_update(), "the row still shows a target");
        assert!(
            !complex.will_update(true, true),
            "and -uf will not write it"
        );
        assert_eq!(
            complex.update_blocker(),
            Some(UpdateBlocker::UnmodellableSpec)
        );
    }

    #[test]
    fn unconstrained_spec_is_its_own_reason() {
        let mut any = check(UpdateSeverity::Patch);
        any.dependency.version_spec = VersionSpec::Any;
        any.target_spec = Some(VersionSpec::Any);
        any.force_spec = Some(VersionSpec::Any);

        assert_eq!(any.update_blocker(), Some(UpdateBlocker::UnconstrainedSpec));
    }

    /// A modellable spec that produced no writable target is still a dead end,
    /// and must not be reported as an unmodellable one.
    #[test]
    fn missing_target_spec_is_reported_separately() {
        let mut check = check(UpdateSeverity::Patch);
        check.target_spec = None;
        check.force_spec = None;

        assert_eq!(
            check.update_blocker(),
            Some(UpdateBlocker::NoWritableTarget)
        );
    }

    #[test]
    fn up_to_date_row_is_never_blocked() {
        let mut none = check(UpdateSeverity::Patch);
        none.target = None;
        none.target_spec = None;
        none.force_spec = None;

        assert!(!none.has_update());
        assert_eq!(none.update_blocker(), None);
    }

    /// The serialized shape - `updatable` plus an optional `blocked_reason` -
    /// is what a `--json` consumer reads, and it is asserted in the CLI crates
    /// rather than here: `core` has no `serde_json` dev-dependency, and adding
    /// one is a `Cargo.toml` change. The invariants the manual `Serialize` impl
    /// depends on are pinned above instead: `update_blocker` is total, and the
    /// three optional date fields are the only conditionally-emitted members.
    #[test]
    fn blocker_reasons_are_distinguishable() {
        let all = [
            UpdateBlocker::UnmodellableSpec,
            UpdateBlocker::UnconstrainedSpec,
            UpdateBlocker::NoWritableTarget,
        ];

        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.marker(), b.marker(), "markers must not collide");
                assert_ne!(
                    a.explanation(),
                    b.explanation(),
                    "explanations must not collide"
                );
            }
        }
    }

    /// The whole point of the failed-check row: it exists, it offers nothing,
    /// and it is neither writable nor counted as a withheld update.
    #[test]
    fn a_failed_check_offers_nothing_and_is_not_an_update() {
        let dependency = check(UpdateSeverity::Patch).dependency;
        let installed = Version::new(6, 0, 0);
        let failed = DependencyCheck::unchecked(&dependency, Some(&installed));

        assert!(failed.check_failed);
        assert!(!failed.has_update());
        assert!(!failed.will_update(true, true));
        assert_eq!(failed.update_blocker(), None, "not a blocked update");
        assert!(!failed.has_newer_available());
        assert_eq!(failed.reported_latest(), None, "no registry answer to give");
        assert_eq!(
            failed.current_version(),
            Some(&installed),
            "what we did know survives"
        );
    }

    /// The placeholder never escapes as a version anyone could act on, even
    /// when there is nothing installed to fall back to.
    #[test]
    fn a_failed_check_reports_no_latest_without_an_installed_version() {
        let mut dependency = check(UpdateSeverity::Patch).dependency;
        dependency.version_spec = VersionSpec::Any;

        let failed = DependencyCheck::unchecked(&dependency, None);
        assert_eq!(failed.reported_latest(), None);
        assert_eq!(failed.current_version(), None);
    }

    #[test]
    fn unrewritable_spec_never_updates() {
        let mut major = check(UpdateSeverity::Major);
        major.force_spec = Some(VersionSpec::Any);
        assert!(!major.will_update(true, true));
    }
}
