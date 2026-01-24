use check_updates_core::{UpdateSeverity, Version};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;

/// Source of a globally installed package
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GlobalSource {
    Uv,
    Pipx,
    PipUser,
}

impl std::fmt::Display for GlobalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobalSource::Uv => write!(f, "uv"),
            GlobalSource::Pipx => write!(f, "pipx"),
            GlobalSource::PipUser => write!(f, "pip"),
        }
    }
}

/// A globally installed package
#[derive(Debug, Clone)]
pub struct GlobalPackage {
    pub name: String,
    pub installed_version: Version,
    pub source: GlobalSource,
    /// Python version (only set for pip --user packages)
    pub python_version: Option<String>,
}

/// Result of checking a global package
#[derive(Debug, Clone)]
pub struct GlobalCheck {
    pub package: GlobalPackage,
    pub latest: Version,
    pub has_update: bool,
}

impl GlobalCheck {
    /// Get update severity for coloring
    pub fn update_severity(&self) -> Option<UpdateSeverity> {
        if !self.has_update {
            return None;
        }
        let current = &self.package.installed_version;
        let target = &self.latest;

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

/// Discovers globally installed packages from various sources
pub struct GlobalPackageDiscovery {
    _include_prerelease: bool,
}

impl GlobalPackageDiscovery {
    pub fn new(include_prerelease: bool) -> Self {
        Self {
            _include_prerelease: include_prerelease,
        }
    }

    /// Discover all globally installed packages
    pub fn discover(&self) -> Vec<GlobalPackage> {
        let mut packages = Vec::new();

        // Try each source, silently skip if not available
        packages.extend(self.discover_uv_tools().unwrap_or_default());
        packages.extend(self.discover_pipx_packages().unwrap_or_default());
        packages.extend(self.discover_pip_user_packages().unwrap_or_default());

        packages
    }

    /// Discover uv tools using `uv tool list`
    fn discover_uv_tools(&self) -> Result<Vec<GlobalPackage>> {
        let output = Command::new("uv").args(["tool", "list"]).output();

        match output {
            Ok(output) if output.status.success() => {
                self.parse_uv_tool_list(&String::from_utf8_lossy(&output.stdout))
            }
            _ => Ok(Vec::new()), // uv not installed or failed, skip silently
        }
    }

    /// Parse output of `uv tool list`
    /// Format: "package_name vX.Y.Z" or "package_name X.Y.Z"
    /// May also have lines starting with "-" for entry points (skip these)
    fn parse_uv_tool_list(&self, output: &str) -> Result<Vec<GlobalPackage>> {
        let mut packages = Vec::new();

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('-') {
                continue;
            }

            // Parse "name vX.Y.Z" or "name X.Y.Z" format
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[0].to_string();
                let version_str = parts[1].trim_start_matches('v');

                if let Ok(version) = Version::from_str(version_str) {
                    packages.push(GlobalPackage {
                        name,
                        installed_version: version,
                        source: GlobalSource::Uv,
                        python_version: None,
                    });
                }
            }
        }

        Ok(packages)
    }

    /// Discover pipx packages
    fn discover_pipx_packages(&self) -> Result<Vec<GlobalPackage>> {
        // Try `pipx list --json` for structured output
        let output = Command::new("pipx").args(["list", "--json"]).output();

        match output {
            Ok(output) if output.status.success() => {
                self.parse_pipx_json(&String::from_utf8_lossy(&output.stdout))
            }
            _ => {
                // Fall back to scanning ~/.local/pipx/venvs/
                self.discover_pipx_from_directory()
            }
        }
    }

    /// Parse pipx list --json output
    fn parse_pipx_json(&self, json_str: &str) -> Result<Vec<GlobalPackage>> {
        let data: serde_json::Value = serde_json::from_str(json_str)?;
        let mut packages = Vec::new();

        if let Some(venvs) = data.get("venvs").and_then(|v| v.as_object()) {
            for (name, venv_data) in venvs {
                if let Some(version_str) = venv_data
                    .pointer("/metadata/main_package/package_version")
                    .and_then(|v| v.as_str())
                {
                    if let Ok(version) = Version::from_str(version_str) {
                        packages.push(GlobalPackage {
                            name: name.clone(),
                            installed_version: version,
                            source: GlobalSource::Pipx,
                            python_version: None,
                        });
                    }
                }
            }
        }

        Ok(packages)
    }

    /// Fall back: scan ~/.local/pipx/venvs/ directory
    fn discover_pipx_from_directory(&self) -> Result<Vec<GlobalPackage>> {
        let pipx_dir = dirs::home_dir()
            .map(|h| h.join(".local/pipx/venvs"))
            .filter(|p| p.exists());

        let Some(pipx_dir) = pipx_dir else {
            return Ok(Vec::new());
        };

        let mut packages = Vec::new();

        for entry in fs::read_dir(&pipx_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();

                // Try to get version from the package's metadata
                if let Some(version) = self.get_pipx_package_version(&entry.path(), &name) {
                    packages.push(GlobalPackage {
                        name,
                        installed_version: version,
                        source: GlobalSource::Pipx,
                        python_version: None,
                    });
                }
            }
        }

        Ok(packages)
    }

    /// Get version of a pipx package by reading its dist-info
    fn get_pipx_package_version(&self, venv_path: &Path, package_name: &str) -> Option<Version> {
        // Look in the venv's site-packages for the dist-info
        let site_packages = venv_path.join("lib");

        if !site_packages.exists() {
            return None;
        }

        // Find the python directory (e.g., python3.11)
        let python_dir = fs::read_dir(&site_packages)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("python"))?;

        let actual_site_packages = python_dir.path().join("site-packages");
        if !actual_site_packages.exists() {
            return None;
        }

        // Look for the dist-info directory
        let normalized_name = package_name.to_lowercase().replace('-', "_");
        for entry in fs::read_dir(&actual_site_packages).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".dist-info") {
                let dist_name = name
                    .strip_suffix(".dist-info")?
                    .to_lowercase()
                    .replace('-', "_");
                // Check if this dist-info matches our package
                if dist_name.starts_with(&normalized_name) {
                    if let Some((_, version)) = self.parse_dist_info_name(&name) {
                        return Some(version);
                    }
                }
            }
        }

        None
    }

    /// Discover pip --user packages from ~/.local/lib/python3.x/site-packages/
    /// Now tracks which Python version each package belongs to
    fn discover_pip_user_packages(&self) -> Result<Vec<GlobalPackage>> {
        let user_lib = dirs::home_dir().map(|h| h.join(".local/lib"));

        let Some(user_lib) = user_lib else {
            return Ok(Vec::new());
        };

        if !user_lib.exists() {
            return Ok(Vec::new());
        }

        let mut packages = Vec::new();

        // Find all python3.x directories and collect them sorted
        let mut python_dirs: Vec<_> = fs::read_dir(&user_lib)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with("python3.") || name.starts_with("python2.")
            })
            .collect();

        // Sort by version descending so newer Python versions come first
        python_dirs.sort_by(|a, b| {
            let a_name = a.file_name().to_string_lossy().to_string();
            let b_name = b.file_name().to_string_lossy().to_string();
            b_name.cmp(&a_name)
        });

        // Track seen packages to avoid duplicates across Python versions
        let mut seen_packages: HashSet<String> = HashSet::new();

        for entry in python_dirs {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            // Extract version like "3.11" from "python3.11"
            let python_version = dir_name.strip_prefix("python").unwrap_or(&dir_name);

            let site_packages = entry.path().join("site-packages");
            if site_packages.exists() {
                packages.extend(self.parse_site_packages(
                    &site_packages,
                    python_version,
                    &mut seen_packages,
                )?);
            }
        }

        Ok(packages)
    }

    /// Parse a site-packages directory for installed packages
    fn parse_site_packages(
        &self,
        site_packages: &Path,
        python_version: &str,
        seen: &mut HashSet<String>,
    ) -> Result<Vec<GlobalPackage>> {
        let mut packages = Vec::new();

        // Look for .dist-info directories
        for entry in fs::read_dir(site_packages)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();

            if name.ends_with(".dist-info") {
                // Parse: "package_name-1.2.3.dist-info"
                if let Some((pkg_name, version)) = self.parse_dist_info_name(&name) {
                    // Normalize package name for deduplication
                    let normalized = pkg_name.to_lowercase().replace('-', "_");

                    // Skip if we've already seen this package (from another Python version)
                    if seen.contains(&normalized) {
                        continue;
                    }
                    seen.insert(normalized);

                    packages.push(GlobalPackage {
                        name: pkg_name,
                        installed_version: version,
                        source: GlobalSource::PipUser,
                        python_version: Some(python_version.to_string()),
                    });
                }
            }
        }

        Ok(packages)
    }

    /// Parse a dist-info directory name to extract package name and version
    /// Format: "package_name-1.2.3.dist-info"
    fn parse_dist_info_name(&self, name: &str) -> Option<(String, Version)> {
        let without_suffix = name.strip_suffix(".dist-info")?;

        // Find the last hyphen that separates name from version
        // Version always starts with a digit
        let mut split_idx = None;
        for (i, c) in without_suffix.char_indices().rev() {
            if c == '-' {
                // Check if what follows is a version (starts with digit)
                if without_suffix[i + 1..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
                {
                    split_idx = Some(i);
                    break;
                }
            }
        }

        let idx = split_idx?;
        let pkg_name = &without_suffix[..idx];
        let version_str = &without_suffix[idx + 1..];

        let version = Version::from_str(version_str).ok()?;
        Some((pkg_name.to_string(), version))
    }
}

/// Group checks by source for upgrade command generation
pub fn group_by_source(checks: &[GlobalCheck]) -> HashMap<GlobalSource, Vec<&GlobalCheck>> {
    checks
        .iter()
        .filter(|c| c.has_update)
        .fold(HashMap::new(), |mut acc, check| {
            acc.entry(check.package.source.clone())
                .or_insert_with(Vec::new)
                .push(check);
            acc
        })
}

/// Check if a Python version is available on the system
pub fn is_python_available(version: &str) -> bool {
    // Try python3.X --version
    let cmd = format!("python{}", version);
    Command::new(&cmd)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get the user's home directory path for display
fn get_pip_user_path(python_version: &str) -> String {
    dirs::home_dir()
        .map(|h| h.join(format!(".local/lib/python{}", python_version)))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| format!("~/.local/lib/python{}", python_version))
}

/// An upgrade command or a comment (for unavailable Python versions)
#[derive(Debug, Clone)]
pub enum UpgradeCommand {
    /// A shell command to run
    Command(String),
    /// A comment (Python version not available)
    Comment(String),
}

/// Generate upgrade commands for each source
pub fn generate_upgrade_commands(checks: &[GlobalCheck]) -> Vec<UpgradeCommand> {
    let updates_by_source = group_by_source(checks);
    let mut commands = Vec::new();

    if updates_by_source.contains_key(&GlobalSource::Uv) {
        commands.push(UpgradeCommand::Command("uv tool upgrade --all".to_string()));
    }

    if updates_by_source.contains_key(&GlobalSource::Pipx) {
        commands.push(UpgradeCommand::Command("pipx upgrade-all".to_string()));
    }

    if let Some(pip_updates) = updates_by_source.get(&GlobalSource::PipUser) {
        // Group pip packages by Python version
        let mut by_python: std::collections::BTreeMap<String, Vec<&str>> =
            std::collections::BTreeMap::new();

        for check in pip_updates {
            let py_version = check
                .package
                .python_version
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            by_python
                .entry(py_version)
                .or_insert_with(Vec::new)
                .push(check.package.name.as_str());
        }

        // Generate a command for each Python version
        for (py_version, package_names) in by_python {
            if is_python_available(&py_version) {
                commands.push(UpgradeCommand::Command(format!(
                    "python{} -m pip install --user --upgrade {}",
                    py_version,
                    package_names.join(" ")
                )));
            } else {
                let path = get_pip_user_path(&py_version);
                commands.push(UpgradeCommand::Comment(format!(
                    "Python {} is no longer installed. Consider removing {} if nothing uses it.",
                    py_version, path
                )));
            }
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uv_tool_list() {
        let discovery = GlobalPackageDiscovery::new(false);
        let output = r#"ruff v0.14.10
    - ruff
ty v0.0.5
    - ty
"#;
        let packages = discovery.parse_uv_tool_list(output).unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "ruff");
        assert_eq!(packages[0].installed_version.to_string(), "0.14.10");
        assert_eq!(packages[0].source, GlobalSource::Uv);
        assert_eq!(packages[1].name, "ty");
        assert_eq!(packages[1].installed_version.to_string(), "0.0.5");
    }

    #[test]
    fn test_parse_uv_tool_list_without_v_prefix() {
        let discovery = GlobalPackageDiscovery::new(false);
        let output = "black 24.10.0\n";
        let packages = discovery.parse_uv_tool_list(output).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "black");
        assert_eq!(packages[0].installed_version.to_string(), "24.10.0");
    }

    #[test]
    fn test_parse_pipx_json() {
        let discovery = GlobalPackageDiscovery::new(false);
        let json = r#"{
            "venvs": {
                "black": {
                    "metadata": {
                        "main_package": {
                            "package_version": "24.10.0"
                        }
                    }
                },
                "ruff": {
                    "metadata": {
                        "main_package": {
                            "package_version": "0.14.9"
                        }
                    }
                }
            }
        }"#;
        let packages = discovery.parse_pipx_json(json).unwrap();
        assert_eq!(packages.len(), 2);
        // Note: HashMap iteration order is not guaranteed, so we check by finding
        let black = packages.iter().find(|p| p.name == "black").unwrap();
        assert_eq!(black.installed_version.to_string(), "24.10.0");
        assert_eq!(black.source, GlobalSource::Pipx);
    }

    #[test]
    fn test_parse_dist_info_name() {
        let discovery = GlobalPackageDiscovery::new(false);

        // Simple case
        let result = discovery.parse_dist_info_name("requests-2.28.0.dist-info");
        assert!(result.is_some());
        let (name, version) = result.unwrap();
        assert_eq!(name, "requests");
        assert_eq!(version.to_string(), "2.28.0");

        // Package with hyphen in name
        let result = discovery.parse_dist_info_name("typing-extensions-4.12.2.dist-info");
        assert!(result.is_some());
        let (name, version) = result.unwrap();
        assert_eq!(name, "typing-extensions");
        assert_eq!(version.to_string(), "4.12.2");

        // Package with underscore
        let result = discovery.parse_dist_info_name("my_package-1.0.0.dist-info");
        assert!(result.is_some());
        let (name, version) = result.unwrap();
        assert_eq!(name, "my_package");
        assert_eq!(version.to_string(), "1.0.0");
    }

    #[test]
    fn test_global_source_display() {
        assert_eq!(GlobalSource::Uv.to_string(), "uv");
        assert_eq!(GlobalSource::Pipx.to_string(), "pipx");
        assert_eq!(GlobalSource::PipUser.to_string(), "pip");
    }

    #[test]
    fn test_update_severity() {
        let pkg = GlobalPackage {
            name: "test".to_string(),
            installed_version: Version::from_str("1.0.0").unwrap(),
            source: GlobalSource::Uv,
            python_version: None,
        };

        // Major update
        let check = GlobalCheck {
            package: pkg.clone(),
            latest: Version::from_str("2.0.0").unwrap(),
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Major));

        // Minor update
        let check = GlobalCheck {
            package: pkg.clone(),
            latest: Version::from_str("1.1.0").unwrap(),
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Minor));

        // Patch update
        let check = GlobalCheck {
            package: pkg.clone(),
            latest: Version::from_str("1.0.1").unwrap(),
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Patch));

        // No update
        let check = GlobalCheck {
            package: pkg,
            latest: Version::from_str("1.0.0").unwrap(),
            has_update: false,
        };
        assert_eq!(check.update_severity(), None);
    }

    #[test]
    fn test_generate_upgrade_commands() {
        let checks = vec![
            GlobalCheck {
                package: GlobalPackage {
                    name: "ruff".to_string(),
                    installed_version: Version::from_str("0.14.9").unwrap(),
                    source: GlobalSource::Uv,
                    python_version: None,
                },
                latest: Version::from_str("0.14.10").unwrap(),
                has_update: true,
            },
            GlobalCheck {
                package: GlobalPackage {
                    name: "black".to_string(),
                    installed_version: Version::from_str("24.1.0").unwrap(),
                    source: GlobalSource::Pipx,
                    python_version: None,
                },
                latest: Version::from_str("24.10.0").unwrap(),
                has_update: true,
            },
            GlobalCheck {
                package: GlobalPackage {
                    name: "requests".to_string(),
                    installed_version: Version::from_str("2.28.0").unwrap(),
                    source: GlobalSource::PipUser,
                    python_version: Some("3.11".to_string()),
                },
                latest: Version::from_str("2.32.3").unwrap(),
                has_update: true,
            },
            GlobalCheck {
                package: GlobalPackage {
                    name: "flask".to_string(),
                    installed_version: Version::from_str("2.3.3").unwrap(),
                    source: GlobalSource::PipUser,
                    python_version: Some("3.11".to_string()),
                },
                latest: Version::from_str("3.0.0").unwrap(),
                has_update: true,
            },
        ];

        let commands = generate_upgrade_commands(&checks);
        // Should have uv, pipx, and either a pip command or comment for 3.11
        assert!(commands.len() >= 3);

        // Check for uv command
        let has_uv = commands.iter().any(|c| matches!(c, UpgradeCommand::Command(s) if s == "uv tool upgrade --all"));
        assert!(has_uv, "Should have uv upgrade command");

        // Check for pipx command
        let has_pipx = commands.iter().any(|c| matches!(c, UpgradeCommand::Command(s) if s == "pipx upgrade-all"));
        assert!(has_pipx, "Should have pipx upgrade command");

        // Check for pip command or comment for Python 3.11
        let has_pip_311 = commands.iter().any(|c| {
            match c {
                UpgradeCommand::Command(s) => s.contains("python3.11") && s.contains("requests") && s.contains("flask"),
                UpgradeCommand::Comment(s) => s.contains("3.11"),
            }
        });
        assert!(has_pip_311, "Should have pip command or comment for Python 3.11");
    }
}
