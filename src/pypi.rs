use crate::version::Version;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Client for querying PyPI API
pub struct PyPiClient {
    client: reqwest::Client,
    base_url: String,
    include_prerelease: bool,
}

/// Package information from PyPI
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub versions: Vec<Version>,
    pub latest: Version,
    pub latest_stable: Option<Version>,
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

    /// Fetch package info from PyPI
    pub async fn get_package(&self, name: &str) -> Result<PackageInfo> {
        let url = format!("{}/{}/json", self.base_url, name);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context(format!("Failed to fetch package '{}'", name))?;

        if !response.status().is_success() {
            if response.status() == 404 {
                return Err(anyhow!("Package '{}' not found on PyPI", name));
            }
            return Err(anyhow!(
                "PyPI API request failed with status: {}",
                response.status()
            ));
        }

        let pypi_data: PyPiResponse = response
            .json()
            .await
            .context(format!("Failed to parse JSON response for '{}'", name))?;

        // Parse all versions from releases
        let mut all_versions: Vec<Version> = Vec::new();
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

            // Try to parse the version
            if let Ok(version) = Version::from_str(version_str) {
                all_versions.push(version);
            }
        }

        if all_versions.is_empty() {
            return Err(anyhow!("No valid versions found for package '{}'", name));
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
            return Err(anyhow!(
                "No stable versions found for package '{}' (use --pre-release to include pre-releases)",
                name
            ));
        }

        // Get latest version (with or without prerelease)
        let latest = if self.include_prerelease {
            all_versions
                .last()
                .ok_or_else(|| anyhow!("No versions found"))?
                .clone()
        } else {
            filtered_versions
                .last()
                .ok_or_else(|| anyhow!("No stable versions found"))?
                .clone()
        };

        // Get latest stable version (always filter out prereleases)
        let latest_stable = all_versions
            .iter()
            .filter(|v| !v.is_prerelease())
            .last()
            .cloned();

        Ok(PackageInfo {
            name: pypi_data.info.name,
            versions: filtered_versions,
            latest,
            latest_stable,
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

        let mut tasks = Vec::new();

        for (index, name) in names.iter().enumerate() {
            let client = self.clone();
            let name = name.clone();
            let callback = Arc::clone(&progress_callback);
            let semaphore = Arc::clone(&semaphore);

            let task = tokio::spawn(async move {
                // Acquire semaphore permit
                let _permit = semaphore.acquire().await.unwrap();

                let result = client.get_package(&name).await;

                // Call progress callback
                callback(index + 1, total);

                (name, result)
            });

            tasks.push(task);
        }

        // Wait for all tasks to complete
        let mut packages = HashMap::new();
        let mut errors = Vec::new();

        for task in tasks {
            match task.await {
                Ok((name, Ok(package_info))) => {
                    packages.insert(name, package_info);
                }
                Ok((name, Err(e))) => {
                    // Extract just the error message without "Failed to fetch" prefix
                    let error_msg = e.to_string();
                    errors.push((name, error_msg));
                }
                Err(e) => {
                    errors.push(("unknown".to_string(), format!("Task failed: {}", e)));
                }
            }
        }

        // Format errors as strings
        let formatted_errors: Vec<String> = errors
            .into_iter()
            .map(|(name, msg)| format!("{}: {}", name, msg))
            .collect();

        // If we have some results, return them even if some packages failed
        if !packages.is_empty() || formatted_errors.is_empty() {
            Ok(GetPackagesResult {
                packages,
                errors: formatted_errors,
            })
        } else {
            // All packages failed
            Err(anyhow!(
                "Failed to fetch all packages:\n{}",
                formatted_errors.join("\n")
            ))
        }
    }
}

/// Result of fetching multiple packages
#[derive(Debug, Clone)]
pub struct GetPackagesResult {
    pub packages: HashMap<String, PackageInfo>,
    pub errors: Vec<String>,
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_package_requests() {
        let client = PyPiClient::new(false);
        let result = client.get_package("requests").await;

        assert!(result.is_ok(), "Failed to fetch requests package: {:?}", result.err());

        let package_info = result.unwrap();
        assert_eq!(package_info.name.to_lowercase(), "requests");
        assert!(!package_info.versions.is_empty());
        assert!(package_info.latest_stable.is_some());
    }

    #[tokio::test]
    async fn test_get_package_not_found() {
        let client = PyPiClient::new(false);
        let result = client.get_package("this-package-definitely-does-not-exist-12345").await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_get_packages_concurrent() {
        let client = PyPiClient::new(false);
        let packages = vec![
            "requests".to_string(),
            "flask".to_string(),
        ];

        // Use Arc<AtomicUsize> for thread-safe counter
        let progress_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress_calls_clone = Arc::clone(&progress_calls);

        let result = client.get_packages(&packages, move |_current, _total| {
            progress_calls_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }).await;

        assert!(result.is_ok(), "Failed to fetch packages: {:?}", result.err());

        let results = result.unwrap();
        assert!(!results.packages.is_empty());

        // Verify progress callback was called
        let calls = progress_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(calls > 0, "Progress callback should have been called");
    }

    #[tokio::test]
    async fn test_custom_index_url() {
        let client = PyPiClient::new(false)
            .with_index_url("https://pypi.org/pypi/");

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

        if result_stable.is_ok() && result_pre.is_ok() {
            let stable = result_stable.unwrap();
            let pre = result_pre.unwrap();

            // Pre-release client might have more versions
            assert!(pre.versions.len() >= stable.versions.len());
        }
    }
}
