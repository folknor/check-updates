use anyhow::{Context, Result};
use check_updates_core::{PackageInfo, Version};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Semaphore;

const NPM_REGISTRY: &str = "https://registry.npmjs.org";

#[derive(Debug, Deserialize)]
struct NpmPackageResponse {
    name: String,
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    versions: HashMap<String, serde_json::Value>,
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

    /// Get package info from npm registry
    pub async fn get_package(&self, name: &str) -> Result<PackageInfo> {
        let url = format!("{NPM_REGISTRY}/{name}");

        let response = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("Failed to fetch package: {name}"))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("Package '{name}' not found on npm");
        }

        let data: NpmPackageResponse = response
            .json()
            .await
            .with_context(|| format!("Failed to parse npm response for: {name}"))?;

        let mut versions: Vec<Version> = data
            .versions
            .keys()
            .filter_map(|v| Version::from_str(v).ok())
            .filter(|v| self.include_prerelease || !v.is_prerelease())
            .collect();

        versions.sort();

        let latest = data
            .dist_tags
            .get("latest")
            .and_then(|v| Version::from_str(v).ok())
            .unwrap_or_else(|| versions.last().cloned().unwrap_or_else(|| Version::new(0, 0, 0)));

        let latest_stable = versions.iter().rfind(|v| !v.is_prerelease()).cloned();

        Ok(PackageInfo {
            name: data.name,
            versions,
            latest,
            latest_stable,
        })
    }

    /// Get multiple packages concurrently with progress callback and rate limiting
    pub async fn get_packages(
        &self,
        names: &[String],
        progress_callback: impl Fn(usize, usize) + Send + Sync + 'static,
    ) -> Vec<(String, Result<PackageInfo>)> {
        let total = names.len();
        let progress_callback = Arc::new(progress_callback);
        let semaphore = Arc::new(Semaphore::new(10));

        let mut tasks = Vec::new();

        for name in names {
            let client = self.clone();
            let name = name.clone();
            let semaphore = Arc::clone(&semaphore);

            let task = tokio::spawn(async move {
                let _permit = semaphore.acquire().await.expect("semaphore closed");
                let result = client.get_package(&name).await;
                (name, result)
            });

            tasks.push(task);
        }

        let mut results = Vec::new();
        for (i, task) in tasks.into_iter().enumerate() {
            match task.await {
                Ok(result) => results.push(result),
                Err(e) => results.push(("unknown".to_string(), Err(anyhow::anyhow!("Task failed: {e}")))),
            }
            progress_callback(i + 1, total);
        }

        results
    }
}

#[cfg(test)]
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
        assert!(result.is_err());
    }
}
