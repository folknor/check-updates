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
            // Get members array
            if let Some(members) = workspace.get("members").and_then(|v| v.as_array()) {
                for member in members {
                    if let Some(pattern) = member.as_str() {
                        // Expand glob patterns and find Cargo.toml files
                        let member_tomls = self.expand_workspace_member(pattern)?;
                        for path in member_tomls {
                            // Avoid duplicates (root might be listed as a member)
                            if path != cargo_toml {
                                detected.push(DetectedFile { path });
                            }
                        }
                    }
                }
            }
        }

        Ok(detected)
    }

    /// Expand a workspace member pattern (may contain globs like "crates/*")
    fn expand_workspace_member(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();

        if pattern.contains('*') {
            // Handle glob pattern
            let full_pattern = self.project_path.join(pattern).join("Cargo.toml");
            let pattern_str = full_pattern.to_string_lossy();

            // Use simple glob expansion
            if let Ok(paths) = glob::glob(&pattern_str) {
                for entry in paths.flatten() {
                    if entry.exists() {
                        results.push(entry);
                    }
                }
            } else {
                // Fallback: try without glob (maybe glob crate not available)
                // Just check if the literal path exists
                let literal_path = self.project_path.join(pattern).join("Cargo.toml");
                if literal_path.exists() {
                    results.push(literal_path);
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

    /// Check if Cargo.lock exists
    pub fn has_lockfile(&self) -> bool {
        self.project_path.join("Cargo.lock").exists()
    }

    /// Get path to Cargo.lock
    pub fn lockfile_path(&self) -> PathBuf {
        self.project_path.join("Cargo.lock")
    }
}
