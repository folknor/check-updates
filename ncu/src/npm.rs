use check_updates_core::{PackageInfo, Version};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// Why a registry fetch failed.
///
/// Every failure used to collapse into one opaque string, so a
/// rate-limited or offline run was indistinguishable from "this package does
/// not exist". Callers classify with [`FetchErrorKind`] rather than matching
/// on message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchErrorKind {
    /// The registry answered 404: the package genuinely is not there.
    NotFound,
    /// 429: back off and retry later.
    RateLimited,
    /// 5xx: the registry is broken, the package may well exist.
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

/// Percent-encode a package name for use as a single registry path segment.
///
/// `@scope/name` was interpolated raw, which happens to work against
/// registry.npmjs.org but is not the documented form and breaks on stricter
/// mirrors and proxies. The documented form is `@scope%2Fname`; only the `/`
/// inside a scoped name needs encoding, every other character npm permits in
/// a package name is already path-safe.
fn encode_package_name(name: &str) -> String {
    name.replace('/', "%2F")
}

/// Emit a one-line summary of registry version strings we could not read.
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
        "warning: {}: {} version{} from the npm registry could not be parsed and {} skipped: {}{}",
        name,
        unparsed.len(),
        if unparsed.len() == 1 { "" } else { "s" },
        if unparsed.len() == 1 { "was" } else { "were" },
        shown.join(", "),
        suffix
    );
}

#[derive(Debug, Deserialize)]
struct NpmPackageResponse {
    name: String,
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    versions: HashMap<String, serde_json::Value>,
    /// npm's registry response includes a `time` map keyed by version
    /// (plus "created"/"modified" entries we ignore) with ISO-8601 dates.
    #[serde(default)]
    time: HashMap<String, String>,
}

#[derive(Clone)]
pub struct NpmClient {
    client: reqwest::Client,
    include_prerelease: bool,
}

impl NpmClient {
    pub fn new(include_prerelease: bool) -> Self {
        Self {
            client: reqwest::Client::new(),
            include_prerelease,
        }
    }

    /// Get package info from npm registry.
    ///
    /// Over-fetching (the full packument per package, no caching) is knowingly
    /// not addressed here.
    /// The abbreviated packument (`Accept:
    /// application/vnd.npm.install-v1+json`) is far smaller but omits the
    /// `time` map, which is exactly what `published_at` - and therefore the
    /// release-date column - is built from. Dropping to it would trade a
    /// user-visible feature for bandwidth. Conditional requests need a cache
    /// store this workspace does not have; see
    /// `ccu/src/cratesio.rs::get_package`.
    pub async fn get_package(&self, name: &str) -> std::result::Result<PackageInfo, FetchError> {
        let url = format!("{NPM_REGISTRY}/{}", encode_package_name(name));

        let response = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
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
                FetchErrorKind::NotFound => format!("Package '{name}' not found on npm"),
                FetchErrorKind::RateLimited => {
                    format!("The npm registry rate-limited the request for '{name}' ({status})")
                }
                _ => format!("npm registry request for '{name}' failed with status {status}"),
            };
            return Err(FetchError::new(name, kind, detail));
        }

        let data: NpmPackageResponse = response.json().await.map_err(|e| {
            FetchError::new(
                name,
                FetchErrorKind::Parse,
                format!("Failed to parse npm response for '{name}': {e}"),
            )
        })?;

        self.build_package_info(name, data)
    }

    /// Pure half of [`Self::get_package`], exercised against fixtures.
    fn build_package_info(
        &self,
        name: &str,
        data: NpmPackageResponse,
    ) -> std::result::Result<PackageInfo, FetchError> {
        // Parse every published version first, then filter. `core::Version`
        // is strict since wave 1, so unreadable strings are dropped - count
        // them instead of swallowing them.
        let mut unparsed: Vec<String> = Vec::new();
        let mut all_versions: Vec<Version> = Vec::new();
        for key in data.versions.keys() {
            match Version::from_str(key) {
                Ok(v) => all_versions.push(v),
                Err(_) => unparsed.push(key.clone()),
            }
        }
        warn_unparsed(name, &unparsed);
        all_versions.sort();

        // A package where nothing parsed used to produce
        // `latest = 0.0.0`, which compares below every installed version and
        // renders as "All dependencies are up to date!" - a total failure
        // presented as a clean result. Fail loudly, as ccu does.
        if all_versions.is_empty() {
            return Err(FetchError::new(
                name,
                FetchErrorKind::NoUsableVersions,
                format!(
                    "No usable versions for package '{name}' ({} unreadable of {} published)",
                    unparsed.len(),
                    data.versions.len()
                ),
            ));
        }

        let versions: Vec<Version> = if self.include_prerelease {
            all_versions.clone()
        } else {
            all_versions
                .iter()
                .filter(|v| !v.is_prerelease())
                .cloned()
                .collect()
        };

        if versions.is_empty() {
            return Err(FetchError::new(
                name,
                FetchErrorKind::NoStableVersions,
                format!(
                    "No stable versions found for package '{name}' (use --pre-release to include pre-releases)"
                ),
            ));
        }

        // `latest` feeds the resolver's force and
        // fallback target, so it must obey the same prerelease policy as
        // `versions`:
        //
        // - without `-p`, a `dist-tags.latest` pointing at a prerelease must
        //   not be used, or `--force` writes a prerelease the user excluded;
        // - with `-p`, `dist-tags.latest` must not cap the target either, or
        //   `-p -uf` can never upgrade *to* a prerelease.
        //
        // In stable mode the npm `latest` tag stays authoritative - it is
        // deliberately allowed to point below the highest published stable
        // version (a maintenance release on an older line).
        let tagged_latest = data
            .dist_tags
            .get("latest")
            .and_then(|v| Version::from_str(v).ok())
            .filter(|v| self.include_prerelease || !v.is_prerelease());

        let newest = versions
            .last()
            .ok_or_else(|| {
                FetchError::new(
                    name,
                    FetchErrorKind::NoUsableVersions,
                    format!("No versions found for package '{name}'"),
                )
            })?
            .clone();

        let latest = match tagged_latest {
            // Prereleases requested: never let the stable tag cap the target.
            Some(tag) if self.include_prerelease => tag.max(newest),
            // Stable mode: npm's `latest` tag is the recommended release even
            // when a higher stable version exists on another line.
            Some(tag) => tag,
            None => newest,
        };

        let latest_stable = versions.iter().rfind(|v| !v.is_prerelease()).cloned();

        // Map publish dates from the npm `time` field. Skip the
        // "created"/"modified" meta-entries since they're not version keys.
        let mut published_at: HashMap<String, String> = HashMap::new();
        for v in &versions {
            if let Some(date) = data.time.get(&v.original) {
                published_at.insert(v.original.clone(), date.clone());
            }
        }

        Ok(PackageInfo {
            name: data.name,
            versions,
            latest,
            latest_stable,
            published_at,
        })
    }

    /// Get multiple packages concurrently with progress callback and rate limiting
    pub async fn get_packages(
        &self,
        names: &[String],
        progress_callback: impl Fn(usize, usize) + Send + Sync + 'static,
    ) -> Vec<(String, std::result::Result<PackageInfo, FetchError>)> {
        let total = names.len();
        let progress_callback = Arc::new(progress_callback);
        let semaphore = Arc::new(Semaphore::new(10));
        // Completions arrive out of order under the semaphore, so
        // report a real completion count, not a spawn index.
        let completed = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();

        for name in names {
            let client = self.clone();
            let name = name.clone();
            let semaphore = Arc::clone(&semaphore);
            let callback = Arc::clone(&progress_callback);
            let completed = Arc::clone(&completed);

            let task = tokio::spawn(async move {
                let _permit = semaphore.acquire().await.expect("semaphore closed");
                let result = client.get_package(&name).await;
                callback(completed.fetch_add(1, Ordering::SeqCst) + 1, total);
                (name, result)
            });

            tasks.push(task);
        }

        // Zipping with `names` keeps the package name available even when the
        // task panicked (this used to be the literal "unknown").
        let mut results = Vec::new();
        for (name, task) in names.iter().zip(tasks) {
            match task.await {
                Ok(result) => results.push(result),
                Err(e) => results.push((
                    name.clone(),
                    Err(FetchError::new(
                        name.clone(),
                        FetchErrorKind::TaskFailed,
                        format!("Task for '{name}' failed: {e}"),
                    )),
                )),
            }
        }

        results
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_package_express() {
        let client = NpmClient::new(false);
        let result = client.get_package("express").await;
        assert!(result.is_ok());
        let info = result.expect("should succeed");
        assert_eq!(info.name, "express");
        assert!(!info.versions.is_empty());
    }

    #[tokio::test]
    async fn test_get_package_not_found() {
        let client = NpmClient::new(false);
        let result = client
            .get_package("this-package-definitely-does-not-exist-12345")
            .await;
        let err = result.expect_err("should fail");
        assert_eq!(err.kind, FetchErrorKind::NotFound);
        assert_eq!(err.package, "this-package-definitely-does-not-exist-12345");
    }

    fn fixture(json: &str) -> NpmPackageResponse {
        serde_json::from_str(json).expect("fixture parses")
    }

    const PRERELEASE_LATEST: &str = r#"{
        "name": "demo",
        "dist-tags": { "latest": "2.0.0-rc.1" },
        "versions": { "1.4.0": {}, "1.5.0": {}, "2.0.0-rc.1": {} },
        "time": { "1.5.0": "2024-03-01T00:00:00Z" }
    }"#;

    #[test]
    fn prerelease_dist_tag_is_not_used_as_latest_without_pre_flag() {
        let client = NpmClient::new(false);
        let info = client
            .build_package_info("demo", fixture(PRERELEASE_LATEST))
            .expect("builds");
        // Without `-p`, latest must stay inside the filtered set.
        assert_eq!(info.latest.original, "1.5.0");
        assert!(info.versions.contains(&info.latest));
        assert!(!info.latest.is_prerelease());
    }

    #[test]
    fn prerelease_is_reachable_as_latest_with_pre_flag() {
        let client = NpmClient::new(true);
        let info = client
            .build_package_info("demo", fixture(PRERELEASE_LATEST))
            .expect("builds");
        assert_eq!(info.latest.original, "2.0.0-rc.1");
        assert_eq!(
            info.latest_stable.as_ref().map(|v| v.original.as_str()),
            Some("1.5.0")
        );
    }

    #[test]
    fn stable_tag_does_not_cap_the_target_when_prereleases_are_requested() {
        // `-p -uf` must be able to reach a prerelease that
        // is newer than the `latest` tag.
        let client = NpmClient::new(true);
        let json = r#"{
            "name": "demo",
            "dist-tags": { "latest": "1.5.0" },
            "versions": { "1.5.0": {}, "2.0.0-beta.2": {} },
            "time": {}
        }"#;
        let info = client
            .build_package_info("demo", fixture(json))
            .expect("builds");
        assert_eq!(info.latest.original, "2.0.0-beta.2");
    }

    #[test]
    fn no_parseable_version_is_an_error_not_zero_zero_zero() {
        // This used to yield latest = 0.0.0 and render
        // as "All dependencies are up to date!".
        let client = NpmClient::new(false);
        let json = r#"{
            "name": "demo",
            "dist-tags": {},
            "versions": { "not-a-version": {}, "also bad": {} },
            "time": {}
        }"#;
        let err = client
            .build_package_info("demo", fixture(json))
            .expect_err("no usable versions");
        assert_eq!(err.kind, FetchErrorKind::NoUsableVersions);
        assert!(!err.kind.is_missing());
        assert_eq!(err.package, "demo");
    }

    #[test]
    fn only_prereleases_reports_no_stable_rather_than_a_bogus_latest() {
        let client = NpmClient::new(false);
        let json = r#"{
            "name": "demo",
            "dist-tags": { "latest": "1.0.0-rc.1" },
            "versions": { "1.0.0-rc.1": {} },
            "time": {}
        }"#;
        let err = client
            .build_package_info("demo", fixture(json))
            .expect_err("no stable versions");
        assert_eq!(err.kind, FetchErrorKind::NoStableVersions);
    }

    #[test]
    fn publish_dates_are_mapped_from_the_time_field() {
        let client = NpmClient::new(false);
        let info = client
            .build_package_info("demo", fixture(PRERELEASE_LATEST))
            .expect("builds");
        assert_eq!(
            info.published_at.get("1.5.0").map(String::as_str),
            Some("2024-03-01T00:00:00Z")
        );
    }

    #[test]
    fn scoped_names_are_percent_encoded_in_the_path() {
        // The documented registry form is `@scope%2Fname`; stricter mirrors
        // reject the raw slash.
        assert_eq!(encode_package_name("@scope/name"), "@scope%2Fname");
        assert_eq!(encode_package_name("express"), "express");
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
            classify_status(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
            FetchErrorKind::ServerError
        );
        assert!(FetchErrorKind::Network.is_transient());
        assert!(!FetchErrorKind::NotFound.is_transient());
    }
}
