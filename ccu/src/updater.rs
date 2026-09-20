use anyhow::{Context, Result};
use check_updates_core::{DependencyCheck, UpdateSeverity, write_atomically};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item, Value};

/// The dependency-bearing tables that can live under `[target.<key>]`.
const TARGET_KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// A dependency the updater was asked to rewrite but could not locate in the
/// manifest - a stale or wrong recorded `section`, or an entry that moved.
#[derive(Debug, Clone)]
pub struct NotApplied {
    pub name: String,
    pub section: Option<String>,
    pub file: PathBuf,
}

/// Per-file result of a rewrite attempt.
struct FileOutcome {
    changed: bool,
    not_applied: Vec<NotApplied>,
}

/// Updates Cargo.toml with new versions
pub struct FileUpdater;

impl FileUpdater {
    pub fn new() -> Self {
        Self
    }

    /// Apply updates to Cargo.toml based on severity filter
    /// - include_minor: false = patch only, true = patch + minor
    /// - force: true = all severities AND use absolute latest version
    pub fn apply_updates(
        &self,
        checks: &[DependencyCheck],
        include_minor: bool,
        force: bool,
    ) -> Result<UpdateResult> {
        let mut modified_files = HashSet::new();

        // Group checks by file, filtering by severity
        let mut file_updates: std::collections::HashMap<PathBuf, Vec<(&DependencyCheck, String)>> =
            std::collections::HashMap::new();

        for check in checks {
            // Determine which version spec to use
            let version_spec = if force {
                // Force mode: use absolute latest for all packages
                check.force_spec.as_ref()
            } else {
                // Normal mode: filter by severity and use target_spec
                match check.severity {
                    Some(UpdateSeverity::Patch) => check.target_spec.as_ref(),
                    Some(UpdateSeverity::Minor) if include_minor => check.target_spec.as_ref(),
                    _ => None, // Skip major updates and minor (if not included)
                }
            };

            if let Some(spec) = version_spec
                && spec.is_rewritable()
            {
                // Use Cargo-specific serialization (bare version = caret, = for pin, etc.)
                // Strip build metadata (+...) since it's not valid in Cargo.toml version requirements
                let new_version = spec.to_cargo_string().unwrap_or_else(|| spec.to_string());
                let new_version = match new_version.find('+') {
                    Some(idx) => new_version[..idx].to_string(),
                    None => new_version,
                };
                file_updates
                    .entry(check.dependency.source_file.clone())
                    .or_default()
                    .push((check, new_version));
            }
        }

        // Update each file. Sorted so a multi-file run reports a stable order.
        let mut file_paths: Vec<PathBuf> = file_updates.keys().cloned().collect();
        file_paths.sort();

        let mut not_applied = Vec::new();

        for file_path in file_paths {
            let updates = file_updates.remove(&file_path).unwrap_or_default();
            let outcome = self
                .update_file(&file_path, &updates)
                .with_context(|| format!("Failed to update file: {}", file_path.display()))?;

            not_applied.extend(outcome.not_applied);

            if outcome.changed {
                modified_files.insert(file_path);
            }
        }

        Ok(UpdateResult {
            modified_files,
            not_applied,
        })
    }

    /// Update a single Cargo.toml file.
    ///
    /// Reports both whether the bytes actually changed and which dependencies
    /// the document lookup failed to locate. A write is scoped to the single
    /// section the parser recorded, so a stale or wrong `section` misses
    /// silently unless the miss is carried back to the caller.
    fn update_file(
        &self,
        file_path: &PathBuf,
        updates: &[(&DependencyCheck, String)],
    ) -> Result<FileOutcome> {
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        let mut doc: DocumentMut = content
            .parse()
            .with_context(|| format!("Failed to parse TOML: {}", file_path.display()))?;

        let mut not_applied = Vec::new();

        // Apply each update. For renamed deps (`local = { package = "real", ... }`)
        // the table key is the local alias, not the upstream name.
        for (check, new_version) in updates {
            let lookup_key = check
                .dependency
                .manifest_key
                .as_deref()
                .unwrap_or(&check.dependency.name);
            let applied = self.update_dependency(
                &mut doc,
                lookup_key,
                new_version,
                check.dependency.section.as_deref(),
            );
            if !applied {
                not_applied.push(NotApplied {
                    name: check.dependency.name.clone(),
                    section: check.dependency.section.clone(),
                    file: file_path.clone(),
                });
            }
        }

        let new_content = doc.to_string();
        if new_content == content {
            // Nothing to write. Claiming the file as modified when its bytes are
            // identical is the same dishonesty pcu's updater already avoids:
            // what is reported as written has to match what was written.
            return Ok(FileOutcome {
                changed: false,
                not_applied,
            });
        }

        write_atomically(file_path, new_content.as_bytes())
            .with_context(|| format!("Failed to write file: {}", file_path.display()))?;

        Ok(FileOutcome {
            changed: true,
            not_applied,
        })
    }

    /// Update a dependency version in the document.
    ///
    /// When the parser recorded which table the dependency came from, only that
    /// table is rewritten. The same crate routinely appears in `[dependencies]`
    /// and `[dev-dependencies]` (or under several `[target.'cfg(..)']` tables) at
    /// deliberately different versions, and rewriting all of them at once
    /// silently destroys the versions we never checked. `None` means the section
    /// is unknown and we fall back to the historical all-sections sweep.
    ///
    /// Returns whether an entry was found and rewritten. The caller needs that
    /// signal: with the write scoped to one section, a stale or misspelled
    /// `section` is otherwise a silent no-op reported as a successful update.
    fn update_dependency(
        &self,
        doc: &mut DocumentMut,
        name: &str,
        new_version: &str,
        section: Option<&str>,
    ) -> bool {
        if let Some(section) = section {
            if let Some(dep) = Self::resolve_section(doc, section).and_then(|t| t.get_mut(name)) {
                self.update_dep_value(dep, new_version);
                return true;
            }
            return false;
        }

        let mut applied = false;

        // Try each dependency section
        let sections = ["dependencies", "dev-dependencies", "build-dependencies"];

        for section in sections {
            if let Some(deps) = doc.get_mut(section)
                && let Some(dep) = deps.get_mut(name)
            {
                self.update_dep_value(dep, new_version);
                applied = true;
            }
        }

        // Try workspace.dependencies
        if let Some(workspace) = doc.get_mut("workspace")
            && let Some(deps) = workspace.get_mut("dependencies")
            && let Some(dep) = deps.get_mut(name)
        {
            self.update_dep_value(dep, new_version);
            applied = true;
        }

        // Try target.*.{dependencies,dev-dependencies,build-dependencies}
        if let Some(target) = doc.get_mut("target")
            && let Some(target_table) = target.as_table_mut()
        {
            for (_, target_value) in target_table.iter_mut() {
                for kind in TARGET_KINDS {
                    if let Some(deps) = target_value.get_mut(kind)
                        && let Some(dep) = deps.get_mut(name)
                    {
                        self.update_dep_value(dep, new_version);
                        applied = true;
                    }
                }
            }
        }

        applied
    }

    /// Resolve a recorded section name to the table that holds the dependency
    /// entries. Target sections are matched by prefix/suffix rather than by
    /// splitting on `.`, because the target key itself is usually a quoted
    /// `cfg(...)` expression containing dots.
    fn resolve_section<'a>(doc: &'a mut DocumentMut, section: &str) -> Option<&'a mut Item> {
        match section {
            "dependencies" | "dev-dependencies" | "build-dependencies" => doc.get_mut(section),
            "workspace.dependencies" => doc.get_mut("workspace")?.get_mut("dependencies"),
            other => {
                let rest = other.strip_prefix("target.")?;
                // Longest suffix first: `.dependencies` is a suffix of both
                // `.dev-dependencies` and `.build-dependencies`.
                let (target_key, kind) = if let Some(k) = rest.strip_suffix(".dev-dependencies") {
                    (k, "dev-dependencies")
                } else if let Some(k) = rest.strip_suffix(".build-dependencies") {
                    (k, "build-dependencies")
                } else {
                    (rest.strip_suffix(".dependencies")?, "dependencies")
                };
                doc.get_mut("target")?.get_mut(target_key)?.get_mut(kind)
            }
        }
    }

    /// Update the version value in a dependency item
    fn update_dep_value(&self, item: &mut Item, new_version: &str) {
        match item {
            // Simple string: serde = "1.0"
            Item::Value(Value::String(s)) => {
                let decor = s.decor().clone();
                let mut new_str = toml_edit::Formatted::new(new_version.to_string());
                *new_str.decor_mut() = decor;
                *s = new_str;
            }
            // Inline table: serde = { version = "1.0", ... }
            Item::Value(Value::InlineTable(table)) => {
                if let Some(version) = table.get_mut("version")
                    && let Value::String(s) = version
                {
                    let decor = s.decor().clone();
                    let mut new_str = toml_edit::Formatted::new(new_version.to_string());
                    *new_str.decor_mut() = decor;
                    *s = new_str;
                }
            }
            // Full table: [dependencies.serde] version = "1.0"
            Item::Table(table) => {
                if let Some(version_item) = table.get_mut("version")
                    && let Item::Value(Value::String(s)) = version_item
                {
                    let decor = s.decor().clone();
                    let mut new_str = toml_edit::Formatted::new(new_version.to_string());
                    *new_str.decor_mut() = decor;
                    *s = new_str;
                }
            }
            _ => {}
        }
    }
}

impl Default for FileUpdater {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of applying updates
#[derive(Debug)]
pub struct UpdateResult {
    /// Files whose bytes actually changed
    pub modified_files: HashSet<PathBuf>,
    /// Updates that were selected for writing but never found in the manifest
    pub not_applied: Vec<NotApplied>,
}

impl UpdateResult {
    /// Warn about selected updates that the manifest lookup never found. These
    /// were displayed as updates, so staying silent would report a write that
    /// did not happen.
    pub fn print_not_applied(&self) {
        for miss in &self.not_applied {
            let section = miss.section.as_deref().unwrap_or("<unknown section>");
            eprintln!(
                "warning: {} was not found in [{}] of {} - not updated",
                miss.name,
                section,
                miss.file.display()
            );
        }
    }

    /// Print post-update messages
    pub fn print_summary(&self) {
        if !self.modified_files.is_empty() {
            println!();
            println!("Run `cargo update` to update Cargo.lock");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use check_updates_core::{Dependency, Version, VersionSpec};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_check(
        name: &str,
        spec_str: &str,
        path: PathBuf,
        target_version: &str,
        severity: UpdateSeverity,
    ) -> DependencyCheck {
        use std::str::FromStr;
        let target = Version::from_str(target_version).unwrap();
        DependencyCheck {
            dependency: Dependency {
                name: name.to_string(),
                version_spec: VersionSpec::parse(spec_str).unwrap(),
                source_file: path,
                line_number: Some(2),
                original_line: format!("{name} = \"{spec_str}\""),
                manifest_key: None,
                section: None,
            },
            installed: Some(Version::from_str(spec_str).unwrap()),
            in_range: Some(target.clone()),
            latest: target.clone(),
            target: Some(target.clone()),
            target_spec: Some(VersionSpec::parse(target_version).unwrap()),
            severity: Some(severity),
            force_spec: Some(VersionSpec::parse(target_version).unwrap()),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        }
    }

    /// A crate pinned at different versions in two tables must only have the
    /// table it was read from rewritten; the other version is a deliberate
    /// choice we never checked and must not be clobbered.
    #[test]
    fn update_is_scoped_to_the_recorded_section() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"[dependencies]
serde = "1.0.0"

[dev-dependencies]
serde = "1.0.1"
"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let mut check = create_check(
            "serde",
            "1.0.0",
            temp_path.clone(),
            "1.0.200",
            UpdateSeverity::Patch,
        );
        check.dependency.section = Some("dependencies".to_string());

        let updater = FileUpdater::new();
        updater.apply_updates(&[check], false, false)?;

        let content = fs::read_to_string(&temp_path)?;
        assert!(content.contains("1.0.200"), "{content}");
        assert!(
            content.contains("1.0.1"),
            "dev-dependencies must be untouched: {content}"
        );

        Ok(())
    }

    /// The section string a parser records for a target table contains dots
    /// inside the (unquoted) `cfg(...)` key, so `resolve_section` must match by
    /// prefix/suffix rather than splitting on `.`. Round-trip it end to end.
    #[test]
    fn target_cfg_sections_round_trip() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"[target.'cfg(unix)'.dependencies]
libc = "0.2.0"

[target.'cfg(unix)'.build-dependencies]
libc = "0.2.1"

[target.'cfg(windows)'.dependencies]
libc = "0.2.2"
"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let mut check = create_check(
            "libc",
            "0.2.0",
            temp_path.clone(),
            "0.2.99",
            UpdateSeverity::Patch,
        );
        check.dependency.section = Some("target.cfg(unix).dependencies".to_string());

        let updater = FileUpdater::new();
        let result = updater.apply_updates(&[check], false, false)?;

        assert!(result.not_applied.is_empty(), "{:?}", result.not_applied);
        assert_eq!(result.modified_files.len(), 1);

        let content = fs::read_to_string(&temp_path)?;
        assert!(content.contains("0.2.99"), "{content}");
        assert!(
            content.contains("0.2.1") && content.contains("0.2.2"),
            "sibling target tables must be untouched: {content}"
        );

        Ok(())
    }

    /// A target build-dependency is now both parseable and writable.
    #[test]
    fn target_build_dependencies_are_writable() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"[target.'cfg(windows)'.build-dependencies]
cc = "1.0.0"
"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let mut check = create_check(
            "cc",
            "1.0.0",
            temp_path.clone(),
            "1.0.90",
            UpdateSeverity::Patch,
        );
        check.dependency.section = Some("target.cfg(windows).build-dependencies".to_string());

        let updater = FileUpdater::new();
        let result = updater.apply_updates(&[check], false, false)?;

        assert!(result.not_applied.is_empty(), "{:?}", result.not_applied);
        assert!(fs::read_to_string(&temp_path)?.contains("1.0.90"));

        Ok(())
    }

    /// A wrong recorded section used to fail silently while the file was still
    /// reported as modified. It must now surface as a miss and leave the file
    /// untouched.
    #[test]
    fn a_missed_lookup_is_reported_and_writes_nothing() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"[dependencies]
serde = "1.0.0"
"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();
        let before = fs::read_to_string(&temp_path)?;

        let mut check = create_check(
            "serde",
            "1.0.0",
            temp_path.clone(),
            "1.0.200",
            UpdateSeverity::Patch,
        );
        // Stale section: serde does not live in [dev-dependencies].
        check.dependency.section = Some("dev-dependencies".to_string());

        let updater = FileUpdater::new();
        let result = updater.apply_updates(&[check], false, false)?;

        assert_eq!(result.not_applied.len(), 1, "{:?}", result.not_applied);
        assert_eq!(result.not_applied[0].name, "serde");
        assert!(
            result.modified_files.is_empty(),
            "an unchanged file must not be reported as modified"
        );
        assert_eq!(fs::read_to_string(&temp_path)?, before);

        Ok(())
    }

    /// The atomic write must land the new bytes and keep the original mode.
    #[test]
    #[cfg(unix)]
    fn atomic_write_preserves_permissions() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"[dependencies]
serde = "1.0.0"
"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o640))?;

        let mut check = create_check(
            "serde",
            "1.0.0",
            temp_path.clone(),
            "1.0.200",
            UpdateSeverity::Patch,
        );
        check.dependency.section = Some("dependencies".to_string());

        let updater = FileUpdater::new();
        updater.apply_updates(&[check], false, false)?;

        let mode = fs::metadata(&temp_path)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "mode was {mode:o}");
        assert!(fs::read_to_string(&temp_path)?.contains("1.0.200"));

        Ok(())
    }

    #[test]
    fn test_update_patch_only() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"[dependencies]
serde = "1.0.0"
tokio = "1.0.0"
"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            create_check(
                "serde",
                "1.0.0",
                temp_path.clone(),
                "1.0.200",
                UpdateSeverity::Patch,
            ),
            create_check(
                "tokio",
                "1.0.0",
                temp_path.clone(),
                "1.5.0",
                UpdateSeverity::Minor,
            ),
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, false, false)?; // patch only

        let content = fs::read_to_string(&temp_path)?;
        assert!(
            content.contains("1.0.200"),
            "serde should be updated: {content}"
        );
        assert!(
            !content.contains("1.5.0"),
            "tokio should NOT be updated: {content}"
        );

        Ok(())
    }

    #[test]
    fn test_update_patch_and_minor() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"[dependencies]
serde = "1.0.0"
tokio = "1.0.0"
"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            create_check(
                "serde",
                "1.0.0",
                temp_path.clone(),
                "1.0.200",
                UpdateSeverity::Patch,
            ),
            create_check(
                "tokio",
                "1.0.0",
                temp_path.clone(),
                "1.5.0",
                UpdateSeverity::Minor,
            ),
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, true, false)?; // patch + minor

        let content = fs::read_to_string(&temp_path)?;
        assert!(
            content.contains("1.0.200"),
            "serde should be updated: {content}"
        );
        assert!(
            content.contains("1.5.0"),
            "tokio should be updated: {content}"
        );

        Ok(())
    }
}
