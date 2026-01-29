use check_updates_core::{DependencyCheck, UpdateSeverity};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

/// Updates package.json with new versions
pub struct FileUpdater;

impl FileUpdater {
    pub fn new() -> Self {
        Self
    }

    /// Apply updates to package.json based on severity filter
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
            let version_spec = if force {
                check.force_spec.as_ref()
            } else {
                match check.severity {
                    Some(UpdateSeverity::Patch) => check.target_spec.as_ref(),
                    Some(UpdateSeverity::Minor) if include_minor => check.target_spec.as_ref(),
                    _ => None,
                }
            };

            if let Some(spec) = version_spec {
                // For npm, preserve the original prefix (^, ~, etc.)
                let new_version = spec.to_string();
                file_updates
                    .entry(check.dependency.source_file.clone())
                    .or_default()
                    .push((check, new_version));
            }
        }

        for (file_path, updates) in file_updates {
            self.update_file(&file_path, &updates)
                .with_context(|| format!("Failed to update file: {}", file_path.display()))?;
            modified_files.insert(file_path);
        }

        Ok(UpdateResult { modified_files })
    }

    fn update_file(
        &self,
        file_path: &PathBuf,
        updates: &[(&DependencyCheck, String)],
    ) -> Result<()> {
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        // Parse as JSON, update, and write back preserving formatting
        let mut parsed: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON: {}", file_path.display()))?;

        for (check, new_version) in updates {
            self.update_dependency(&mut parsed, &check.dependency.name, new_version);
        }

        // Write back with pretty formatting
        let updated = serde_json::to_string_pretty(&parsed)
            .with_context(|| "Failed to serialize JSON")?;

        fs::write(file_path, updated + "\n")
            .with_context(|| format!("Failed to write file: {}", file_path.display()))?;

        Ok(())
    }

    fn update_dependency(&self, doc: &mut serde_json::Value, name: &str, new_version: &str) {
        let sections = [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ];

        for section in sections {
            if let Some(deps) = doc.get_mut(section).and_then(|v| v.as_object_mut()) {
                if deps.contains_key(name) {
                    deps.insert(name.to_string(), serde_json::Value::String(new_version.to_string()));
                }
            }
        }
    }
}

impl Default for FileUpdater {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct UpdateResult {
    pub modified_files: HashSet<PathBuf>,
}

impl UpdateResult {
    pub fn print_summary(&self) {
        if !self.modified_files.is_empty() {
            println!("Run `npm install` to install updated packages");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use check_updates_core::{Dependency, Version, VersionSpec};
    use std::io::Write;
    use std::str::FromStr;
    use tempfile::NamedTempFile;

    fn create_check(
        name: &str,
        spec_str: &str,
        path: PathBuf,
        target_version: &str,
        severity: UpdateSeverity,
    ) -> DependencyCheck {
        let target = Version::from_str(target_version).unwrap();
        DependencyCheck {
            dependency: Dependency {
                name: name.to_string(),
                version_spec: VersionSpec::parse(spec_str).unwrap(),
                source_file: path,
                line_number: 2,
                original_line: format!("\"{}\": \"{}\"", name, spec_str),
            },
            installed: Some(Version::from_str(spec_str.trim_start_matches('^').trim_start_matches('~')).unwrap()),
            in_range: Some(target.clone()),
            latest: target.clone(),
            target: Some(target.clone()),
            target_spec: Some(VersionSpec::parse(&format!("^{}", target_version)).unwrap()),
            severity: Some(severity),
            force_spec: Some(VersionSpec::parse(&format!("^{}", target_version)).unwrap()),
        }
    }

    #[test]
    fn test_update_patch_only() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"{{
  "dependencies": {{
    "express": "^4.18.0",
    "lodash": "^4.17.0"
  }}
}}"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            create_check("express", "^4.18.0", temp_path.clone(), "4.18.2", UpdateSeverity::Patch),
            create_check("lodash", "^4.17.0", temp_path.clone(), "4.18.0", UpdateSeverity::Minor),
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, false, false)?;

        let content = fs::read_to_string(&temp_path)?;
        assert!(content.contains("4.18.2"), "express should be updated: {}", content);
        assert!(!content.contains("4.18.0") || content.contains("^4.18.0"), "lodash should NOT be updated");

        Ok(())
    }
}
