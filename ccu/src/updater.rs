use check_updates_core::{DependencyCheck, UpdateSeverity};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item, Value};

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

            if let Some(spec) = version_spec {
                // Use version_string() for Cargo.toml format (without "==" prefix)
                let new_version = spec.version_string().unwrap_or_else(|| spec.to_string());
                file_updates
                    .entry(check.dependency.source_file.clone())
                    .or_default()
                    .push((check, new_version));
            }
        }

        // Update each file
        for (file_path, updates) in file_updates {
            self.update_file(&file_path, &updates)
                .with_context(|| format!("Failed to update file: {}", file_path.display()))?;

            modified_files.insert(file_path);
        }

        Ok(UpdateResult { modified_files })
    }

    /// Update a single Cargo.toml file
    fn update_file(
        &self,
        file_path: &PathBuf,
        updates: &[(&DependencyCheck, String)],
    ) -> Result<()> {
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        let mut doc: DocumentMut = content
            .parse()
            .with_context(|| format!("Failed to parse TOML: {}", file_path.display()))?;

        // Apply each update
        for (check, new_version) in updates {
            self.update_dependency(&mut doc, &check.dependency.name, new_version);
        }

        // Write the updated content
        fs::write(file_path, doc.to_string())
            .with_context(|| format!("Failed to write file: {}", file_path.display()))?;

        Ok(())
    }

    /// Update a dependency version in the document
    fn update_dependency(&self, doc: &mut DocumentMut, name: &str, new_version: &str) {
        // Try each dependency section
        let sections = [
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
        ];

        for section in sections {
            if let Some(deps) = doc.get_mut(section) {
                if let Some(dep) = deps.get_mut(name) {
                    self.update_dep_value(dep, new_version);
                }
            }
        }

        // Try workspace.dependencies
        if let Some(workspace) = doc.get_mut("workspace") {
            if let Some(deps) = workspace.get_mut("dependencies") {
                if let Some(dep) = deps.get_mut(name) {
                    self.update_dep_value(dep, new_version);
                }
            }
        }

        // Try target.*.dependencies
        if let Some(target) = doc.get_mut("target") {
            if let Some(target_table) = target.as_table_mut() {
                for (_, target_value) in target_table.iter_mut() {
                    if let Some(deps) = target_value.get_mut("dependencies") {
                        if let Some(dep) = deps.get_mut(name) {
                            self.update_dep_value(dep, new_version);
                        }
                    }
                    if let Some(deps) = target_value.get_mut("dev-dependencies") {
                        if let Some(dep) = deps.get_mut(name) {
                            self.update_dep_value(dep, new_version);
                        }
                    }
                }
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
                if let Some(version) = table.get_mut("version") {
                    if let Value::String(s) = version {
                        let decor = s.decor().clone();
                        let mut new_str = toml_edit::Formatted::new(new_version.to_string());
                        *new_str.decor_mut() = decor;
                        *s = new_str;
                    }
                }
            }
            // Full table: [dependencies.serde] version = "1.0"
            Item::Table(table) => {
                if let Some(version_item) = table.get_mut("version") {
                    if let Item::Value(Value::String(s)) = version_item {
                        let decor = s.decor().clone();
                        let mut new_str = toml_edit::Formatted::new(new_version.to_string());
                        *new_str.decor_mut() = decor;
                        *s = new_str;
                    }
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
    /// Files that were modified
    pub modified_files: HashSet<PathBuf>,
}

impl UpdateResult {
    /// Print post-update messages
    pub fn print_summary(&self) {
        if !self.modified_files.is_empty() {
            println!("Run `cargo update` to update Cargo.lock");
        }
    }
}

#[cfg(test)]
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
                line_number: 2,
                original_line: format!("{} = \"{}\"", name, spec_str),
            },
            installed: Some(Version::from_str(spec_str).unwrap()),
            in_range: Some(target.clone()),
            latest: target.clone(),
            target: Some(target.clone()),
            target_spec: Some(VersionSpec::parse(target_version).unwrap()),
            severity: Some(severity),
            force_spec: Some(VersionSpec::parse(target_version).unwrap()),
        }
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
            create_check("serde", "1.0.0", temp_path.clone(), "1.0.200", UpdateSeverity::Patch),
            create_check("tokio", "1.0.0", temp_path.clone(), "1.5.0", UpdateSeverity::Minor),
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, false, false)?; // patch only

        let content = fs::read_to_string(&temp_path)?;
        assert!(content.contains("1.0.200"), "serde should be updated: {}", content);
        assert!(!content.contains("1.5.0"), "tokio should NOT be updated: {}", content);

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
            create_check("serde", "1.0.0", temp_path.clone(), "1.0.200", UpdateSeverity::Patch),
            create_check("tokio", "1.0.0", temp_path.clone(), "1.5.0", UpdateSeverity::Minor),
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, true, false)?; // patch + minor

        let content = fs::read_to_string(&temp_path)?;
        assert!(content.contains("1.0.200"), "serde should be updated: {}", content);
        assert!(content.contains("1.5.0"), "tokio should be updated: {}", content);

        Ok(())
    }
}
