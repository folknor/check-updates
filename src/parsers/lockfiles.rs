use crate::version::Version;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

/// Parser for various lock files to get installed versions
pub struct LockfileParser;

/// Structure for parsing uv.lock and poetry.lock [[package]] sections
#[derive(Debug, Deserialize)]
struct TomlPackage {
    name: String,
    version: String,
}

/// Structure for parsing TOML lock files with [[package]] arrays
#[derive(Debug, Deserialize)]
struct TomlLockFile {
    package: Vec<TomlPackage>,
}

/// Structure for parsing pdm.lock which has [package.metadata] section
#[derive(Debug, Deserialize)]
struct PdmLockFile {
    package: Vec<PdmPackage>,
}

#[derive(Debug, Deserialize)]
struct PdmPackage {
    name: String,
    version: String,
}

impl LockfileParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse a lock file and return a map of package name -> installed version
    pub fn parse(&self, path: &PathBuf) -> Result<HashMap<String, Version>> {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid lock file path")?;

        match filename {
            "uv.lock" => self.parse_uv_lock(path),
            "poetry.lock" => self.parse_poetry_lock(path),
            "pdm.lock" => self.parse_pdm_lock(path),
            _ => anyhow::bail!("Unsupported lock file: {}", filename),
        }
    }

    /// Try to find and parse any lock file in the given directory
    pub fn find_and_parse(&self, dir: &PathBuf) -> Result<HashMap<String, Version>> {
        // Priority order: uv.lock, poetry.lock, pdm.lock
        let lock_files = ["uv.lock", "poetry.lock", "pdm.lock"];

        for filename in &lock_files {
            let lock_path = dir.join(filename);
            if lock_path.exists() {
                return self.parse(&lock_path);
            }
        }

        // No lock file found - return empty map
        Ok(HashMap::new())
    }

    /// Check if we can parse this lock file
    pub fn can_parse(&self, path: &PathBuf) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| {
                n == "uv.lock"
                    || n == "poetry.lock"
                    || n == "pdm.lock"
                    || n == "Pipfile.lock"
                    || n == "conda-lock.yml"
            })
            .unwrap_or(false)
    }

    /// Parse uv.lock file (TOML format with [[package]] sections)
    fn parse_uv_lock(&self, path: &PathBuf) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read uv.lock at {:?}", path))?;

        let lock_file: TomlLockFile = toml::from_str(&content)
            .with_context(|| format!("Failed to parse uv.lock at {:?}", path))?;

        let mut versions = HashMap::new();
        for package in lock_file.package {
            // Normalize package name to lowercase
            let name = package.name.to_lowercase().replace('_', "-");

            match Version::from_str(&package.version) {
                Ok(version) => {
                    versions.insert(name, version);
                }
                Err(e) => {
                    // Log warning but continue parsing other packages
                    eprintln!(
                        "Warning: Failed to parse version '{}' for package '{}': {}",
                        package.version, package.name, e
                    );
                }
            }
        }

        Ok(versions)
    }

    /// Parse poetry.lock file (TOML format with [[package]] sections)
    fn parse_poetry_lock(&self, path: &PathBuf) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read poetry.lock at {:?}", path))?;

        let lock_file: TomlLockFile = toml::from_str(&content)
            .with_context(|| format!("Failed to parse poetry.lock at {:?}", path))?;

        let mut versions = HashMap::new();
        for package in lock_file.package {
            // Normalize package name to lowercase
            let name = package.name.to_lowercase().replace('_', "-");

            match Version::from_str(&package.version) {
                Ok(version) => {
                    versions.insert(name, version);
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to parse version '{}' for package '{}': {}",
                        package.version, package.name, e
                    );
                }
            }
        }

        Ok(versions)
    }

    /// Parse pdm.lock file (TOML format)
    fn parse_pdm_lock(&self, path: &PathBuf) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read pdm.lock at {:?}", path))?;

        let lock_file: PdmLockFile = toml::from_str(&content)
            .with_context(|| format!("Failed to parse pdm.lock at {:?}", path))?;

        let mut versions = HashMap::new();
        for package in lock_file.package {
            // Normalize package name to lowercase
            let name = package.name.to_lowercase().replace('_', "-");

            match Version::from_str(&package.version) {
                Ok(version) => {
                    versions.insert(name, version);
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to parse version '{}' for package '{}': {}",
                        package.version, package.name, e
                    );
                }
            }
        }

        Ok(versions)
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
    fn test_parse_uv_lock() {
        let lock_content = r#"
version = 1

[[package]]
name = "requests"
version = "2.31.0"

[[package]]
name = "numpy"
version = "1.24.3"

[[package]]
name = "flask"
version = "2.3.0"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = LockfileParser::new();
        let versions = parser.parse_uv_lock(&path).unwrap();

        assert_eq!(versions.len(), 3);
        assert_eq!(versions.get("requests").unwrap().to_string(), "2.31.0");
        assert_eq!(versions.get("numpy").unwrap().to_string(), "1.24.3");
        assert_eq!(versions.get("flask").unwrap().to_string(), "2.3.0");
    }

    #[test]
    fn test_parse_poetry_lock() {
        let lock_content = r#"
[[package]]
name = "requests"
version = "2.31.0"
description = "Python HTTP for Humans."

[[package]]
name = "Django"
version = "4.2.0"
description = "A high-level Python Web framework"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = LockfileParser::new();
        let versions = parser.parse_poetry_lock(&path).unwrap();

        assert_eq!(versions.len(), 2);
        assert_eq!(versions.get("requests").unwrap().to_string(), "2.31.0");
        assert_eq!(versions.get("django").unwrap().to_string(), "4.2.0");
    }

    #[test]
    fn test_parse_pdm_lock() {
        let lock_content = r#"
[[package]]
name = "click"
version = "8.1.3"

[[package]]
name = "Flask"
version = "2.3.0"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = LockfileParser::new();
        let versions = parser.parse_pdm_lock(&path).unwrap();

        assert_eq!(versions.len(), 2);
        assert_eq!(versions.get("click").unwrap().to_string(), "8.1.3");
        assert_eq!(versions.get("flask").unwrap().to_string(), "2.3.0");
    }

    #[test]
    fn test_can_parse() {
        let parser = LockfileParser::new();

        assert!(parser.can_parse(&PathBuf::from("uv.lock")));
        assert!(parser.can_parse(&PathBuf::from("poetry.lock")));
        assert!(parser.can_parse(&PathBuf::from("pdm.lock")));
        assert!(!parser.can_parse(&PathBuf::from("requirements.txt")));
    }

    #[test]
    fn test_find_and_parse() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir_path = temp_dir.path().to_path_buf();

        // Create a uv.lock file
        let lock_path = dir_path.join("uv.lock");
        let lock_content = r#"
[[package]]
name = "requests"
version = "2.31.0"
"#;
        fs::write(&lock_path, lock_content).unwrap();

        let parser = LockfileParser::new();
        let versions = parser.find_and_parse(&dir_path).unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions.get("requests").unwrap().to_string(), "2.31.0");
    }

    #[test]
    fn test_find_and_parse_no_lockfile() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir_path = temp_dir.path().to_path_buf();

        let parser = LockfileParser::new();
        let versions = parser.find_and_parse(&dir_path).unwrap();

        // Should return empty map, not an error
        assert_eq!(versions.len(), 0);
    }
}
