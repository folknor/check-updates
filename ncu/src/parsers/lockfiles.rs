use anyhow::{Context, Result};
use check_updates_core::Version;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use crate::detector::LockfileType;

pub struct LockfileParser;

impl LockfileParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse installed versions from a lock file
    pub fn parse(&self, path: &Path, lockfile_type: LockfileType) -> Result<HashMap<String, Version>> {
        match lockfile_type {
            LockfileType::Npm => self.parse_package_lock(path),
            LockfileType::Pnpm => self.parse_pnpm_lock(path),
            LockfileType::Yarn => self.parse_yarn_lock(path),
            LockfileType::Bun => self.parse_bun_lock(path),
        }
    }

    /// Parse package-lock.json (npm)
    fn parse_package_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let mut versions = HashMap::new();

        // npm v7+ format: packages field with "" for root and "node_modules/pkg" for deps
        if let Some(packages) = parsed.get("packages").and_then(|v| v.as_object()) {
            for (key, pkg_data) in packages {
                // Skip root package (empty key)
                if key.is_empty() {
                    continue;
                }

                // Extract package name from path (node_modules/name or node_modules/@scope/name)
                let name = key
                    .strip_prefix("node_modules/")
                    .unwrap_or(key)
                    .to_string();

                // Skip nested node_modules
                if name.contains("node_modules/") {
                    continue;
                }

                if let Some(version_str) = pkg_data.get("version").and_then(|v| v.as_str()) {
                    if let Ok(version) = Version::from_str(version_str) {
                        versions.insert(name, version);
                    }
                }
            }
        }
        // npm v6 format: dependencies field
        else if let Some(dependencies) = parsed.get("dependencies").and_then(|v| v.as_object()) {
            self.parse_npm_v6_deps(dependencies, &mut versions);
        }

        Ok(versions)
    }

    fn parse_npm_v6_deps(
        &self,
        deps: &serde_json::Map<String, serde_json::Value>,
        versions: &mut HashMap<String, Version>,
    ) {
        for (name, data) in deps {
            if let Some(version_str) = data.get("version").and_then(|v| v.as_str()) {
                if let Ok(version) = Version::from_str(version_str) {
                    versions.insert(name.clone(), version);
                }
            }
        }
    }

    /// Parse pnpm-lock.yaml
    fn parse_pnpm_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let mut versions = HashMap::new();

        // pnpm v9 format: snapshots or packages
        // Package entries like: "express@4.18.2" or "express@4.18.2(supports-color@8.0.0)"
        if let Some(packages) = parsed.get("packages").and_then(|v| v.as_mapping()) {
            for (key, _) in packages {
                if let Some(key_str) = key.as_str() {
                    if let Some((name, version)) = Self::parse_pnpm_package_key(key_str) {
                        versions.insert(name, version);
                    }
                }
            }
        }

        // Also check snapshots (pnpm v9)
        if let Some(snapshots) = parsed.get("snapshots").and_then(|v| v.as_mapping()) {
            for (key, _) in snapshots {
                if let Some(key_str) = key.as_str() {
                    if let Some((name, version)) = Self::parse_pnpm_package_key(key_str) {
                        versions.entry(name).or_insert(version);
                    }
                }
            }
        }

        Ok(versions)
    }

    /// Parse pnpm package key like "express@4.18.2" or "@types/node@20.0.0"
    fn parse_pnpm_package_key(key: &str) -> Option<(String, Version)> {
        // Handle scoped packages: @scope/name@version
        let (name, version_str) = if key.starts_with('@') {
            // Find the second @ which separates name from version
            let rest = &key[1..];
            if let Some(at_pos) = rest.find('@') {
                let name = &key[..at_pos + 1];
                let version_part = &rest[at_pos + 1..];
                // Remove any peer dep suffix like (supports-color@8.0.0)
                let version_str = version_part.split('(').next().unwrap_or(version_part);
                (name.to_string(), version_str)
            } else {
                return None;
            }
        } else {
            // Regular package: name@version
            let parts: Vec<&str> = key.splitn(2, '@').collect();
            if parts.len() != 2 {
                return None;
            }
            let version_str = parts[1].split('(').next().unwrap_or(parts[1]);
            (parts[0].to_string(), version_str)
        };

        Version::from_str(version_str).ok().map(|v| (name, v))
    }

    /// Parse yarn.lock (yarn classic and berry)
    fn parse_yarn_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let mut versions = HashMap::new();

        // Yarn lock format is custom, not YAML
        // Entry format:
        // "package@^1.0.0":
        //   version "1.2.3"
        let mut current_packages: Vec<String> = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();

            // Package header line (may have multiple packages)
            if !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !trimmed.starts_with("version")
                && !trimmed.starts_with("resolved")
                && !trimmed.starts_with("integrity")
                && !trimmed.starts_with("dependencies")
                && !line.starts_with(' ')
                && !line.starts_with('\t')
            {
                current_packages = Self::parse_yarn_header(trimmed);
            }

            // Version line
            if trimmed.starts_with("version") {
                if let Some(version) = Self::parse_yarn_version_line(trimmed) {
                    for pkg in &current_packages {
                        versions.entry(pkg.clone()).or_insert_with(|| version.clone());
                    }
                }
            }
        }

        Ok(versions)
    }

    fn parse_yarn_header(line: &str) -> Vec<String> {
        // Format: "pkg@^1.0.0", "pkg@~1.0.0":
        // or: pkg@^1.0.0, pkg@~1.0.0:
        let line = line.trim_end_matches(':');
        let mut packages = Vec::new();

        for part in line.split(", ") {
            let part = part.trim().trim_matches('"');
            // Extract package name (before the @version part)
            if let Some(name) = Self::extract_package_name(part) {
                packages.push(name);
            }
        }

        packages
    }

    fn extract_package_name(spec: &str) -> Option<String> {
        // Handle @scope/name@version
        if spec.starts_with('@') {
            let rest = &spec[1..];
            if let Some(at_pos) = rest.find('@') {
                return Some(spec[..at_pos + 1].to_string());
            }
        } else if let Some(at_pos) = spec.find('@') {
            return Some(spec[..at_pos].to_string());
        }
        None
    }

    fn parse_yarn_version_line(line: &str) -> Option<Version> {
        // version "1.2.3" or version: "1.2.3"
        let line = line.trim_start_matches("version").trim();
        let line = line.trim_start_matches(':').trim();
        let version_str = line.trim_matches('"');
        Version::from_str(version_str).ok()
    }

    /// Parse bun.lockb (binary format - limited support)
    fn parse_bun_lock(&self, _path: &Path) -> Result<HashMap<String, Version>> {
        // bun.lockb is a binary format, difficult to parse without bun itself
        // For now, return empty and rely on package.json versions
        Ok(HashMap::new())
    }
}

impl Default for LockfileParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_package_lock_v7() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".json")?;
        writeln!(
            file,
            r#"{{
  "name": "test",
  "lockfileVersion": 3,
  "packages": {{
    "": {{}},
    "node_modules/express": {{
      "version": "4.18.2"
    }},
    "node_modules/lodash": {{
      "version": "4.17.21"
    }}
  }}
}}"#
        )?;

        let parser = LockfileParser::new();
        let versions = parser.parse(file.path(), LockfileType::Npm)?;

        assert_eq!(versions.get("express").unwrap().to_string(), "4.18.2");
        assert_eq!(versions.get("lodash").unwrap().to_string(), "4.17.21");

        Ok(())
    }

    #[test]
    fn test_parse_pnpm_package_key() {
        let (name, version) = LockfileParser::parse_pnpm_package_key("express@4.18.2").unwrap();
        assert_eq!(name, "express");
        assert_eq!(version.to_string(), "4.18.2");

        let (name, version) =
            LockfileParser::parse_pnpm_package_key("@types/node@20.0.0").unwrap();
        assert_eq!(name, "@types/node");
        assert_eq!(version.to_string(), "20.0.0");
    }
}
