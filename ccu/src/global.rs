use check_updates_core::{UpdateSeverity, Version};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

/// Source of a globally installed cargo crate
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GlobalSource {
    /// Installed from crates.io (or another registry)
    Registry,
    /// Installed from a git repository
    Git,
    /// Installed from a local path (cargo install --path)
    Path,
}

impl std::fmt::Display for GlobalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobalSource::Registry => write!(f, "crates.io"),
            GlobalSource::Git => write!(f, "git"),
            GlobalSource::Path => write!(f, "path"),
        }
    }
}

/// A globally installed cargo crate
#[derive(Debug, Clone)]
pub struct GlobalPackage {
    pub name: String,
    pub installed_version: Version,
    pub source: GlobalSource,
    pub binaries: Vec<String>,
    /// For git installs: the repo URL
    pub git_url: Option<String>,
    /// For git installs: the installed commit hash
    pub git_hash: Option<String>,
    /// For path installs: the local filesystem path
    pub local_path: Option<PathBuf>,
}

/// Result of checking a global package for updates
#[derive(Debug, Clone)]
pub struct GlobalCheck {
    pub package: GlobalPackage,
    /// For registry crates: the latest version on crates.io
    pub latest_version: Option<Version>,
    /// For git crates: the latest commit hash on the default branch
    pub latest_hash: Option<String>,
    /// For git/path crates: how many commits behind
    pub commits_behind: Option<u64>,
    /// For path crates: whether there are uncommitted local changes
    pub has_dirty_changes: bool,
    /// Whether an update is available
    pub has_update: bool,
}

impl GlobalCheck {
    /// Get update severity for coloring (registry crates only)
    pub fn update_severity(&self) -> Option<UpdateSeverity> {
        if !self.has_update {
            return None;
        }
        let latest = self.latest_version.as_ref()?;
        let current = &self.package.installed_version;

        if latest.major > current.major {
            Some(UpdateSeverity::Major)
        } else if latest.minor > current.minor {
            Some(UpdateSeverity::Minor)
        } else if latest.patch > current.patch {
            Some(UpdateSeverity::Patch)
        } else {
            None
        }
    }
}

/// Discovers globally installed cargo crates from ~/.cargo/.crates.toml
pub struct GlobalPackageDiscovery {}

impl GlobalPackageDiscovery {
    pub fn new() -> Self {
        Self {}
    }

    /// Parse ~/.cargo/.crates.toml and return discovered packages
    pub fn discover(&self) -> Result<Vec<GlobalPackage>> {
        let crates_toml = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".cargo/.crates.toml"))
            .filter(|p| p.exists());

        let Some(path) = crates_toml else {
            return Ok(Vec::new());
        };

        let contents = std::fs::read_to_string(&path)?;
        self.parse_crates_toml(&contents)
    }

    /// Parse the contents of .crates.toml
    ///
    /// Format:
    /// ```toml
    /// [v1]
    /// "bat 0.26.1 (registry+https://github.com/rust-lang/crates.io-index)" = ["bat"]
    /// "rtk 0.35.0 (git+https://github.com/rtk-ai/rtk#8a7106c8...)" = ["rtk"]
    /// "brokkr 0.1.0 (path+file:///home/folk/Programs/brokkr)" = ["brokkr"]
    /// ```
    fn parse_crates_toml(&self, contents: &str) -> Result<Vec<GlobalPackage>> {
        let parsed: toml::Value = toml::from_str(contents)?;
        let mut packages = Vec::new();

        let Some(v1) = parsed.get("v1").and_then(|v| v.as_table()) else {
            return Ok(packages);
        };

        for (key, value) in v1 {
            let binaries: Vec<String> = value
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            if let Some(pkg) = self.parse_crate_key(key, binaries) {
                packages.push(pkg);
            }
        }

        Ok(packages)
    }

    /// Parse a single crate key like:
    /// "bat 0.26.1 (registry+https://github.com/rust-lang/crates.io-index)"
    /// "rtk 0.35.0 (git+https://github.com/rtk-ai/rtk#8a7106c8f2996ebc75b38a71c5f342f17811ce39)"
    /// "brokkr 0.1.0 (path+file:///home/folk/Programs/brokkr)"
    fn parse_crate_key(&self, key: &str, binaries: Vec<String>) -> Option<GlobalPackage> {
        // Split: "name version (source)"
        let paren_start = key.find('(')?;
        let paren_end = key.rfind(')')?;
        let source_str = &key[paren_start + 1..paren_end];
        let name_version = key[..paren_start].trim();

        // Split name and version
        let space_idx = name_version.rfind(' ')?;
        let name = &name_version[..space_idx];
        let version_str = &name_version[space_idx + 1..];

        let version = Version::from_str(version_str).ok()?;

        if source_str.starts_with("registry+") {
            Some(GlobalPackage {
                name: name.to_string(),
                installed_version: version,
                source: GlobalSource::Registry,
                binaries,
                git_url: None,
                git_hash: None,
                local_path: None,
            })
        } else if source_str.starts_with("git+") {
            // Parse: "git+https://github.com/user/repo#commithash"
            let git_part = &source_str[4..]; // strip "git+"
            let (url, hash) = if let Some(hash_idx) = git_part.find('#') {
                (
                    git_part[..hash_idx].to_string(),
                    Some(git_part[hash_idx + 1..].to_string()),
                )
            } else {
                (git_part.to_string(), None)
            };

            Some(GlobalPackage {
                name: name.to_string(),
                installed_version: version,
                source: GlobalSource::Git,
                binaries,
                git_url: Some(url),
                git_hash: hash,
                local_path: None,
            })
        } else if source_str.starts_with("path+") {
            // Parse: "path+file:///home/user/project"
            let path_str = source_str
                .strip_prefix("path+file://")
                .unwrap_or(&source_str[5..]);
            let path = PathBuf::from(path_str);

            // Only include if the path still exists and is a git repo
            if path.join(".git").exists() {
                Some(GlobalPackage {
                    name: name.to_string(),
                    installed_version: version,
                    source: GlobalSource::Path,
                    binaries,
                    git_url: None,
                    git_hash: None,
                    local_path: Some(path),
                })
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// Check git repositories for newer commits
pub async fn check_git_updates(packages: &[GlobalPackage]) -> HashMap<String, GitStatus> {
    let client = reqwest::Client::builder()
        .user_agent("cargo-check-updates/0.1.0")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut results = HashMap::new();

    for pkg in packages {
        if pkg.source != GlobalSource::Git {
            continue;
        }
        let Some(ref url) = pkg.git_url else {
            continue;
        };
        let Some(ref installed_hash) = pkg.git_hash else {
            continue;
        };

        if let Some((owner, repo)) = parse_github_url(url) {
            if let Some(status) = check_github_repo(&client, &owner, &repo, installed_hash).await {
                results.insert(pkg.name.clone(), status);
            }
        }
    }

    results
}

/// Status of a git-installed crate
#[derive(Debug, Clone)]
pub struct GitStatus {
    /// Latest commit hash on the default branch
    pub latest_hash: String,
    /// How many commits the installed version is behind
    pub commits_behind: u64,
}

/// Extract owner/repo from a GitHub URL
fn parse_github_url(url: &str) -> Option<(String, String)> {
    // Handle: https://github.com/owner/repo or https://github.com/owner/repo.git
    let url = url.trim_end_matches(".git");
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() >= 2 {
        let repo = parts[parts.len() - 1];
        let owner = parts[parts.len() - 2];
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    None
}

/// Query GitHub API to check if a commit is behind HEAD
async fn check_github_repo(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    installed_hash: &str,
) -> Option<GitStatus> {
    // Use the compare API: compare installed_hash...HEAD
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/compare/{installed_hash}...HEAD"
    );

    let response = client.get(&url).send().await.ok()?;

    if !response.status().is_success() {
        return None;
    }

    let json: serde_json::Value = response.json().await.ok()?;

    let status = json.get("status")?.as_str()?;
    let ahead_by = json.get("ahead_by")?.as_u64()?;

    if status == "ahead" && ahead_by > 0 {
        // Get the latest commit hash from the compare response
        let latest_hash = json
            .pointer("/commits")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.last())
            .and_then(|c| c.get("sha"))
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string();

        Some(GitStatus {
            latest_hash,
            commits_behind: ahead_by,
        })
    } else {
        Some(GitStatus {
            latest_hash: installed_hash.to_string(),
            commits_behind: 0,
        })
    }
}

/// Status of a path-installed crate's local git repo
#[derive(Debug, Clone)]
pub struct PathStatus {
    /// Current HEAD commit hash
    pub head_hash: String,
    /// How many commits behind the remote tracking branch
    pub commits_behind: u64,
    /// Whether there are uncommitted changes
    pub has_dirty_changes: bool,
    /// Remote URL (if any) for display/reinstall
    pub remote_url: Option<String>,
}

/// Check path-installed crates for git updates by running git commands in their directories
pub fn check_path_updates(packages: &[GlobalPackage]) -> HashMap<String, PathStatus> {
    let mut results = HashMap::new();

    for pkg in packages {
        if pkg.source != GlobalSource::Path {
            continue;
        }
        let Some(ref path) = pkg.local_path else {
            continue;
        };

        if let Some(status) = check_local_git_repo(path) {
            results.insert(pkg.name.clone(), status);
        }
    }

    results
}

/// Check a local git repo for how far behind it is from its remote
fn check_local_git_repo(path: &std::path::Path) -> Option<PathStatus> {
    use std::process::Command;

    // Get current HEAD hash
    let head_output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    if !head_output.status.success() {
        return None;
    }
    let head_hash = String::from_utf8_lossy(&head_output.stdout).trim().to_string();

    // Check for dirty working tree
    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .ok()?;
    let has_dirty_changes = status_output.status.success()
        && !String::from_utf8_lossy(&status_output.stdout).trim().is_empty();

    // Fetch from remote (quick, silent)
    let _ = Command::new("git")
        .args(["fetch", "--quiet"])
        .current_dir(path)
        .output();

    // Get remote URL
    let remote_output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .output()
        .ok();
    let remote_url = remote_output
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    // Count commits behind: git rev-list HEAD..@{upstream}
    let behind_output = Command::new("git")
        .args(["rev-list", "--count", "HEAD..@{upstream}"])
        .current_dir(path)
        .output()
        .ok();

    let commits_behind = behind_output
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0);

    Some(PathStatus {
        head_hash,
        commits_behind,
        has_dirty_changes,
        remote_url,
    })
}

/// Generate upgrade commands for outdated global crates
pub fn generate_upgrade_commands(checks: &[GlobalCheck]) -> Vec<String> {
    let mut commands = Vec::new();

    for check in checks {
        if !check.has_update {
            continue;
        }

        match check.package.source {
            GlobalSource::Registry => {
                commands.push(format!("cargo install {}", check.package.name));
            }
            GlobalSource::Git => {
                if let Some(ref url) = check.package.git_url {
                    commands.push(format!("cargo install --git {url}"));
                }
            }
            GlobalSource::Path => {
                if let Some(ref path) = check.package.local_path {
                    commands.push(format!(
                        "cd {} && git pull && cargo install --path .",
                        path.display()
                    ));
                }
            }
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_crate_key_registry() {
        let discovery = GlobalPackageDiscovery::new();
        let key = "bat 0.26.1 (registry+https://github.com/rust-lang/crates.io-index)";
        let pkg = discovery
            .parse_crate_key(key, vec!["bat".to_string()])
            .unwrap();
        assert_eq!(pkg.name, "bat");
        assert_eq!(pkg.installed_version.to_string(), "0.26.1");
        assert_eq!(pkg.source, GlobalSource::Registry);
        assert!(pkg.git_url.is_none());
        assert!(pkg.git_hash.is_none());
    }

    #[test]
    fn test_parse_crate_key_git() {
        let discovery = GlobalPackageDiscovery::new();
        let key = "rtk 0.35.0 (git+https://github.com/rtk-ai/rtk#8a7106c8f2996ebc75b38a71c5f342f17811ce39)";
        let pkg = discovery
            .parse_crate_key(key, vec!["rtk".to_string()])
            .unwrap();
        assert_eq!(pkg.name, "rtk");
        assert_eq!(pkg.installed_version.to_string(), "0.35.0");
        assert_eq!(pkg.source, GlobalSource::Git);
        assert_eq!(
            pkg.git_url.as_deref(),
            Some("https://github.com/rtk-ai/rtk")
        );
        assert_eq!(
            pkg.git_hash.as_deref(),
            Some("8a7106c8f2996ebc75b38a71c5f342f17811ce39")
        );
    }

    #[test]
    fn test_parse_crate_key_path_no_git() {
        let discovery = GlobalPackageDiscovery::new();
        // Path that doesn't exist or has no .git → skipped
        let key = "fakecrate 0.1.0 (path+file:///nonexistent/path/fakecrate)";
        let pkg = discovery.parse_crate_key(key, vec!["fakecrate".to_string()]);
        assert!(pkg.is_none());
    }

    #[test]
    fn test_parse_crate_key_path_with_git() {
        let discovery = GlobalPackageDiscovery::new();
        // Use this repo itself as a known git repo
        let this_repo = env!("CARGO_MANIFEST_DIR");
        let parent = std::path::Path::new(this_repo).parent().unwrap();
        let key = format!(
            "testrepo 0.1.0 (path+file://{})",
            parent.display()
        );
        let pkg = discovery.parse_crate_key(&key, vec!["testrepo".to_string()]);
        assert!(pkg.is_some());
        let pkg = pkg.unwrap();
        assert_eq!(pkg.source, GlobalSource::Path);
        assert_eq!(pkg.local_path.unwrap(), parent);
    }

    #[test]
    fn test_parse_crates_toml() {
        let discovery = GlobalPackageDiscovery::new();
        let toml = r#"[v1]
"bat 0.26.1 (registry+https://github.com/rust-lang/crates.io-index)" = ["bat"]
"rtk 0.35.0 (git+https://github.com/rtk-ai/rtk#8a7106c8f2996ebc75b38a71c5f342f17811ce39)" = ["rtk"]
"fakecrate 0.1.0 (path+file:///nonexistent/fakecrate)" = ["fakecrate"]
"#;
        let packages = discovery.parse_crates_toml(toml).unwrap();
        // Should have 2 packages (nonexistent path is skipped)
        assert_eq!(packages.len(), 2);

        let registry = packages.iter().find(|p| p.name == "bat").unwrap();
        assert_eq!(registry.source, GlobalSource::Registry);

        let git = packages.iter().find(|p| p.name == "rtk").unwrap();
        assert_eq!(git.source, GlobalSource::Git);
    }

    #[test]
    fn test_parse_github_url() {
        let (owner, repo) = parse_github_url("https://github.com/rtk-ai/rtk").unwrap();
        assert_eq!(owner, "rtk-ai");
        assert_eq!(repo, "rtk");

        let (owner, repo) = parse_github_url("https://github.com/wild-linker/wild.git").unwrap();
        assert_eq!(owner, "wild-linker");
        assert_eq!(repo, "wild");
    }

    #[test]
    fn test_generate_upgrade_commands() {
        let checks = vec![
            GlobalCheck {
                package: GlobalPackage {
                    name: "bat".to_string(),
                    installed_version: Version::from_str("0.26.1").unwrap(),
                    source: GlobalSource::Registry,
                    binaries: vec!["bat".to_string()],
                    git_url: None,
                    git_hash: None,
                    local_path: None,
                },
                latest_version: Some(Version::from_str("0.27.0").unwrap()),
                latest_hash: None,
                commits_behind: None,
                has_dirty_changes: false,
                has_update: true,
            },
            GlobalCheck {
                package: GlobalPackage {
                    name: "rtk".to_string(),
                    installed_version: Version::from_str("0.35.0").unwrap(),
                    source: GlobalSource::Git,
                    binaries: vec!["rtk".to_string()],
                    git_url: Some("https://github.com/rtk-ai/rtk".to_string()),
                    git_hash: Some("abc123".to_string()),
                    local_path: None,
                },
                latest_version: None,
                latest_hash: Some("def456".to_string()),
                commits_behind: Some(5),
                has_dirty_changes: false,
                has_update: true,
            },
        ];

        let commands = generate_upgrade_commands(&checks);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], "cargo install bat");
        assert_eq!(commands[1], "cargo install --git https://github.com/rtk-ai/rtk");
    }

    #[test]
    fn test_update_severity() {
        let pkg = GlobalPackage {
            name: "test".to_string(),
            installed_version: Version::from_str("1.0.0").unwrap(),
            source: GlobalSource::Registry,
            binaries: vec![],
            git_url: None,
            git_hash: None,
            local_path: None,
        };

        let check = GlobalCheck {
            package: pkg.clone(),
            latest_version: Some(Version::from_str("2.0.0").unwrap()),
            latest_hash: None,
            commits_behind: None,
            has_dirty_changes: false,
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Major));

        let check = GlobalCheck {
            package: pkg.clone(),
            latest_version: Some(Version::from_str("1.1.0").unwrap()),
            latest_hash: None,
            commits_behind: None,
            has_dirty_changes: false,
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Minor));

        let check = GlobalCheck {
            package: pkg,
            latest_version: Some(Version::from_str("1.0.1").unwrap()),
            latest_hash: None,
            commits_behind: None,
            has_dirty_changes: false,
            has_update: true,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Patch));
    }
}
