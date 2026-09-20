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
/// `main.rs` printed under "Packages not found on PyPI:". A rate-limited or
/// offline run therefore told the user their packages do not exist. Callers
/// classify with [`FetchErrorKind`] instead of reading the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchErrorKind {
    /// The index answered 404: the package genuinely is not there.
    NotFound,
    /// 429: back off and retry later.
    RateLimited,
    /// 5xx: the index is broken, the package may well exist.
    ServerError,
    /// Any other non-success status.
    HttpStatus,
    /// The request timed out.
    Timeout,
    /// Connection / DNS / TLS failure: no answer at all.
    Network,
    /// A response arrived but was not the JSON we expect.
    Parse,
    /// The index answered, but not one version string was usable.
    NoUsableVersions,
    /// Versions exist but all are prereleases and `--pre-release` is off.
    NoStableVersions,
    /// The worker task panicked or was cancelled.
    TaskFailed,
}

impl FetchErrorKind {
    /// True only for the one kind that means "this package does not exist".
    pub fn is_missing(self) -> bool {
        matches!(self, Self::NotFound)
    }

    /// Failures worth retrying: the package's status is simply unknown.
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

/// Emit a one-line summary of index version strings we could not read.
fn warn_unparsed(name: &str, unparsed: &[String]) {
    if unparsed.is_empty() {
        return;
    }
    let mut shown: Vec<&str> = unparsed.iter().take(3).map(String::as_str).collect();
    shown.sort_unstable();
    let more = unparsed.len().saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(" (and {more} more)")
    } else {
        String::new()
    };
    eprintln!(
        "warning: {}: {} version{} from PyPI could not be parsed and {} skipped: {}{}",
        name,
        unparsed.len(),
        if unparsed.len() == 1 { "" } else { "s" },
        if unparsed.len() == 1 { "was" } else { "were" },
        shown.join(", "),
        suffix
    );
}

/// Client for querying PyPI API
pub struct PyPiClient {
    client: reqwest::Client,
    base_url: String,
    include_prerelease: bool,
}

/// PyPI JSON API response structure
#[derive(Debug, Deserialize)]
struct PyPiResponse {
    info: PyPiInfo,
    releases: HashMap<String, Vec<PyPiRelease>>,
}

#[derive(Debug, Deserialize)]
struct PyPiInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct PyPiRelease {
    #[allow(dead_code)]
    yanked: Option<bool>,
    /// ISO-8601 upload time. Each release file has its own; the earliest
    /// across files for a version is the de-facto release date.
    upload_time_iso_8601: Option<String>,
}

impl PyPiClient {
    pub fn new(include_prerelease: bool) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("python-check-updates/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: "https://pypi.org/pypi".to_string(),
            include_prerelease,
        }
    }

    pub fn with_index_url(mut self, url: &str) -> Self {
        // Remove trailing slash if present
        self.base_url = url.trim_end_matches('/').to_string();
        self
    }

    /// Fetch package info from PyPI.
    ///
    /// Over-fetching (one full request per package, no caching) is knowingly
    /// not addressed here:
    /// the PyPI JSON API is the only endpoint that carries per-file upload
    /// times, which is what `published_at` needs, and conditional requests
    /// would need a cache store this workspace does not have. See the note on
    /// `ccu/src/cratesio.rs::get_package`.
    pub async fn get_package(&self, name: &str) -> std::result::Result<PackageInfo, FetchError> {
        let url = format!("{}/{}/json", self.base_url, name);

        let response = self.client.get(&url).send().await.map_err(|e| {
            FetchError::new(
                name,
                classify_transport(&e),
                format!("Failed to fetch package '{name}': {e}"),
            )
        })?;

        let status = response.status();
        if !status.is_success() {
            let kind = classify_status(status);
            let detail = match kind {
                FetchErrorKind::NotFound => format!("Package '{name}' not found on PyPI"),
                FetchErrorKind::RateLimited => {
                    format!("PyPI rate-limited the request for '{name}' ({status})")
                }
                _ => format!("PyPI request for '{name}' failed with status {status}"),
            };
            return Err(FetchError::new(name, kind, detail));
        }

        let pypi_data: PyPiResponse = response.json().await.map_err(|e| {
            FetchError::new(
                name,
                FetchErrorKind::Parse,
                format!("Failed to parse JSON response for '{name}': {e}"),
            )
        })?;

        self.build_package_info(name, pypi_data)
    }

    /// Pure half of [`Self::get_package`], exercised against fixtures.
    fn build_package_info(
        &self,
        name: &str,
        pypi_data: PyPiResponse,
    ) -> std::result::Result<PackageInfo, FetchError> {
        // Parse all versions from releases. PyPI tracks an upload time per
        // file (wheel/sdist) within a release; take the earliest as the
        // version's release date.
        let mut all_versions: Vec<Version> = Vec::new();
        let mut published_at: HashMap<String, String> = HashMap::new();
        let mut unparsed: Vec<String> = Vec::new();
        for (version_str, releases) in &pypi_data.releases {
            // Skip yanked releases (empty release list or all yanked)
            if releases.is_empty() {
                continue;
            }

            // Check if all releases are yanked
            let all_yanked = releases.iter().all(|r| r.yanked.unwrap_or(false));
            if all_yanked {
                continue;
            }

            // Try to parse the version. core::Version::from_str is strict
            // since wave 1, so anything we cannot read is dropped - counted
            // here rather than swallowed.
            match Version::from_str(version_str) {
                Ok(version) => {
                    let earliest = releases
                        .iter()
                        .filter_map(|r| r.upload_time_iso_8601.as_deref())
                        .min();
                    if let Some(date) = earliest {
                        published_at.insert(version.original.clone(), date.to_string());
                    }
                    all_versions.push(version);
                }
                Err(_) => unparsed.push(version_str.clone()),
            }
        }
        warn_unparsed(name, &unparsed);

        if all_versions.is_empty() {
            return Err(FetchError::new(
                name,
                FetchErrorKind::NoUsableVersions,
                format!(
                    "No usable versions for package '{name}' ({} unreadable of {} releases)",
                    unparsed.len(),
                    pypi_data.releases.len()
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
                    "No stable versions found for package '{name}' (use --pre-release to include pre-releases)"
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
                    format!("No versions found for package '{name}'"),
                )
            })?
            .clone();

        // Get latest stable version (always filter out prereleases)
        let latest_stable = all_versions.iter().rfind(|v| !v.is_prerelease()).cloned();

        Ok(PackageInfo {
            name: pypi_data.info.name,
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
        let semaphore = Arc::new(Semaphore::new(10));
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

        // Failures are reported, never fatal - including when every package
        // failed. A partial failure already returned its results with the
        // failures alongside them, and making the total case an error meant the
        // same event was handled under two opposite policies: one dependency
        // unreachable produced a report, all of them produced no report at all.
        // The caller turns each failure into a `check_failed` row, which says
        // more than an aborted run does.
        Ok(GetPackagesResult { packages, failures })
    }
}

/// Result of fetching multiple packages
#[derive(Debug, Clone)]
pub struct GetPackagesResult {
    pub packages: HashMap<String, PackageInfo>,
    /// Classified failures, one per package that could not be checked.
    pub failures: Vec<FetchError>,
}

// Implement Clone for PyPiClient to support concurrent usage
impl Clone for PyPiClient {
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
    async fn test_get_package_requests() {
        let client = PyPiClient::new(false);
        let result = client.get_package("requests").await;

        assert!(
            result.is_ok(),
            "Failed to fetch requests package: {:?}",
            result.err()
        );

        let package_info = result.unwrap();
        assert_eq!(package_info.name.to_lowercase(), "requests");
        assert!(!package_info.versions.is_empty());
        assert!(package_info.latest_stable.is_some());
    }

    #[tokio::test]
    async fn test_get_package_not_found() {
        let client = PyPiClient::new(false);
        let result = client
            .get_package("this-package-definitely-does-not-exist-12345")
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind, FetchErrorKind::NotFound);
        assert!(err.kind.is_missing());
        assert_eq!(err.package, "this-package-definitely-does-not-exist-12345");
    }

    fn parse_fixture(json: &str) -> PyPiResponse {
        serde_json::from_str(json).expect("fixture parses")
    }

    const FIXTURE: &str = r#"{
        "info": { "name": "demo" },
        "releases": {
            "1.0.0": [ { "yanked": false, "upload_time_iso_8601": "2024-01-01T00:00:00Z" } ],
            "1.1.0b1": [ { "yanked": false, "upload_time_iso_8601": "2024-02-01T00:00:00Z" } ],
            "0.9.0": [ { "yanked": true, "upload_time_iso_8601": "2023-01-01T00:00:00Z" } ]
        }
    }"#;

    #[test]
    fn fixture_latest_excludes_prerelease_by_default() {
        let client = PyPiClient::new(false);
        let info = client
            .build_package_info("demo", parse_fixture(FIXTURE))
            .expect("builds");
        assert_eq!(info.latest.original, "1.0.0");
        assert!(info.versions.iter().all(|v| !v.is_prerelease()));
    }

    #[test]
    fn fixture_yanked_release_is_dropped() {
        let client = PyPiClient::new(false);
        let info = client
            .build_package_info("demo", parse_fixture(FIXTURE))
            .expect("builds");
        assert!(!info.versions.iter().any(|v| v.original == "0.9.0"));
    }

    #[test]
    fn fixture_unparseable_versions_are_an_error() {
        let client = PyPiClient::new(false);
        let json = r#"{
            "info": { "name": "demo" },
            "releases": { "not-a-version": [ { "yanked": false, "upload_time_iso_8601": null } ] }
        }"#;
        let err = client
            .build_package_info("demo", parse_fixture(json))
            .expect_err("no usable versions");
        assert_eq!(err.kind, FetchErrorKind::NoUsableVersions);
        assert!(!err.kind.is_missing());
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
            classify_status(reqwest::StatusCode::SERVICE_UNAVAILABLE),
            FetchErrorKind::ServerError
        );
        assert!(FetchErrorKind::Timeout.is_transient());
        assert!(!FetchErrorKind::NotFound.is_transient());
    }

    #[tokio::test]
    async fn test_get_packages_concurrent() {
        let client = PyPiClient::new(false);
        let packages = vec!["requests".to_string(), "flask".to_string()];

        // Use Arc<AtomicUsize> for thread-safe counter
        let progress_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress_calls_clone = Arc::clone(&progress_calls);

        let result = client
            .get_packages(&packages, move |_current, _total| {
                progress_calls_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .await;

        assert!(
            result.is_ok(),
            "Failed to fetch packages: {:?}",
            result.err()
        );

        let results = result.unwrap();
        assert!(!results.packages.is_empty());

        // Verify progress callback was called
        let calls = progress_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(calls > 0, "Progress callback should have been called");
    }

    #[tokio::test]
    async fn test_custom_index_url() {
        let client = PyPiClient::new(false).with_index_url("https://pypi.org/pypi/");

        assert_eq!(client.base_url, "https://pypi.org/pypi");
    }

    #[tokio::test]
    async fn test_prerelease_filtering() {
        let client_stable = PyPiClient::new(false);
        let client_pre = PyPiClient::new(true);

        // Find a package that has prereleases (e.g., many popular packages)
        // This test might be flaky depending on package state
        let result_stable = client_stable.get_package("django").await;
        let result_pre = client_pre.get_package("django").await;

        if let (Ok(stable), Ok(pre)) = (result_stable, result_pre) {
            // Pre-release client might have more versions
            assert!(pre.versions.len() >= stable.versions.len());
        }
    }
}
