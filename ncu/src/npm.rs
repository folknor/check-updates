use anyhow::{Context, Result};
use check_updates_core::{PackageInfo, Version};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;

const NPM_REGISTRY: &str = "https://registry.npmjs.org";

#[derive(Debug, Deserialize)]
struct NpmPackageResponse {
    name: String,
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    versions: HashMap<String, serde_json::Value>,
}

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
        let url = format!("{}/{}", NPM_REGISTRY, name);

        let response = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("Failed to fetch package: {}", name))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("Package '{}' not found on npm", name);
        }

        let data: NpmPackageResponse = response
            .json()
            .await
            .with_context(|| format!("Failed to parse npm response for: {}", name))?;

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

        let latest_stable = versions.iter().filter(|v| !v.is_prerelease()).last().cloned();

        Ok(PackageInfo {
            name: data.name,
            versions,
            latest,
            latest_stable,
        })
    }

    /// Get multiple packages concurrently
    pub async fn get_packages(&self, names: &[String]) -> Vec<(String, Result<PackageInfo>)> {
        let futures: Vec<_> = names
            .iter()
            .map(|name| {
                let name = name.clone();
                let client = self.clone();
                async move {
                    let result = client.get_package(&name).await;
                    (name, result)
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }
}

impl Clone for NpmClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            include_prerelease: self.include_prerelease,
        }
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
        let info = result.unwrap();
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
