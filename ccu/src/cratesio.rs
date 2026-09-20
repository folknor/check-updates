use anyhow::Result;
use check_updates_core::{PackageInfo, Version};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

/// Why a registry fetch failed.
///
/// Every failure used to collapse into one opaque string that
/// `main.rs` printed under "Crates not found on crates.io:". A rate-limited
/// or offline run therefore told the user their crates do not exist. Callers
/// classify with [`FetchErrorKind`] instead of reading the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchErrorKind {
    /// The registry answered 404: the crate genuinely is not there.
    NotFound,
    /// 429, or 403 with a rate-limit body: back off and retry later.
    RateLimited,
    /// 5xx: the registry is broken, the crate may well exist.
    ServerError,
    /// Any other non-success status.
    HttpStatus,
    /// The request timed out.
    Timeout,
    /// Connection / DNS / TLS failure: no answer at all.
    Network,
    /// A response arrived but was not the JSON we expect.
    Parse,
    /// The registry answered, but not one version string was usable.
    NoUsableVersions,
    /// Versions exist but all are prereleases and `--pre-release` is off.
    NoStableVersions,
    /// The worker task panicked or was cancelled.
    TaskFailed,
}

impl FetchErrorKind {
    /// True only for the one kind that means "this crate does not exist".
    /// Everything else is a failure to check, not a verdict on the crate.
    pub fn is_missing(self) -> bool {
        matches!(self, Self::NotFound)
    }

    /// Failures worth retrying: the crate's status is simply unknown.
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::ServerError
                | Self::Timeout
                | Self::Network
                | Self::TaskFailed
        )
    }

    /// Stable machine-readable tag for the `--json` envelope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::ServerError => "server_error",
            Self::HttpStatus => "http_status",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Parse => "parse",
            Self::NoUsableVersions => "no_usable_versions",
            Self::NoStableVersions => "no_stable_versions",
            Self::TaskFailed => "task_failed",
        }
    }
}

/// A classified fetch failure that always carries the package it belongs to.
#[derive(Debug, Clone)]
pub struct FetchError {
    pub package: String,
    pub kind: FetchErrorKind,
    pub detail: String,
}

impl FetchError {
    pub fn new(
        package: impl Into<String>,
        kind: FetchErrorKind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            package: package.into(),
            kind,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for FetchError {}

/// Map a transport-level reqwest error onto a kind.
fn classify_transport(err: &reqwest::Error) -> FetchErrorKind {
    if err.is_timeout() {
        FetchErrorKind::Timeout
    } else if err.is_decode() {
        FetchErrorKind::Parse
    } else {
        FetchErrorKind::Network
    }
}

fn classify_status(status: reqwest::StatusCode) -> FetchErrorKind {
    if status == reqwest::StatusCode::NOT_FOUND {
        FetchErrorKind::NotFound
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        FetchErrorKind::RateLimited
    } else if status.is_server_error() {
        FetchErrorKind::ServerError
    } else {
        FetchErrorKind::HttpStatus
    }
}

#[derive(Debug, Deserialize)]
struct CrateVersion {
    num: String,
    yanked: bool,
    /// ISO-8601 publish timestamp from crates.io
    created_at: Option<String>,
}

/// Client for querying crates.io API
pub struct CratesIoClient {
    client: reqwest::Client,
    base_url: String,
    include_prerelease: bool,
}

/// crates.io API response for a single crate
#[derive(Debug, Deserialize)]
struct CrateResponse {
    #[serde(rename = "crate")]
    crate_info: CrateInfo,
    versions: Vec<CrateVersion>,
}

#[derive(Debug, Deserialize)]
struct CrateInfo {
    name: String,
}

impl CratesIoClient {
    pub fn new(include_prerelease: bool) -> Self {
        Self {
            client: reqwest::Client::builder()
                // crates.io requires a user-agent with contact info
                .user_agent(
                    "cargo-check-updates/0.1.0 (https://github.com/folknor/cargo-check-updates)",
                )
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: "https://crates.io/api/v1/crates".to_string(),
            include_prerelease,
        }
    }

    /// Fetch package info from crates.io.
    ///
    /// Over-fetching (one full request per crate, no caching) is knowingly
    /// not addressed here.
    /// `/api/v1/crates/{name}` returns the whole version history and we only
    /// need `num`, `yanked` and `created_at`. The lighter alternatives each
    /// cost more than they save right now: the sparse index
    /// (`index.crates.io/{a}/{b}/{name}`) has a different shape, no publish
    /// dates at all, and its own path rules for short names; `/versions` is
    /// still the full list. Conditional requests need a cache store that does
    /// not exist in this workspace - that is a new module plus a cache
    /// eviction policy, not a change to this file. Revisit together, not
    /// piecemeal.
    pub async fn get_package(&self, name: &str) -> std::result::Result<PackageInfo, FetchError> {
        let url = format!("{}/{}", self.base_url, name);

        let response = self.client.get(&url).send().await.map_err(|e| {
            FetchError::new(
                name,
                classify_transport(&e),
                format!("Failed to fetch crate '{name}': {e}"),
            )
        })?;

        let status = response.status();
        if !status.is_success() {
            let kind = classify_status(status);
            let detail = match kind {
                FetchErrorKind::NotFound => format!("Crate '{name}' not found on crates.io"),
                FetchErrorKind::RateLimited => {
                    format!("crates.io rate-limited the request for '{name}' ({status})")
                }
                _ => format!("crates.io request for '{name}' failed with status {status}"),
            };
            return Err(FetchError::new(name, kind, detail));
        }

        let crate_data: CrateResponse = response.json().await.map_err(|e| {
            FetchError::new(
                name,
                FetchErrorKind::Parse,
                format!("Failed to parse JSON response for '{name}': {e}"),
            )
        })?;

        self.build_package_info(name, crate_data)
    }

    /// Pure half of [`Self::get_package`], so it can be exercised against
    /// fixtures without touching the network.
    fn build_package_info(
        &self,
        name: &str,
        crate_data: CrateResponse,
    ) -> std::result::Result<PackageInfo, FetchError> {
        // Parse all versions, skipping yanked ones. Track publish dates by
        // the version's `original` string so the resolver can attach them
        // to DependencyCheck.
        let mut all_versions: Vec<Version> = Vec::new();
        let mut published_at: HashMap<String, String> = HashMap::new();
        let mut unparsed: Vec<String> = Vec::new();
        for version in &crate_data.versions {
            if version.yanked {
                continue;
            }

            match Version::from_str(&version.num) {
                Ok(v) => {
                    if let Some(date) = &version.created_at {
                        published_at.insert(v.original.clone(), date.clone());
                    }
                    all_versions.push(v);
                }
                // core::Version::from_str is strict since wave 1, so a
                // registry string we cannot read is dropped here. Count it
                // rather than swallowing it silently.
                Err(_) => unparsed.push(version.num.clone()),
            }
        }
        warn_unparsed(name, &unparsed);

        if all_versions.is_empty() {
            return Err(FetchError::new(
                name,
                FetchErrorKind::NoUsableVersions,
                format!(
                    "No usable versions for crate '{name}' ({} unreadable, {} yanked or absent)",
                    unparsed.len(),
                    crate_data.versions.len() - unparsed.len()
                ),
            ));
        }

        // Sort versions in ascending order
        all_versions.sort();

        // Filter versions based on prerelease setting
        let filtered_versions: Vec<Version> = if self.include_prerelease {
            all_versions.clone()
        } else {
            all_versions
                .iter()
                .filter(|v| !v.is_prerelease())
                .cloned()
                .collect()
        };

        if filtered_versions.is_empty() {
            return Err(FetchError::new(
                name,
                FetchErrorKind::NoStableVersions,
                format!(
                    "No stable versions found for crate '{name}' (use --pre-release to include pre-releases)"
                ),
            ));
        }

        // `filtered_versions` is `all_versions` when prereleases are included,
        // so its last element is the right target in both modes.
        let latest = filtered_versions
            .last()
            .ok_or_else(|| {
                FetchError::new(
                    name,
                    FetchErrorKind::NoUsableVersions,
                    format!("No versions found for crate '{name}'"),
                )
            })?
            .clone();

        // Get latest stable version (always filter out prereleases)
        let latest_stable = all_versions.iter().rfind(|v| !v.is_prerelease()).cloned();

        Ok(PackageInfo {
            name: crate_data.crate_info.name,
            versions: filtered_versions,
            latest,
            latest_stable,
            published_at,
        })
    }

    /// Fetch multiple packages concurrently
    pub async fn get_packages(
        &self,
        names: &[String],
        progress_callback: impl Fn(usize, usize) + Send + Sync + 'static,
    ) -> Result<GetPackagesResult> {
        let total = names.len();
        let progress_callback = Arc::new(progress_callback);

        // Limit concurrent requests to avoid overwhelming the server
        // crates.io has rate limits, so be conservative
        let semaphore = Arc::new(Semaphore::new(5));
        // Completions arrive out of order under the semaphore, so
        // the spawn index is not a completion count. Count completions.
        let completed = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();

        for name in names {
            let client = self.clone();
            let name = name.clone();
            let callback = Arc::clone(&progress_callback);
            let semaphore = Arc::clone(&semaphore);
            let completed = Arc::clone(&completed);

            let task = tokio::spawn(async move {
                // Acquire semaphore permit
                let _permit = semaphore.acquire().await.expect("semaphore closed");

                let result = client.get_package(&name).await;

                // Call progress callback
                callback(completed.fetch_add(1, Ordering::SeqCst) + 1, total);

                (name, result)
            });

            tasks.push(task);
        }

        // Wait for all tasks to complete. Zipping with `names` keeps the
        // package name available even when the task itself panicked
        // (this used to be recorded as the literal "unknown").
        let mut packages = HashMap::new();
        let mut failures: Vec<FetchError> = Vec::new();

        for (name, task) in names.iter().zip(tasks) {
            match task.await {
                Ok((name, Ok(package_info))) => {
                    packages.insert(name, package_info);
                }
                Ok((_, Err(e))) => failures.push(e),
                Err(e) => failures.push(FetchError::new(
                    name.clone(),
                    FetchErrorKind::TaskFailed,
                    format!("Task for '{name}' failed: {e}"),
                )),
            }
        }

        // Failures are reported, never fatal - including when every crate
        // failed. A partial failure already returned its results with the
        // failures alongside them, and making the total case an error meant the
        // same event was handled under two opposite policies: one dependency
        // unreachable produced a report, all of them produced no report at all.
        // The caller turns each failure into a `check_failed` row, which says
        // more than an aborted run does.
        Ok(GetPackagesResult { packages, failures })
    }
}

/// Emit a one-line summary of registry version strings we could not read.
fn warn_unparsed(name: &str, unparsed: &[String]) {
    if unparsed.is_empty() {
        return;
    }
    let shown: Vec<&str> = unparsed.iter().take(3).map(String::as_str).collect();
    let more = unparsed.len().saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(" (and {more} more)")
    } else {
        String::new()
    };
    eprintln!(
        "warning: {}: {} version{} from crates.io could not be parsed and {} skipped: {}{}",
        name,
        unparsed.len(),
        if unparsed.len() == 1 { "" } else { "s" },
        if unparsed.len() == 1 { "was" } else { "were" },
        shown.join(", "),
        suffix
    );
}

/// Result of fetching multiple packages
#[derive(Debug, Clone)]
pub struct GetPackagesResult {
    pub packages: HashMap<String, PackageInfo>,
    /// Classified failures, one per package that could not be checked.
    pub failures: Vec<FetchError>,
}

// Implement Clone for CratesIoClient to support concurrent usage
impl Clone for CratesIoClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            include_prerelease: self.include_prerelease,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_package_serde() {
        let client = CratesIoClient::new(false);
        let result = client.get_package("serde").await;

        assert!(
            result.is_ok(),
            "Failed to fetch serde crate: {:?}",
            result.err()
        );

        let package_info = result.unwrap();
        assert_eq!(package_info.name.to_lowercase(), "serde");
        assert!(!package_info.versions.is_empty());
        assert!(package_info.latest_stable.is_some());
    }

    #[tokio::test]
    async fn test_get_package_not_found() {
        let client = CratesIoClient::new(false);
        let result = client
            .get_package("this-crate-definitely-does-not-exist-12345")
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind, FetchErrorKind::NotFound);
        assert!(err.kind.is_missing());
        assert_eq!(err.package, "this-crate-definitely-does-not-exist-12345");
    }

    fn parse_fixture(json: &str) -> CrateResponse {
        serde_json::from_str(json).expect("fixture parses")
    }

    const FIXTURE: &str = r#"{
        "crate": { "name": "demo" },
        "versions": [
            { "num": "1.0.0", "yanked": false, "created_at": "2024-01-01T00:00:00Z" },
            { "num": "1.1.0-beta.1", "yanked": false, "created_at": "2024-02-01T00:00:00Z" },
            { "num": "0.9.0", "yanked": true, "created_at": "2023-01-01T00:00:00Z" }
        ]
    }"#;

    #[test]
    fn fixture_latest_excludes_prerelease_by_default() {
        let client = CratesIoClient::new(false);
        let info = client
            .build_package_info("demo", parse_fixture(FIXTURE))
            .expect("builds");
        assert_eq!(info.latest.original, "1.0.0");
        assert_eq!(info.versions.len(), 1);
        assert_eq!(
            info.published_at.get("1.0.0").map(String::as_str),
            Some("2024-01-01T00:00:00Z")
        );
    }

    #[test]
    fn fixture_latest_includes_prerelease_when_asked() {
        let client = CratesIoClient::new(true);
        let info = client
            .build_package_info("demo", parse_fixture(FIXTURE))
            .expect("builds");
        assert_eq!(info.latest.original, "1.1.0-beta.1");
        assert_eq!(
            info.latest_stable.as_ref().map(|v| v.original.as_str()),
            Some("1.0.0")
        );
    }

    #[test]
    fn fixture_unparseable_versions_are_an_error_not_a_zero() {
        let client = CratesIoClient::new(false);
        let json = r#"{
            "crate": { "name": "demo" },
            "versions": [ { "num": "not-a-version", "yanked": false, "created_at": null } ]
        }"#;
        let err = client
            .build_package_info("demo", parse_fixture(json))
            .expect_err("no usable versions");
        assert_eq!(err.kind, FetchErrorKind::NoUsableVersions);
        assert!(!err.kind.is_missing());
    }

    #[test]
    fn fixture_only_prereleases_reports_no_stable() {
        let client = CratesIoClient::new(false);
        let json = r#"{
            "crate": { "name": "demo" },
            "versions": [ { "num": "1.0.0-rc.1", "yanked": false, "created_at": null } ]
        }"#;
        let err = client
            .build_package_info("demo", parse_fixture(json))
            .expect_err("no stable versions");
        assert_eq!(err.kind, FetchErrorKind::NoStableVersions);
    }

    #[test]
    fn status_classification_separates_missing_from_transient() {
        assert_eq!(
            classify_status(reqwest::StatusCode::NOT_FOUND),
            FetchErrorKind::NotFound
        );
        assert_eq!(
            classify_status(reqwest::StatusCode::TOO_MANY_REQUESTS),
            FetchErrorKind::RateLimited
        );
        assert_eq!(
            classify_status(reqwest::StatusCode::BAD_GATEWAY),
            FetchErrorKind::ServerError
        );
        assert_eq!(
            classify_status(reqwest::StatusCode::UNAUTHORIZED),
            FetchErrorKind::HttpStatus
        );
        assert!(FetchErrorKind::RateLimited.is_transient());
        assert!(!FetchErrorKind::NotFound.is_transient());
    }
}
