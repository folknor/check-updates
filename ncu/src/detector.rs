use anyhow::Result;
use std::path::PathBuf;

/// Detected package.json file
#[derive(Debug, Clone)]
pub struct DetectedFile {
    pub path: PathBuf,
}

/// Detects package.json files in a project, including workspace members
pub struct ProjectDetector {
    project_path: PathBuf,
}

impl ProjectDetector {
    pub fn new(project_path: PathBuf) -> Self {
        Self { project_path }
    }

    /// Detect all package.json files in the project
    pub fn detect(&self) -> Result<Vec<DetectedFile>> {
        let mut detected = Vec::new();

        let package_json = self.project_path.join("package.json");
        if !package_json.exists() {
            return Ok(detected);
        }

        detected.push(DetectedFile {
            path: package_json.clone(),
        });

        // Check for workspace packages (npm/yarn/pnpm workspaces)
        if let Ok(content) = std::fs::read_to_string(&package_json) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(workspaces) = self.get_workspaces(&parsed) {
                    for pattern in workspaces {
                        let member_jsons = self.expand_workspace_pattern(&pattern)?;
                        for path in member_jsons {
                            if path != package_json {
                                detected.push(DetectedFile { path });
                            }
                        }
                    }
                }
            }
        }

        Ok(detected)
    }

    /// Extract workspace patterns from package.json
    fn get_workspaces(&self, parsed: &serde_json::Value) -> Option<Vec<String>> {
        // npm/yarn format: "workspaces": ["packages/*"]
        if let Some(workspaces) = parsed.get("workspaces") {
            // Direct array format
            if let Some(arr) = workspaces.as_array() {
                return Some(
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect(),
                );
            }
            // Yarn format: { "packages": ["packages/*"] }
            if let Some(packages) = workspaces.get("packages").and_then(|v| v.as_array()) {
                return Some(
                    packages
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect(),
                );
            }
        }
        None
    }

    /// Expand a workspace pattern (may contain globs)
    fn expand_workspace_pattern(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();
        let full_pattern = self.project_path.join(pattern).join("package.json");
        let pattern_str = full_pattern.to_string_lossy();

        if let Ok(paths) = glob::glob(&pattern_str) {
            for entry in paths.flatten() {
                if entry.exists() {
                    results.push(entry);
                }
            }
        }

        Ok(results)
    }

    /// Check if a lock file exists and return which type
    pub fn detect_lockfile(&self) -> Option<LockfileType> {
        if self.project_path.join("package-lock.json").exists() {
            Some(LockfileType::Npm)
        } else if self.project_path.join("pnpm-lock.yaml").exists() {
            Some(LockfileType::Pnpm)
        } else if self.project_path.join("yarn.lock").exists() {
            Some(LockfileType::Yarn)
        } else if self.project_path.join("bun.lockb").exists() {
            Some(LockfileType::Bun)
        } else {
            None
        }
    }

    pub fn lockfile_path(&self, lockfile_type: LockfileType) -> PathBuf {
        match lockfile_type {
            LockfileType::Npm => self.project_path.join("package-lock.json"),
            LockfileType::Pnpm => self.project_path.join("pnpm-lock.yaml"),
            LockfileType::Yarn => self.project_path.join("yarn.lock"),
            LockfileType::Bun => self.project_path.join("bun.lockb"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockfileType {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}
