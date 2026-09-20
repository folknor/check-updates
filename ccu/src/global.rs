use anyhow::Result;
use check_updates_core::{DependencyResolver, UpdateSeverity, Version};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

/// Source of a globally installed cargo crate
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
    /// Whether the check could not be completed, so `has_update: false` means
    /// "unknown" rather than "up to date".
    ///
    /// Set for git installs whose remote is not GitHub (we only speak the
    /// GitHub compare API), for GitHub requests that fail or are rate limited,
    /// and for path installs whose local git commands failed.
    pub check_failed: bool,
}

impl GlobalCheck {
    /// Get update severity for coloring (registry crates only).
    ///
    /// Delegates to [`DependencyResolver::calculate_severity`] so global mode
    /// classifies a move exactly the way project mode does. The comparison must
    /// not be re-derived from the major/minor/patch fields here: a move that is
    /// newer without changing the triple (leaving a pre-release, gaining a
    /// post-release, a fourth release segment) would then return `None` while
    /// `has_update` is `true`, and the row would claim an update with no
    /// severity.
    pub fn update_severity(&self) -> Option<UpdateSeverity> {
        if !self.has_update || self.check_failed {
            return None;
        }
        DependencyResolver::calculate_severity(
            Some(&self.package.installed_version),
            self.latest_version.as_ref(),
        )
    }
}

/// Discovers globally installed cargo crates from ~/.cargo/.crates.toml
#[derive(Default)]
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
        } else if let Some(git_part) = source_str.strip_prefix("git+") {
            // Parse: "git+https://github.com/user/repo#commithash"
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
        } else if let Some(path_str) = source_str
            .strip_prefix("path+file://")
            .or_else(|| source_str.strip_prefix("path+"))
        {
            // Parse: "path+file:///home/user/project" or "path+/home/user/project"
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

/// How many GitHub API requests we allow in flight at once.
const GIT_CHECK_CONCURRENCY: usize = 5;

/// Check git repositories for newer commits
///
/// Every git-sourced package gets an entry in the returned map. Packages whose
/// remote we cannot interrogate (non-GitHub host, network failure, rate limit)
/// get a `GitStatus` with `unknown: true` rather than being silently omitted:
/// omission used to render as "up to date".
///
/// Requests run concurrently under a semaphore instead of in a sequential
/// `for` loop of awaits. `progress` is invoked once per completed
/// package with a monotonically increasing count.
pub async fn check_git_updates<F>(
    packages: &[GlobalPackage],
    progress: F,
) -> HashMap<String, GitStatus>
where
    F: Fn(usize) + Send + Sync + 'static,
{
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let client = reqwest::Client::builder()
        .user_agent("cargo-check-updates/0.1.0")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let semaphore = Arc::new(tokio::sync::Semaphore::new(GIT_CHECK_CONCURRENCY));
    let done = Arc::new(AtomicUsize::new(0));
    let progress = Arc::new(progress);

    let mut tasks = tokio::task::JoinSet::new();

    for pkg in packages {
        if pkg.source != GlobalSource::Git {
            continue;
        }
        let Some(url) = pkg.git_url.clone() else {
            continue;
        };
        let Some(installed_hash) = pkg.git_hash.clone() else {
            continue;
        };

        let name = pkg.name.clone();
        let client = client.clone();
        let semaphore = Arc::clone(&semaphore);
        let done = Arc::clone(&done);
        let progress = Arc::clone(&progress);

        tasks.spawn(async move {
            let _permit = semaphore.acquire().await;

            let status = match parse_github_url(&url) {
                Some((owner, repo)) => check_github_repo(&client, &owner, &repo, &installed_hash)
                    .await
                    .unwrap_or_else(|| GitStatus::unknown(&installed_hash)),
                // Not a GitHub remote: gitlab, codeberg, sourcehut, self-hosted
                // gitea. We have no API for these, so say so instead of
                // claiming the install is current.
                None => GitStatus::unknown(&installed_hash),
            };

            // Arc<F> is not itself callable; deref to F.
            (*progress)(done.fetch_add(1, Ordering::Relaxed) + 1);
            (name, status)
        });
    }

    let mut results = HashMap::new();
    while let Some(joined) = tasks.join_next().await {
        if let Ok((name, status)) = joined {
            results.insert(name, status);
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
    /// True when the remote could not be consulted at all. `commits_behind: 0`
    /// then means "we do not know", not "current".
    pub unknown: bool,
}

impl GitStatus {
    /// An indeterminate result: we could not reach or understand the remote.
    fn unknown(installed_hash: &str) -> Self {
        Self {
            latest_hash: installed_hash.to_string(),
            commits_behind: 0,
            unknown: true,
        }
    }
}

/// Extract owner/repo from a GitHub URL
///
/// Only github.com is accepted. The previous implementation took the last two
/// `/`-separated segments of any URL, so gitlab/codeberg/gitea/sourcehut
/// remotes were happily sent to api.github.com.
fn parse_github_url(url: &str) -> Option<(String, String)> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);

    // scp-like form: git@github.com:owner/repo
    let rest = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        // URL form, with or without scheme and with or without a leading www.
        let without_scheme = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .or_else(|| url.strip_prefix("ssh://git@"))
            .or_else(|| url.strip_prefix("git://"))
            .unwrap_or(url);
        let without_www = without_scheme
            .strip_prefix("www.")
            .unwrap_or(without_scheme);
        without_www.strip_prefix("github.com/")?
    };

    let mut parts = rest.split('/').filter(|s| !s.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    // Anything deeper than owner/repo is not a repository root.
    if parts.next().is_some() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Query GitHub API to check if a commit is behind HEAD
///
/// `None` means indeterminate: transport error, non-success status (404 for a
/// private or renamed repo, 403/429 for the unauthenticated rate limit), or a
/// response we could not parse. Callers must not read that as "up to date".
async fn check_github_repo(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    installed_hash: &str,
) -> Option<GitStatus> {
    // Use the compare API: compare installed_hash...HEAD
    let url =
        format!("https://api.github.com/repos/{owner}/{repo}/compare/{installed_hash}...HEAD");

    let response = client.get(&url).send().await.ok()?;

    if !response.status().is_success() {
        return None;
    }

    let json: serde_json::Value = response.json().await.ok()?;

    let status = json.get("status")?.as_str()?;
    // The compare is `base...head` = `installed_hash...HEAD`, so GitHub's
    // `ahead_by` counts the commits HEAD has that the *installed* hash lacks.
    // Named from the base's point of view that is exactly how far behind the
    // installed version is, which is why it lands in `commits_behind`. The two
    // names invert each other only because they are measured from opposite
    // ends; `behind_by` here would be commits the installed hash has that
    // upstream does not, which is not what we report.
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
            unknown: false,
        })
    } else {
        Some(GitStatus {
            latest_hash: installed_hash.to_string(),
            commits_behind: 0,
            unknown: false,
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
    /// True when the local git commands failed, so `commits_behind: 0` means
    /// "we do not know", not "current".
    pub unknown: bool,
}

impl PathStatus {
    /// An indeterminate result: the repo could not be interrogated.
    fn unknown() -> Self {
        Self {
            head_hash: String::new(),
            commits_behind: 0,
            has_dirty_changes: false,
            remote_url: None,
            unknown: true,
        }
    }
}

/// Check path-installed crates for git updates by running git commands in their directories
///
/// Each repo is inspected on a blocking-pool thread (`spawn_blocking`) and all
/// of them run concurrently, so `git fetch` no longer blocks the async runtime
/// thread serially for every path install. Every path package gets an
/// entry; failures are reported as `unknown` rather than omitted.
pub async fn check_path_updates(packages: &[GlobalPackage]) -> HashMap<String, PathStatus> {
    let mut tasks = tokio::task::JoinSet::new();

    for pkg in packages {
        if pkg.source != GlobalSource::Path {
            continue;
        }
        let Some(path) = pkg.local_path.clone() else {
            continue;
        };
        let name = pkg.name.clone();

        tasks.spawn_blocking(move || {
            let status = check_local_git_repo(&path).unwrap_or_else(PathStatus::unknown);
            (name, status)
        });
    }

    let mut results = HashMap::new();
    while let Some(joined) = tasks.join_next().await {
        if let Ok((name, status)) = joined {
            results.insert(name, status);
        }
    }

    results
}

/// Environment that keeps `git fetch` from hanging on an unreachable remote
///
/// tokio is built here without the `time` and `process` features, so there is
/// no `tokio::time::timeout` and no async child to kill. Git's own transport
/// knobs are used instead: never prompt for credentials, and abort an HTTP
/// transfer that moves less than 1 KiB/s for 15 seconds. SSH gets an explicit
/// connect timeout through `GIT_SSH_COMMAND`.
fn git_fetch_command() -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "-c",
        "http.lowSpeedLimit=1000",
        "-c",
        "http.lowSpeedTime=15",
        "fetch",
        "--quiet",
    ]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_ASKPASS", "");
    cmd.env(
        "GIT_SSH_COMMAND",
        "ssh -o BatchMode=yes -o ConnectTimeout=10",
    );
    cmd
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
    let head_hash = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();

    // Check for dirty working tree
    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .ok()?;
    let has_dirty_changes = status_output.status.success()
        && !String::from_utf8_lossy(&status_output.stdout)
            .trim()
            .is_empty();

    // Fetch from remote (quick, silent, non-interactive, bounded)
    let _ = git_fetch_command().current_dir(path).output();

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

    // A failure here is not "zero commits behind". `rev-list HEAD..@{upstream}`
    // exits non-zero when the branch has no upstream configured, and the count
    // is stale-or-wrong when the preceding fetch could not reach the remote. In
    // either case we did not determine the distance, so the row is `unknown`
    // and the dirty/HEAD facts we *did* establish are kept.
    let commits_behind = behind_output.filter(|o| o.status.success()).and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<u64>()
            .ok()
    });

    let unknown = commits_behind.is_none();

    Some(PathStatus {
        head_hash,
        commits_behind: commits_behind.unwrap_or(0),
        has_dirty_changes,
        remote_url,
        unknown,
    })
}

/// Generate upgrade commands for outdated global crates
///
/// Path installs are special-cased for a dirty working tree:
/// `git pull` refuses to run with uncommitted changes, and a repo that is only
/// dirty (0 commits behind) has `has_update: false` yet still renders as
/// "dirty" in the table, so it used to get no command at all.
pub fn generate_upgrade_commands(checks: &[GlobalCheck]) -> Vec<String> {
    let mut commands = Vec::new();

    for check in checks {
        // A failed check is not an update; we do not know either way.
        if check.check_failed {
            continue;
        }

        let dirty_path_rebuild =
            check.package.source == GlobalSource::Path && check.has_dirty_changes;
        if !check.has_update && !dirty_path_rebuild {
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
                    let path = path.display();
                    let behind = check.commits_behind.unwrap_or(0);
                    let command = match (check.has_dirty_changes, behind) {
                        // Clean and behind: the original command works.
                        (false, _) => {
                            format!("cd {path} && git pull && cargo install --path .")
                        }
                        // Dirty but current: nothing to pull, but the working
                        // tree holds changes that are not in the installed
                        // binary. Rebuilding from the dirty tree is exactly
                        // what is wanted here.
                        (true, 0) => {
                            format!("cd {path} && cargo install --path .  # uncommitted changes")
                        }
                        // Dirty and behind: `git pull` would refuse. Do not
                        // emit a command that cannot run, and do not stash on
                        // the user's behalf - just say what has to happen.
                        (true, n) => format!(
                            "cd {path}  # {n} commit(s) behind with uncommitted changes: commit or stash, then: git pull && cargo install --path ."
                        ),
                    };
                    commands.push(command);
                }
            }
        }
    }

    commands
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        let key = format!("testrepo 0.1.0 (path+file://{})", parent.display());
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

        let (owner, repo) = parse_github_url("git@github.com:rtk-ai/rtk.git").unwrap();
        assert_eq!(owner, "rtk-ai");
        assert_eq!(repo, "rtk");

        let (owner, repo) = parse_github_url("ssh://git@github.com/rtk-ai/rtk").unwrap();
        assert_eq!(owner, "rtk-ai");
        assert_eq!(repo, "rtk");
    }

    #[test]
    fn test_parse_github_url_rejects_other_forges() {
        assert!(parse_github_url("https://gitlab.com/owner/repo").is_none());
        assert!(parse_github_url("https://codeberg.org/owner/repo").is_none());
        assert!(parse_github_url("https://git.sr.ht/~user/repo").is_none());
        assert!(parse_github_url("https://git.example.com/owner/repo").is_none());
        assert!(parse_github_url("git@gitlab.com:owner/repo.git").is_none());
        // github.io pages and deeper paths are not repository roots
        assert!(parse_github_url("https://owner.github.io/repo").is_none());
        assert!(parse_github_url("https://github.com/owner/repo/tree/main").is_none());
    }

    fn path_check(path: &str, behind: Option<u64>, dirty: bool, has_update: bool) -> GlobalCheck {
        GlobalCheck {
            package: GlobalPackage {
                name: "local".to_string(),
                installed_version: Version::from_str("0.1.0").unwrap(),
                source: GlobalSource::Path,
                binaries: vec!["local".to_string()],
                git_url: None,
                git_hash: None,
                local_path: Some(PathBuf::from(path)),
            },
            latest_version: None,
            latest_hash: None,
            commits_behind: behind,
            has_dirty_changes: dirty,
            has_update,
            check_failed: false,
        }
    }

    #[test]
    fn test_upgrade_command_clean_path_repo_pulls() {
        let commands = generate_upgrade_commands(&[path_check("/p", Some(3), false, true)]);
        assert_eq!(
            commands,
            vec!["cd /p && git pull && cargo install --path ."]
        );
    }

    #[test]
    fn test_upgrade_command_dirty_but_current_path_repo_rebuilds() {
        // has_update is false (0 commits behind) but the table still shows it
        // as dirty, so it must still get a command.
        let commands = generate_upgrade_commands(&[path_check("/p", Some(0), true, false)]);
        assert_eq!(commands.len(), 1);
        assert!(commands[0].starts_with("cd /p && cargo install --path ."));
        assert!(!commands[0].contains("git pull"));
    }

    #[test]
    fn test_upgrade_command_dirty_and_behind_path_repo_does_not_pull() {
        let commands = generate_upgrade_commands(&[path_check("/p", Some(3), true, true)]);
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("commit or stash"));
        assert!(!commands[0].starts_with("cd /p && git pull"));
    }

    #[test]
    fn test_upgrade_commands_skip_failed_checks() {
        let mut check = path_check("/p", None, false, false);
        check.check_failed = true;
        assert!(generate_upgrade_commands(&[check]).is_empty());
    }

    #[test]
    fn test_failed_check_has_no_severity() {
        let check = GlobalCheck {
            package: GlobalPackage {
                name: "x".to_string(),
                installed_version: Version::from_str("1.0.0").unwrap(),
                source: GlobalSource::Git,
                binaries: vec![],
                git_url: Some("https://gitlab.com/o/r".to_string()),
                git_hash: Some("abc".to_string()),
                local_path: None,
            },
            latest_version: Some(Version::from_str("2.0.0").unwrap()),
            latest_hash: None,
            commits_behind: None,
            has_dirty_changes: false,
            has_update: true,
            check_failed: true,
        };
        assert_eq!(check.update_severity(), None);
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
                check_failed: false,
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
                check_failed: false,
            },
        ];

        let commands = generate_upgrade_commands(&checks);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], "cargo install bat");
        assert_eq!(
            commands[1],
            "cargo install --git https://github.com/rtk-ai/rtk"
        );
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
            check_failed: false,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Major));

        let check = GlobalCheck {
            package: pkg.clone(),
            latest_version: Some(Version::from_str("1.1.0").unwrap()),
            latest_hash: None,
            commits_behind: None,
            has_dirty_changes: false,
            has_update: true,
            check_failed: false,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Minor));

        let check = GlobalCheck {
            package: pkg,
            latest_version: Some(Version::from_str("1.0.1").unwrap()),
            latest_hash: None,
            commits_behind: None,
            has_dirty_changes: false,
            has_update: true,
            check_failed: false,
        };
        assert_eq!(check.update_severity(), Some(UpdateSeverity::Patch));
    }
}
