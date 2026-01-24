use check_updates_core::{PackageInfo, Version};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Semaphore;

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

#[derive(Debug, Deserialize)]
struct CrateVersion {
    num: String,
    yanked: bool,
}

impl CratesIoClient {
    pub fn new(include_prerelease: bool) -> Self {
        Self {
            client: reqwest::Client::builder()
                // crates.io requires a user-agent with contact info
                .user_agent("cargo-check-updates/0.1.0 (https://github.com/folknor/cargo-check-updates)")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: "https://crates.io/api/v1/crates".to_string(),
            include_prerelease,
        }
    }

    /// Fetch package info from crates.io
    pub async fn get_package(&self, name: &str) -> Result<PackageInfo> {
        let url = format!("{}/{}", self.base_url, name);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context(format!("Failed to fetch crate '{}'", name))?;

        if !response.status().is_success() {
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(anyhow!("Crate '{}' not found on crates.io", name));
            }
            return Err(anyhow!(
                "crates.io API request failed with status: {}",
                response.status()
            ));
        }

        let crate_data: CrateResponse = response
            .json()
            .await
            .context(format!("Failed to parse JSON response for '{}'", name))?;

        // Parse all versions, skipping yanked ones
        let mut all_versions: Vec<Version> = Vec::new();
        for version in &crate_data.versions {
            if version.yanked {
                continue;
            }

            if let Ok(v) = Version::from_str(&version.num) {
                all_versions.push(v);
            }
        }

        if all_versions.is_empty() {
            return Err(anyhow!("No valid versions found for crate '{}'", name));
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
                "No stable versions found for crate '{}' (use --pre-release to include pre-releases)",
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
            name: crate_data.crate_info.name,
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
        // crates.io has rate limits, so be conservative
        let semaphore = Arc::new(Semaphore::new(5));

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
                "Failed to fetch all crates:\n{}",
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_package_serde() {
        let client = CratesIoClient::new(false);
        let result = client.get_package("serde").await;

        assert!(result.is_ok(), "Failed to fetch serde crate: {:?}", result.err());

        let package_info = result.unwrap();
        assert_eq!(package_info.name.to_lowercase(), "serde");
        assert!(!package_info.versions.is_empty());
        assert!(package_info.latest_stable.is_some());
    }

    #[tokio::test]
    async fn test_get_package_not_found() {
        let client = CratesIoClient::new(false);
        let result = client.get_package("this-crate-definitely-does-not-exist-12345").await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
