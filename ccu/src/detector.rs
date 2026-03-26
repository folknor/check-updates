use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use toml::Value;

/// Detected Cargo.toml file
#[derive(Debug, Clone)]
pub struct DetectedFile {
    pub path: PathBuf,
}

/// Detects Cargo.toml files in a project, including workspace members
pub struct ProjectDetector {
    project_path: PathBuf,
}

impl ProjectDetector {
    pub fn new(project_path: PathBuf) -> Self {
        Self { project_path }
    }

    /// Detect all Cargo.toml files in the project (root + workspace members)
    pub fn detect(&self) -> Result<Vec<DetectedFile>> {
        let mut detected = Vec::new();

        // Check for Cargo.toml in project root
        let cargo_toml = self.project_path.join("Cargo.toml");
        if !cargo_toml.exists() {
            return Ok(detected);
        }

        detected.push(DetectedFile {
            path: cargo_toml.clone(),
        });

        // Parse root Cargo.toml to find workspace members
        let content = fs::read_to_string(&cargo_toml)
            .with_context(|| format!("Failed to read {}", cargo_toml.display()))?;

        let parsed: Value = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", cargo_toml.display()))?;

        // Look for [workspace] section
        if let Some(workspace) = parsed.get("workspace").and_then(|v| v.as_table()) {
            // Collect excluded patterns
            let excludes: Vec<&str> = workspace
                .get("exclude")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect()
                })
                .unwrap_or_default();

            if let Some(members) = workspace.get("members").and_then(|v| v.as_array()) {
                // Explicit members list
                for member in members {
                    if let Some(pattern) = member.as_str() {
                        let member_tomls = self.expand_workspace_member(pattern)?;
                        for path in member_tomls {
                            if path != cargo_toml && !self.is_excluded(&path, &excludes) {
                                detected.push(DetectedFile { path });
                            }
                        }
                    }
                }
            } else {
                // No members field: auto-discover subdirectories with Cargo.toml
                let discovered = self.auto_discover_members()?;
                for path in discovered {
                    if path != cargo_toml && !self.is_excluded(&path, &excludes) {
                        detected.push(DetectedFile { path });
                    }
                }
            }
        }

        Ok(detected)
    }

    /// Expand a workspace member pattern (may contain globs like "crates/*")
    fn expand_workspace_member(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();

        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            // Handle glob pattern
            let full_pattern = self.project_path.join(pattern).join("Cargo.toml");
            let pattern_str = full_pattern.to_string_lossy();

            let paths = glob::glob(&pattern_str)
                .with_context(|| format!("Invalid workspace member glob pattern: {pattern}"))?;

            for entry in paths {
                let path = entry.with_context(|| {
                    format!("Error reading glob match for pattern: {pattern}")
                })?;
                if path.exists() {
                    results.push(path);
                }
            }
        } else {
            // Direct path, no glob
            let member_toml = self.project_path.join(pattern).join("Cargo.toml");
            if member_toml.exists() {
                results.push(member_toml);
            }
        }

        Ok(results)
    }

    /// Auto-discover workspace members by recursively scanning subdirectories for Cargo.toml.
    /// Skips `target/` and hidden directories.
    fn auto_discover_members(&self) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();
        self.scan_dir_recursive(&self.project_path, &mut results)?;
        results.sort();
        Ok(results)
    }

    fn scan_dir_recursive(&self, dir: &std::path::Path, results: &mut Vec<PathBuf>) -> Result<()> {
        let entries = fs::read_dir(dir)
            .with_context(|| format!("Failed to read directory {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip target directories and hidden directories
            if name_str == "target" || name_str.starts_with('.') {
                continue;
            }

            let candidate = entry.path().join("Cargo.toml");
            if candidate.exists() {
                results.push(candidate);
            }

            // Continue scanning deeper
            self.scan_dir_recursive(&entry.path(), results)?;
        }

        Ok(())
    }

    /// Check if a Cargo.toml path matches any of the exclude patterns
    fn is_excluded(&self, path: &std::path::Path, excludes: &[&str]) -> bool {
        // Get the member directory relative to the project root
        let member_dir = match path.parent() {
            Some(p) => p,
            None => return false,
        };
        let relative = match member_dir.strip_prefix(&self.project_path) {
            Ok(r) => r.to_string_lossy(),
            Err(_) => return false,
        };

        for pattern in excludes {
            if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
                // Glob-based exclude
                let full_pattern = self.project_path.join(pattern).join("Cargo.toml");
                if let Ok(glob_pattern) = glob::Pattern::new(&full_pattern.to_string_lossy())
                    && glob_pattern.matches_path(path)
                {
                    return true;
                }
            } else {
                // Literal exclude
                if relative == *pattern {
                    return true;
                }
            }
        }

        false
    }

    /// Check if Cargo.lock exists
    pub fn has_lockfile(&self) -> bool {
        self.project_path.join("Cargo.lock").exists()
    }

    /// Get path to Cargo.lock
    pub fn lockfile_path(&self) -> PathBuf {
        self.project_path.join("Cargo.lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_cargo_toml(dir: &std::path::Path, content: &str) {
        fs::write(dir.join("Cargo.toml"), content).expect("write Cargo.toml");
    }

    #[test]
    fn test_detect_no_cargo_toml() -> Result<()> {
        let tmp = TempDir::new()?;
        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        assert!(detected.is_empty());
        Ok(())
    }

    #[test]
    fn test_detect_single_crate() -> Result<()> {
        let tmp = TempDir::new()?;
        create_cargo_toml(tmp.path(), "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n");
        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        assert_eq!(detected.len(), 1);
        Ok(())
    }

    #[test]
    fn test_detect_workspace_members() -> Result<()> {
        let tmp = TempDir::new()?;
        create_cargo_toml(
            tmp.path(),
            "[workspace]\nmembers = [\"crate-a\", \"crate-b\"]\n",
        );
        fs::create_dir(tmp.path().join("crate-a"))?;
        create_cargo_toml(
            &tmp.path().join("crate-a"),
            "[package]\nname = \"crate-a\"\n",
        );
        fs::create_dir(tmp.path().join("crate-b"))?;
        create_cargo_toml(
            &tmp.path().join("crate-b"),
            "[package]\nname = \"crate-b\"\n",
        );

        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        assert_eq!(detected.len(), 3); // root + 2 members
        Ok(())
    }

    #[test]
    fn test_detect_workspace_glob_pattern() -> Result<()> {
        let tmp = TempDir::new()?;
        create_cargo_toml(tmp.path(), "[workspace]\nmembers = [\"crates/*\"]\n");
        let crates_dir = tmp.path().join("crates");
        fs::create_dir(&crates_dir)?;
        fs::create_dir(crates_dir.join("foo"))?;
        create_cargo_toml(&crates_dir.join("foo"), "[package]\nname = \"foo\"\n");
        fs::create_dir(crates_dir.join("bar"))?;
        create_cargo_toml(&crates_dir.join("bar"), "[package]\nname = \"bar\"\n");

        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        assert_eq!(detected.len(), 3); // root + 2 glob matches
        Ok(())
    }

    #[test]
    fn test_detect_workspace_exclude() -> Result<()> {
        let tmp = TempDir::new()?;
        create_cargo_toml(
            tmp.path(),
            "[workspace]\nmembers = [\"crate-a\", \"crate-b\"]\nexclude = [\"crate-b\"]\n",
        );
        fs::create_dir(tmp.path().join("crate-a"))?;
        create_cargo_toml(
            &tmp.path().join("crate-a"),
            "[package]\nname = \"crate-a\"\n",
        );
        fs::create_dir(tmp.path().join("crate-b"))?;
        create_cargo_toml(
            &tmp.path().join("crate-b"),
            "[package]\nname = \"crate-b\"\n",
        );

        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        assert_eq!(detected.len(), 2); // root + crate-a (crate-b excluded)
        Ok(())
    }

    #[test]
    fn test_auto_discover_members() -> Result<()> {
        let tmp = TempDir::new()?;
        // Workspace section without members field
        create_cargo_toml(tmp.path(), "[workspace]\nresolver = \"2\"\n");

        // Immediate subdirectory
        fs::create_dir(tmp.path().join("core"))?;
        create_cargo_toml(&tmp.path().join("core"), "[package]\nname = \"core\"\n");

        // Nested subdirectory (e.g., crates/clients/desktop)
        let nested = tmp.path().join("crates").join("clients").join("desktop");
        fs::create_dir_all(&nested)?;
        create_cargo_toml(&nested, "[package]\nname = \"desktop\"\n");

        // Should skip target/ and hidden dirs
        let target_dir = tmp.path().join("target").join("debug");
        fs::create_dir_all(&target_dir)?;
        create_cargo_toml(&target_dir, "[package]\nname = \"fake\"\n");
        let hidden = tmp.path().join(".hidden");
        fs::create_dir(&hidden)?;
        create_cargo_toml(&hidden, "[package]\nname = \"hidden\"\n");

        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        // root + core + desktop (target/ and .hidden/ skipped)
        assert_eq!(detected.len(), 3, "detected: {:?}", detected.iter().map(|d| &d.path).collect::<Vec<_>>());
        Ok(())
    }
}
