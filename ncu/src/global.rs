use anyhow::Result;
use check_updates_core::{UpdateSeverity, Version};
use std::process::Command;
use std::str::FromStr;

/// Source of a globally installed package
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GlobalSource {
    Npm,
}

impl std::fmt::Display for GlobalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobalSource::Npm => write!(f, "npm"),
        }
    }
}

/// A globally installed package
#[derive(Debug, Clone)]
pub struct GlobalPackage {
    pub name: String,
    pub installed_version: Version,
    pub source: GlobalSource,
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

/// Discovers globally installed packages
#[derive(Default)]
pub struct GlobalPackageDiscovery;

impl GlobalPackageDiscovery {
    pub fn new() -> Self {
        Self
    }

    /// Discover all globally installed packages
    pub fn discover(&self) -> Vec<GlobalPackage> {
        self.discover_npm_packages().unwrap_or_default()
    }

    /// Discover npm global packages using `npm list -g --json --depth=0`
    fn discover_npm_packages(&self) -> Result<Vec<GlobalPackage>> {
        let output = Command::new("npm")
            .args(["list", "-g", "--json", "--depth=0"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                self.parse_npm_global_json(&String::from_utf8_lossy(&output.stdout))
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Parse `npm list -g --json --depth=0` output
    /// Format: {"dependencies": {"pkg": {"version": "X.Y.Z"}, ...}}
    fn parse_npm_global_json(&self, json_str: &str) -> Result<Vec<GlobalPackage>> {
        let data: serde_json::Value = serde_json::from_str(json_str)?;
        let mut packages = Vec::new();

        if let Some(deps) = data.get("dependencies").and_then(|v| v.as_object()) {
            for (name, dep_data) in deps {
                if let Some(version_str) = dep_data.get("version").and_then(|v| v.as_str())
                    && let Ok(version) = Version::from_str(version_str)
                {
                    packages.push(GlobalPackage {
                        name: name.clone(),
                        installed_version: version,
                        source: GlobalSource::Npm,
                    });
                }
            }
        }

        Ok(packages)
    }
}

/// Generate upgrade commands for outdated global packages
pub fn generate_upgrade_commands(checks: &[GlobalCheck]) -> Vec<String> {
    let outdated: Vec<&GlobalCheck> = checks.iter().filter(|c| c.has_update).collect();

    if outdated.is_empty() {
        return Vec::new();
    }

    let package_names: Vec<&str> = outdated.iter().map(|c| c.package.name.as_str()).collect();
    vec![format!("npm install -g {}", package_names.join(" "))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_npm_global_json() {
        let discovery = GlobalPackageDiscovery::new();
        let json = r#"{
            "version": "10.2.0",
            "name": "lib",
            "dependencies": {
                "typescript": {
                    "version": "5.4.5"
                },
                "prettier": {
                    "version": "3.2.5"
                },
                "@angular/cli": {
                    "version": "17.3.8"
                }
            }
        }"#;
        let packages = discovery.parse_npm_global_json(json).expect("should parse");
        assert_eq!(packages.len(), 3);

        let ts = packages.iter().find(|p| p.name == "typescript").expect("should find typescript");
        assert_eq!(ts.installed_version.to_string(), "5.4.5");
        assert_eq!(ts.source, GlobalSource::Npm);

        let angular = packages.iter().find(|p| p.name == "@angular/cli").expect("should find @angular/cli");
        assert_eq!(angular.installed_version.to_string(), "17.3.8");
    }

    #[test]
    fn test_parse_npm_global_json_empty() {
        let discovery = GlobalPackageDiscovery::new();
        let json = r#"{"version": "10.2.0", "name": "lib"}"#;
        let packages = discovery.parse_npm_global_json(json).expect("should parse");
        assert!(packages.is_empty());
    }

    #[test]
    fn test_global_source_display() {
        assert_eq!(GlobalSource::Npm.to_string(), "npm");
    }

    #[test]
    fn test_update_severity() {
        let pkg = GlobalPackage {
            name: "test".to_string(),
            installed_version: Version::from_str("1.0.0").expect("valid version"),
            source: GlobalSource::Npm,
        };

        let check = GlobalCheck {
            package: pkg.clone(),
            latest: Version::from_str("2.0.0").expect("valid version"),
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Major));

        let check = GlobalCheck {
            package: pkg.clone(),
            latest: Version::from_str("1.1.0").expect("valid version"),
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Minor));

        let check = GlobalCheck {
            package: pkg.clone(),
            latest: Version::from_str("1.0.1").expect("valid version"),
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Patch));

        let check = GlobalCheck {
            package: pkg,
            latest: Version::from_str("1.0.0").expect("valid version"),
            has_update: false,
        };
        assert_eq!(check.update_severity(), None);
    }

    #[test]
    fn test_generate_upgrade_commands() {
        let checks = vec![
            GlobalCheck {
                package: GlobalPackage {
                    name: "typescript".to_string(),
                    installed_version: Version::from_str("5.4.5").expect("valid version"),
                    source: GlobalSource::Npm,
                },
                latest: Version::from_str("5.6.3").expect("valid version"),
                has_update: true,
            },
            GlobalCheck {
                package: GlobalPackage {
                    name: "prettier".to_string(),
                    installed_version: Version::from_str("3.2.5").expect("valid version"),
                    source: GlobalSource::Npm,
                },
                latest: Version::from_str("3.2.5").expect("valid version"),
                has_update: false,
            },
        ];

        let commands = generate_upgrade_commands(&checks);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0], "npm install -g typescript");
    }

    #[test]
    fn test_generate_upgrade_commands_none_outdated() {
        let checks = vec![GlobalCheck {
            package: GlobalPackage {
                name: "typescript".to_string(),
                installed_version: Version::from_str("5.6.3").expect("valid version"),
                source: GlobalSource::Npm,
            },
            latest: Version::from_str("5.6.3").expect("valid version"),
            has_update: false,
        }];

        let commands = generate_upgrade_commands(&checks);
        assert!(commands.is_empty());
    }
}
