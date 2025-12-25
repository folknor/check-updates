use crate::global::UpgradeCommand;
use crate::version::Version;
use anyhow::Result;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

/// Information about an installed uv-managed Python version
#[derive(Debug, Clone)]
pub struct UvPythonInfo {
    /// Full implementation name (e.g., "cpython-3.11.5-linux-x86_64-gnu")
    pub full_name: String,
    /// Python version (e.g., "3.11.5")
    pub version: Version,
    /// Installation path (if installed, otherwise None)
    pub path: Option<PathBuf>,
    /// Whether this is installed or just available for download
    pub is_installed: bool,
    /// Python implementation type (cpython, pypy, graalpy, etc.)
    pub implementation: String,
}

/// Result of checking a Python series for updates
#[derive(Debug, Clone)]
pub struct UvPythonCheck {
    /// The major.minor series (e.g., "3.11")
    pub series: String,
    /// Currently installed version in this series
    pub installed_version: Version,
    /// Latest available patch in this series from endoflife.date
    pub latest_version: Version,
    /// Whether an update is available
    pub has_update: bool,
    /// Full uv python info for the installed version
    pub python_info: UvPythonInfo,
}

impl UvPythonCheck {
    /// Get update severity for coloring (patch or minor)
    pub fn is_patch_update(&self) -> bool {
        self.has_update
            && self.latest_version.major == self.installed_version.major
            && self.latest_version.minor == self.installed_version.minor
    }
}

/// Response from endoflife.date API for a Python cycle
#[derive(Debug, Deserialize)]
struct PythonCycle {
    cycle: String,      // "3.11", "3.12", etc.
    latest: String,     // "3.11.14", "3.12.12", etc.
}

/// Discovery and checking for uv-managed Python installations
pub struct UvPythonDiscovery {}

impl UvPythonDiscovery {
    pub fn new() -> Self {
        Self {}
    }

    /// Parse `uv python list` output to find installed Python versions
    fn parse_uv_python_list(&self, output: &str) -> Result<Vec<UvPythonInfo>> {
        let mut versions = Vec::new();

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            let full_name = parts[0];

            // Skip if not installed (has "<download available>" suffix)
            let is_installed = !line.contains("<download available>");

            // Parse: "cpython-3.11.5-linux-x86_64-gnu"
            let name_parts: Vec<&str> = full_name.split('-').collect();
            if name_parts.len() < 2 {
                continue;
            }

            let implementation = name_parts[0]; // "cpython", "pypy", etc.
            let version_str = name_parts[1]; // "3.11.5"

            // Skip freethreaded variants for simplicity
            if full_name.contains("+freethreaded") {
                continue;
            }

            // Skip non-cpython for now (can extend later)
            if implementation != "cpython" {
                continue;
            }

            if let Ok(version) = Version::from_str(version_str) {
                let path = if is_installed && parts.len() > 1 {
                    Some(PathBuf::from(parts[1]))
                } else {
                    None
                };

                versions.push(UvPythonInfo {
                    full_name: full_name.to_string(),
                    version,
                    path,
                    is_installed,
                    implementation: implementation.to_string(),
                });
            }
        }

        Ok(versions)
    }

    /// Fetch latest versions for all Python series from endoflife.date
    async fn fetch_latest_python_versions(&self) -> Result<HashMap<String, Version>> {
        let url = "https://endoflife.date/api/python.json";

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;

        let response = client.get(url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to fetch Python version data");
        }

        let cycles: Vec<PythonCycle> = response.json().await?;

        // Build map of "3.11" -> "3.11.14", "3.12" -> "3.12.12", etc.
        let mut versions = HashMap::new();
        for cycle in cycles {
            if cycle.cycle.starts_with("3.") {
                // Only Python 3.x
                if let Ok(version) = Version::from_str(&cycle.latest) {
                    versions.insert(cycle.cycle.clone(), version);
                }
            }
        }

        Ok(versions)
    }

    /// Discover installed uv Python versions and check for updates
    pub async fn discover_and_check(&self) -> Result<Vec<UvPythonCheck>> {
        // 1. Run `uv python list`
        let output = Command::new("uv").args(["python", "list"]).output();

        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return Ok(Vec::new()), // uv not installed or failed
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let installed = self.parse_uv_python_list(&stdout)?;

        // Filter to only installed versions
        let installed: Vec<_> = installed
            .into_iter()
            .filter(|v| v.is_installed)
            .collect();

        if installed.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Fetch latest versions per series from endoflife.date
        let latest_versions = self.fetch_latest_python_versions().await?;

        // 3. Build checks grouped by series
        let mut checks = Vec::new();
        let mut seen_series = HashSet::new();

        for python in installed {
            let series = format!("{}.{}", python.version.major, python.version.minor);

            // Only check each series once (if multiple same series installed)
            if seen_series.contains(&series) {
                continue;
            }
            seen_series.insert(series.clone());

            if let Some(latest) = latest_versions.get(&series) {
                let has_update = latest > &python.version;

                checks.push(UvPythonCheck {
                    series: series.clone(),
                    installed_version: python.version.clone(),
                    latest_version: latest.clone(),
                    has_update,
                    python_info: python,
                });
            }
        }

        Ok(checks)
    }
}

/// Generate upgrade commands for outdated uv Python versions
pub fn generate_uv_python_upgrade_commands(checks: &[UvPythonCheck]) -> Vec<UpgradeCommand> {
    let mut commands = Vec::new();

    let outdated: Vec<_> = checks.iter().filter(|c| c.has_update).collect();

    if outdated.is_empty() {
        return commands;
    }

    // Generate: uv python install 3.11.14
    for check in outdated {
        commands.push(UpgradeCommand::Command(format!(
            "uv python install {}",
            check.latest_version
        )));
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uv_python_list() {
        let discovery = UvPythonDiscovery::new();
        let output = r#"cpython-3.11.5-linux-x86_64-gnu     /home/user/.local/share/uv/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11
cpython-3.12.2-linux-x86_64-gnu     /usr/bin/python3.12
cpython-3.13.0-linux-x86_64-gnu     <download available>
"#;
        let versions = discovery.parse_uv_python_list(output).unwrap();

        // Should have 3 versions total (2 installed, 1 download available)
        assert_eq!(versions.len(), 3);

        assert_eq!(versions[0].version.to_string(), "3.11.5");
        assert_eq!(versions[0].implementation, "cpython");
        assert!(versions[0].is_installed);
        assert!(versions[0].path.is_some());

        assert_eq!(versions[1].version.to_string(), "3.12.2");
        assert!(versions[1].is_installed);
        assert!(versions[1].path.is_some());

        assert_eq!(versions[2].version.to_string(), "3.13.0");
        assert!(!versions[2].is_installed);
        assert!(versions[2].path.is_none());
    }

    #[test]
    fn test_parse_uv_python_list_skip_freethreaded() {
        let discovery = UvPythonDiscovery::new();
        let output = r#"cpython-3.13.0+freethreaded-linux-x86_64-gnu     /path/to/python
cpython-3.12.2-linux-x86_64-gnu     /usr/bin/python3.12
"#;
        let versions = discovery.parse_uv_python_list(output).unwrap();

        // Should only have 1 version (freethreaded skipped)
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "3.12.2");
    }

    #[test]
    fn test_parse_uv_python_list_skip_non_cpython() {
        let discovery = UvPythonDiscovery::new();
        let output = r#"pypy-3.10.14-linux-x86_64-gnu     /path/to/pypy
cpython-3.12.2-linux-x86_64-gnu     /usr/bin/python3.12
"#;
        let versions = discovery.parse_uv_python_list(output).unwrap();

        // Should only have cpython version
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].implementation, "cpython");
        assert_eq!(versions[0].version.to_string(), "3.12.2");
    }

    #[test]
    fn test_generate_upgrade_commands() {
        let checks = vec![
            UvPythonCheck {
                series: "3.11".to_string(),
                installed_version: Version::from_str("3.11.5").unwrap(),
                latest_version: Version::from_str("3.11.14").unwrap(),
                has_update: true,
                python_info: UvPythonInfo {
                    full_name: "cpython-3.11.5-linux-x86_64-gnu".to_string(),
                    version: Version::from_str("3.11.5").unwrap(),
                    path: None,
                    is_installed: true,
                    implementation: "cpython".to_string(),
                },
            },
            UvPythonCheck {
                series: "3.12".to_string(),
                installed_version: Version::from_str("3.12.2").unwrap(),
                latest_version: Version::from_str("3.12.12").unwrap(),
                has_update: true,
                python_info: UvPythonInfo {
                    full_name: "cpython-3.12.2-linux-x86_64-gnu".to_string(),
                    version: Version::from_str("3.12.2").unwrap(),
                    path: None,
                    is_installed: true,
                    implementation: "cpython".to_string(),
                },
            },
        ];

        let commands = generate_uv_python_upgrade_commands(&checks);
        assert_eq!(commands.len(), 2);

        match &commands[0] {
            UpgradeCommand::Command(cmd) => {
                assert_eq!(cmd, "uv python install 3.11.14");
            }
            _ => panic!("Expected Command"),
        }

        match &commands[1] {
            UpgradeCommand::Command(cmd) => {
                assert_eq!(cmd, "uv python install 3.12.12");
            }
            _ => panic!("Expected Command"),
        }
    }

    #[test]
    fn test_is_patch_update() {
        let check = UvPythonCheck {
            series: "3.11".to_string(),
            installed_version: Version::from_str("3.11.5").unwrap(),
            latest_version: Version::from_str("3.11.14").unwrap(),
            has_update: true,
            python_info: UvPythonInfo {
                full_name: "cpython-3.11.5-linux-x86_64-gnu".to_string(),
                version: Version::from_str("3.11.5").unwrap(),
                path: None,
                is_installed: true,
                implementation: "cpython".to_string(),
            },
        };

        assert!(check.is_patch_update());

        // No update
        let check_no_update = UvPythonCheck {
            series: "3.11".to_string(),
            installed_version: Version::from_str("3.11.14").unwrap(),
            latest_version: Version::from_str("3.11.14").unwrap(),
            has_update: false,
            python_info: UvPythonInfo {
                full_name: "cpython-3.11.14-linux-x86_64-gnu".to_string(),
                version: Version::from_str("3.11.14").unwrap(),
                path: None,
                is_installed: true,
                implementation: "cpython".to_string(),
            },
        };

        assert!(!check_no_update.is_patch_update());
    }
}
