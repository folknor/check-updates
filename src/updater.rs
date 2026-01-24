use crate::resolver::{DependencyCheck, UpdateSeverity};
use crate::detector::PackageManager;
use anyhow::{Context, Result};
use std::collections::{HashSet, HashMap};
use std::path::PathBuf;
use std::fs;

/// Updates dependency files with new versions
pub struct FileUpdater;

impl FileUpdater {
    pub fn new() -> Self {
        Self
    }

    /// Apply updates to dependency files based on severity filter
    /// - include_minor: false = patch only, true = patch + minor
    /// - force: true = all severities AND use absolute latest version
    pub fn apply_updates(
        &self,
        checks: &[DependencyCheck],
        include_minor: bool,
        force: bool,
    ) -> Result<UpdateResult> {
        let mut modified_files = HashSet::new();
        let mut package_file_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut package_managers = HashSet::new();

        // Group checks by file, filtering by severity
        let mut file_updates: HashMap<PathBuf, Vec<(&DependencyCheck, String)>> = HashMap::new();

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
                let new_version = spec.to_string();
                file_updates
                    .entry(check.dependency.source_file.clone())
                    .or_default()
                    .push((check, new_version));

                // Track which packages appear in which files
                package_file_map
                    .entry(check.dependency.name.clone())
                    .or_insert_with(Vec::new)
                    .push(check.dependency.source_file.clone());
            }
        }

        // Update each file
        for (file_path, updates) in file_updates {
            self.update_file(&file_path, &updates)
                .with_context(|| format!("Failed to update file: {}", file_path.display()))?;

            modified_files.insert(file_path.clone());

            // Detect package manager from file name
            if let Some(pm) = detect_package_manager(&file_path) {
                package_managers.insert(pm);
            }
        }

        // Find packages updated in multiple files
        let mut multi_file_packages: Vec<String> = package_file_map
            .iter()
            .filter_map(|(pkg, files)| {
                let unique_files: HashSet<_> = files.iter().collect();
                if unique_files.len() > 1 {
                    Some(pkg.clone())
                } else {
                    None
                }
            })
            .collect();
        multi_file_packages.sort();

        Ok(UpdateResult {
            modified_files,
            multi_file_packages,
            package_managers,
        })
    }

    /// Update a single file with the given dependency updates
    fn update_file(&self, file_path: &PathBuf, updates: &[(&DependencyCheck, String)]) -> Result<()> {
        // Read the entire file
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

        // Sort updates by line number in descending order to avoid offset issues
        let mut sorted_updates: Vec<_> = updates.iter().collect();
        sorted_updates.sort_by(|a, b| b.0.dependency.line_number.cmp(&a.0.dependency.line_number));

        // Apply each update
        for (check, new_version) in sorted_updates {
            let line_idx = check.dependency.line_number.saturating_sub(1);

            if line_idx >= lines.len() {
                continue; // Skip if line number is out of bounds
            }

            let original_line = &lines[line_idx];

            let updated_line = self.replace_version_in_line(
                original_line,
                &check.dependency.name,
                &check.dependency.version_spec.to_string(),
                new_version,
                file_path,
            )?;

            lines[line_idx] = updated_line;
        }

        // Write the file back
        let new_content = lines.join("\n");
        // Add trailing newline if original had one
        let new_content = if content.ends_with('\n') {
            format!("{}\n", new_content)
        } else {
            new_content
        };

        fs::write(file_path, new_content)
            .with_context(|| format!("Failed to write file: {}", file_path.display()))?;

        Ok(())
    }

    /// Replace version specification in a line
    fn replace_version_in_line(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
        file_path: &PathBuf,
    ) -> Result<String> {
        let file_name = file_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        // Determine file type and use appropriate replacement strategy
        if file_name.starts_with("requirements") || file_name.ends_with(".txt") {
            self.replace_in_requirements(line, package_name, old_spec, new_spec)
        } else if file_name == "pyproject.toml" {
            self.replace_in_pyproject(line, package_name, old_spec, new_spec)
        } else if file_name.starts_with("environment.") &&
                  (file_name.ends_with(".yml") || file_name.ends_with(".yaml")) {
            self.replace_in_conda(line, package_name, old_spec, new_spec)
        } else {
            // Default to requirements.txt style
            self.replace_in_requirements(line, package_name, old_spec, new_spec)
        }
    }

    /// Replace version in requirements.txt format
    fn replace_in_requirements(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
    ) -> Result<String> {
        // Format: package==1.0.0 or package>=1.0.0,<2.0.0 or package[extras]==1.0.0

        // Try exact match first
        if let Some(new_line) = line.replace(&format!("{}{}", package_name, old_spec),
                                              &format!("{}{}", package_name, new_spec))
                                    .into() {
            if new_line != line {
                return Ok(new_line);
            }
        }

        // Try with brackets (extras)
        if line.contains('[') {
            if let Some(bracket_start) = line.find('[') {
                if let Some(bracket_end) = line.find(']') {
                    let before_bracket = &line[..bracket_start];
                    let extras = &line[bracket_start..=bracket_end];
                    let after_bracket = &line[bracket_end + 1..];

                    if before_bracket.trim() == package_name {
                        let new_after = after_bracket.replace(old_spec, new_spec);
                        return Ok(format!("{}{}{}", before_bracket, extras, new_after));
                    }
                }
            }
        }

        // Fallback: simple string replacement
        Ok(line.replace(old_spec, new_spec))
    }

    /// Replace version in pyproject.toml format
    fn replace_in_pyproject(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
    ) -> Result<String> {
        // Format: package = "^1.0.0" or package = {version = "^1.0.0", ...}

        // Check if line contains the package name (case-insensitive for TOML keys)
        if line.to_lowercase().contains(&package_name.to_lowercase()) {
            // Replace the version spec, preserving quotes
            let result = line.replace(
                &format!("\"{}\"", old_spec),
                &format!("\"{}\"", new_spec)
            );
            if result != line {
                return Ok(result);
            }

            // Try single quotes
            let result = line.replace(
                &format!("'{}'", old_spec),
                &format!("'{}'", new_spec)
            );
            if result != line {
                return Ok(result);
            }
        }

        // Fallback
        Ok(line.replace(old_spec, new_spec))
    }

    /// Replace version in conda environment.yml format
    fn replace_in_conda(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
    ) -> Result<String> {
        // Format: - package==1.0.0 or - package=1.0.0

        // Conda uses = instead of == sometimes
        let conda_old_spec = old_spec.replace("==", "=");
        let conda_new_spec = new_spec.replace("==", "=");

        // Try with ==
        let result = line.replace(
            &format!("{}{}", package_name, old_spec),
            &format!("{}{}", package_name, new_spec)
        );
        if result != line {
            return Ok(result);
        }

        // Try with single =
        let result = line.replace(
            &format!("{}{}", package_name, conda_old_spec),
            &format!("{}{}", package_name, conda_new_spec)
        );
        if result != line {
            return Ok(result);
        }

        // Fallback
        Ok(line.replace(old_spec, new_spec))
    }
}

/// Detect package manager from file path
fn detect_package_manager(path: &PathBuf) -> Option<PackageManager> {
    let file_name = path.file_name()?.to_str()?;

    if file_name.starts_with("requirements") {
        Some(PackageManager::Pip)
    } else if file_name == "pyproject.toml" {
        // We'd need to read the file to determine if it's uv, poetry, or pdm
        // For now, default to uv as it's the most common
        Some(PackageManager::Uv)
    } else if file_name.starts_with("environment.") &&
              (file_name.ends_with(".yml") || file_name.ends_with(".yaml")) {
        Some(PackageManager::Conda)
    } else if file_name == "uv.lock" {
        Some(PackageManager::Uv)
    } else if file_name == "poetry.lock" {
        Some(PackageManager::Poetry)
    } else if file_name == "pdm.lock" {
        Some(PackageManager::Pdm)
    } else {
        None
    }
}

/// Result of applying updates
#[derive(Debug)]
pub struct UpdateResult {
    /// Files that were modified
    pub modified_files: HashSet<PathBuf>,
    /// Packages that were updated in multiple files
    pub multi_file_packages: Vec<String>,
    /// Package managers detected (for sync command suggestions)
    pub package_managers: HashSet<PackageManager>,
}

impl UpdateResult {
    /// Print post-update messages
    pub fn print_summary(&self) {
        if !self.multi_file_packages.is_empty() {
            println!(
                "\nNote: The following packages were updated in multiple files: {}",
                self.multi_file_packages.join(", ")
            );
        }

        for pm in &self.package_managers {
            let cmd = match pm {
                PackageManager::Pip => "pip install -r requirements.txt",
                PackageManager::Uv => "uv lock",
                PackageManager::Poetry => "poetry lock",
                PackageManager::Pdm => "pdm lock",
                PackageManager::Conda => "conda env update",
            };
            println!("Run {} to sync dependencies", cmd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_replace_in_requirements() {
        let updater = FileUpdater::new();

        // Test basic pinned version
        let result = updater.replace_in_requirements(
            "requests==2.28.0",
            "requests",
            "==2.28.0",
            "==2.32.3"
        ).unwrap();
        assert_eq!(result, "requests==2.32.3");

        // Test range version
        let result = updater.replace_in_requirements(
            "numpy>=1.24.0,<2.0.0",
            "numpy",
            ">=1.24.0,<2.0.0",
            ">=1.26.0,<2.0.0"
        ).unwrap();
        assert_eq!(result, "numpy>=1.26.0,<2.0.0");

        // Test with extras
        let result = updater.replace_in_requirements(
            "requests[security]==2.28.0",
            "requests",
            "==2.28.0",
            "==2.32.3"
        ).unwrap();
        assert_eq!(result, "requests[security]==2.32.3");
    }

    #[test]
    fn test_replace_in_pyproject() {
        let updater = FileUpdater::new();

        // Test with double quotes
        let result = updater.replace_in_pyproject(
            "requests = \"^2.28.0\"",
            "requests",
            "^2.28.0",
            "^2.32.3"
        ).unwrap();
        assert_eq!(result, "requests = \"^2.32.3\"");

        // Test with single quotes
        let result = updater.replace_in_pyproject(
            "numpy = '^1.24.0'",
            "numpy",
            "^1.24.0",
            "^1.26.0"
        ).unwrap();
        assert_eq!(result, "numpy = '^1.26.0'");
    }

    #[test]
    fn test_replace_in_conda() {
        let updater = FileUpdater::new();

        // Test with == operator
        let result = updater.replace_in_conda(
            "  - numpy==1.24.0",
            "numpy",
            "==1.24.0",
            "==1.26.0"
        ).unwrap();
        assert_eq!(result, "  - numpy==1.26.0");

        // Test with single = operator
        let result = updater.replace_in_conda(
            "  - requests=2.28.0",
            "requests",
            "==2.28.0",
            "==2.32.3"
        ).unwrap();
        assert_eq!(result, "  - requests=2.32.3");
    }

    #[test]
    fn test_detect_package_manager() {
        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/requirements.txt")),
            Some(PackageManager::Pip)
        );

        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/requirements-dev.txt")),
            Some(PackageManager::Pip)
        );

        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/pyproject.toml")),
            Some(PackageManager::Uv)
        );

        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/environment.yml")),
            Some(PackageManager::Conda)
        );

        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/poetry.lock")),
            Some(PackageManager::Poetry)
        );
    }

    #[test]
    fn test_update_file_integration() -> Result<()> {
        use crate::parsers::Dependency;
        use crate::version::{Version, VersionSpec};

        let updater = FileUpdater::new();

        // Create a temporary requirements.txt file
        let mut temp_file = NamedTempFile::new()?;
        writeln!(temp_file, "requests==2.28.0")?;
        writeln!(temp_file, "numpy>=1.24.0,<2.0.0")?;
        writeln!(temp_file, "flask==2.0.3")?;
        temp_file.flush()?;

        let temp_path = temp_file.path().to_path_buf();

        // Create mock dependency checks
        let check1 = DependencyCheck {
            dependency: Dependency {
                name: "requests".to_string(),
                version_spec: VersionSpec::Pinned(Version::new(2, 28, 0)),
                source_file: temp_path.clone(),
                line_number: 1,
                original_line: "requests==2.28.0".to_string(),
            },
            installed: Some(Version::new(2, 28, 0)),
            in_range: Some(Version::new(2, 32, 3)),
            latest: Version::new(2, 32, 3),
            target: Some(Version::new(2, 32, 3)),
            target_spec: Some(VersionSpec::Pinned(Version::new(2, 32, 3))),
            severity: Some(UpdateSeverity::Minor),
            force_spec: Some(VersionSpec::Pinned(Version::new(2, 32, 3))),
        };
        let check2 = DependencyCheck {
            dependency: Dependency {
                name: "flask".to_string(),
                version_spec: VersionSpec::Pinned(Version::new(2, 0, 3)),
                source_file: temp_path.clone(),
                line_number: 3,
                original_line: "flask==2.0.3".to_string(),
            },
            installed: Some(Version::new(2, 0, 3)),
            in_range: Some(Version::new(2, 3, 3)),
            latest: Version::new(2, 3, 3),
            target: Some(Version::new(2, 3, 3)),
            target_spec: Some(VersionSpec::Pinned(Version::new(2, 3, 3))),
            severity: Some(UpdateSeverity::Minor),
            force_spec: Some(VersionSpec::Pinned(Version::new(2, 3, 3))),
        };

        // Create updates with version strings
        let updates: Vec<(&DependencyCheck, String)> = vec![
            (&check1, "==2.32.3".to_string()),
            (&check2, "==2.3.3".to_string()),
        ];

        // Apply updates
        updater.update_file(&temp_path, &updates)?;

        // Read the updated file
        let updated_content = fs::read_to_string(&temp_path)?;
        let lines: Vec<&str> = updated_content.lines().collect();

        // Verify updates
        assert_eq!(lines[0], "requests==2.32.3");
        assert_eq!(lines[1], "numpy>=1.24.0,<2.0.0"); // Unchanged
        assert_eq!(lines[2], "flask==2.3.3");

        Ok(())
    }

    #[test]
    fn test_update_patch_only() -> Result<()> {
        use crate::parsers::Dependency;
        use crate::version::{Version, VersionSpec};

        let mut file = NamedTempFile::new()?;
        writeln!(file, "serde==1.0.0")?;
        writeln!(file, "tokio==1.0.0")?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            DependencyCheck {
                dependency: Dependency {
                    name: "serde".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: 1,
                    original_line: "serde==1.0.0".to_string(),
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 0, 200)),
                latest: Version::new(1, 0, 200),
                target: Some(Version::new(1, 0, 200)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
                severity: Some(UpdateSeverity::Patch),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
            },
            DependencyCheck {
                dependency: Dependency {
                    name: "tokio".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: 2,
                    original_line: "tokio==1.0.0".to_string(),
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 5, 0)),
                latest: Version::new(1, 5, 0),
                target: Some(Version::new(1, 5, 0)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
                severity: Some(UpdateSeverity::Minor),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
            },
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, false, false)?; // patch only

        let content = fs::read_to_string(&temp_path)?;
        assert!(content.contains("==1.0.200"), "serde should be updated: {}", content);
        assert!(!content.contains("==1.5.0"), "tokio should NOT be updated: {}", content);

        Ok(())
    }

    #[test]
    fn test_update_patch_and_minor() -> Result<()> {
        use crate::parsers::Dependency;
        use crate::version::{Version, VersionSpec};

        let mut file = NamedTempFile::new()?;
        writeln!(file, "serde==1.0.0")?;
        writeln!(file, "tokio==1.0.0")?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            DependencyCheck {
                dependency: Dependency {
                    name: "serde".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: 1,
                    original_line: "serde==1.0.0".to_string(),
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 0, 200)),
                latest: Version::new(1, 0, 200),
                target: Some(Version::new(1, 0, 200)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
                severity: Some(UpdateSeverity::Patch),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
            },
            DependencyCheck {
                dependency: Dependency {
                    name: "tokio".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: 2,
                    original_line: "tokio==1.0.0".to_string(),
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 5, 0)),
                latest: Version::new(1, 5, 0),
                target: Some(Version::new(1, 5, 0)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
                severity: Some(UpdateSeverity::Minor),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
            },
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, true, false)?; // patch + minor

        let content = fs::read_to_string(&temp_path)?;
        assert!(content.contains("==1.0.200"), "serde should be updated: {}", content);
        assert!(content.contains("==1.5.0"), "tokio should be updated: {}", content);

        Ok(())
    }
}
