use anyhow::Result;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Directory names that never contain a workspace member's own `package.json`,
/// only installed or vendored third-party copies of one.
const NEVER_A_MEMBER: &[&str] = &["node_modules", "bower_components", "jspm_packages"];

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

        // Identity of a dependency downstream is (source_file, section, name); the
        // file half of that key is only sound if every detected path appears once and
        // in one spelling. Overlapping workspace patterns ("packages/*" plus
        // "packages/**") otherwise yield the same manifest twice, and the root
        // manifest can arrive under a second spelling via a pattern such as ".".
        // Canonicalize and dedup here so the caller never sees a repeat.
        let mut seen = HashSet::new();
        seen.insert(normalize(&package_json));
        detected.push(DetectedFile {
            path: package_json.clone(),
        });

        // Check for workspace packages (npm/yarn/pnpm workspaces)
        if let Ok(content) = std::fs::read_to_string(&package_json)
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(workspaces) = self.get_workspaces(&parsed)
        {
            for pattern in workspaces {
                let member_jsons = self.expand_workspace_pattern(&pattern)?;
                for path in member_jsons {
                    if seen.insert(normalize(&path)) {
                        detected.push(DetectedFile { path });
                    }
                }
            }
        }

        Ok(detected)
    }

    /// True when `path` lies inside a directory that cannot hold a workspace member:
    /// an installed-package tree (`node_modules/` and friends) or a hidden directory.
    ///
    /// Only the components below `project_path` are examined, so running ncu on a
    /// project that itself happens to live under some `node_modules/` still works.
    fn is_excluded(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.project_path) else {
            return false;
        };

        relative.components().any(|component| {
            let Component::Normal(name) = component else {
                return false;
            };
            let Some(name) = name.to_str() else {
                return false;
            };
            if name == "package.json" {
                return false;
            }
            NEVER_A_MEMBER.contains(&name) || name.starts_with('.')
        })
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

    /// Expand a workspace pattern (may contain globs).
    ///
    /// `"workspaces": ["packages/**"]` is legal and common, and `**` descends into
    /// `packages/*/node_modules/`, where every installed transitive package carries a
    /// `package.json`. Without a guard ncu parses and registry-queries thousands of
    /// third-party manifests, and `-u` rewrites files inside `node_modules`. So every
    /// glob hit is filtered through `is_excluded` before it is accepted.
    ///
    /// This is the npm-side counterpart of ccu's b24f805 ("skip gitignored dirs during
    /// workspace auto-discovery"). ccu could use `ignore::WalkBuilder` because it walks
    /// the tree itself; a workspace pattern is a glob, not a walk, so the exclusion is
    /// spelled out by hand instead. The one thing that costs us is gitignore awareness:
    /// a gitignored directory that is not one of `NEVER_A_MEMBER` and is not hidden is
    /// still matched. Closing that gap needs `ignore = "0.4"` in `ncu/Cargo.toml` (ccu
    /// already depends on it), after which this should become a `WalkBuilder` walk whose
    /// results are matched against `glob::Pattern`.
    ///
    /// Only the workspace pattern itself is glob syntax. The project path was chosen by
    /// the user's filesystem, not written into `package.json`, so a directory literally
    /// named `a[b]` or `v*` must not be reinterpreted as a pattern; the prefix is
    /// escaped before the member pattern is spliced onto it. Same rule as ccu's
    /// `expand_workspace_member`.
    fn expand_workspace_pattern(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();
        let pattern_str = format!(
            "{}/{}",
            Self::escape_glob_prefix(&self.project_path),
            PathBuf::from(pattern)
                .join("package.json")
                .to_string_lossy()
        );

        if let Ok(paths) = glob::glob(&pattern_str) {
            for entry in paths.flatten() {
                if entry.is_file() && !self.is_excluded(&entry) {
                    results.push(entry);
                }
            }
        }

        results.sort();
        Ok(results)
    }

    /// Escape glob metacharacters in a path that is data, not pattern.
    ///
    /// `glob::Pattern::escape` wraps `*`, `?` and `[` in character classes.
    fn escape_glob_prefix(path: &Path) -> String {
        glob::Pattern::escape(&path.to_string_lossy())
    }

    /// Check if a lock file exists and return which type
    pub fn detect_lockfile(&self) -> Option<LockfileType> {
        if self.project_path.join("package-lock.json").exists() {
            Some(LockfileType::Npm)
        } else if self.project_path.join("pnpm-lock.yaml").exists() {
            Some(LockfileType::Pnpm)
        } else if self.project_path.join("yarn.lock").exists() {
            Some(LockfileType::Yarn)
        } else if self.project_path.join("bun.lock").exists()
            || self.project_path.join("bun.lockb").exists()
        {
            Some(LockfileType::Bun)
        } else {
            None
        }
    }

    /// Path of the lock file for `lockfile_type`. Bun has two: the text
    /// `bun.lock`, which the lock-file parser reads, is preferred over the
    /// binary `bun.lockb`, which it can only warn about.
    pub fn lockfile_path(&self, lockfile_type: LockfileType) -> PathBuf {
        match lockfile_type {
            LockfileType::Npm => self.project_path.join("package-lock.json"),
            LockfileType::Pnpm => self.project_path.join("pnpm-lock.yaml"),
            LockfileType::Yarn => self.project_path.join("yarn.lock"),
            LockfileType::Bun => {
                let text = self.project_path.join("bun.lock");
                if text.exists() {
                    text
                } else {
                    self.project_path.join("bun.lockb")
                }
            }
        }
    }
}

/// Canonical form of `path`, for identity comparisons only. Falls back to the
/// path as given when it cannot be canonicalized (broken symlink, races), which
/// is safe: two spellings that both fail to canonicalize simply stay distinct.
///
/// The canonical form is deliberately *not* what gets stored in `DetectedFile`.
/// `Dependency::source_file` is what the updater writes back to and what the
/// output shows the user, and that must stay the path the user pointed us at,
/// not a symlink-resolved `/proc`-style rewrite of it.
fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockfileType {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_package_json(dir: &Path, body: &str) -> Result<()> {
        fs::create_dir_all(dir)?;
        fs::write(dir.join("package.json"), body)?;
        Ok(())
    }

    #[test]
    fn recursive_workspace_glob_skips_node_modules() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path();

        write_package_json(root, r#"{"name":"root","workspaces":["packages/**"]}"#)?;
        write_package_json(&root.join("packages/a"), r#"{"name":"a"}"#)?;
        write_package_json(&root.join("packages/b"), r#"{"name":"b"}"#)?;
        // Installed third-party copies that "packages/**" also matches.
        write_package_json(
            &root.join("packages/a/node_modules/lodash"),
            r#"{"name":"lodash"}"#,
        )?;
        write_package_json(
            &root.join("packages/a/node_modules/@scope/pkg"),
            r#"{"name":"@scope/pkg"}"#,
        )?;
        write_package_json(
            &root.join("packages/b/node_modules/left-pad"),
            r#"{"name":"left-pad"}"#,
        )?;

        let detected = ProjectDetector::new(root.to_path_buf()).detect()?;
        let paths: Vec<_> = detected.iter().map(|d| d.path.clone()).collect();

        assert_eq!(paths.len(), 3, "detected: {paths:?}");
        assert!(
            paths
                .iter()
                .all(|p| !p.components().any(|c| c.as_os_str() == "node_modules")),
            "detected: {paths:?}"
        );
        Ok(())
    }

    #[test]
    fn recursive_workspace_glob_skips_hidden_directories() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path();

        write_package_json(root, r#"{"name":"root","workspaces":["packages/**"]}"#)?;
        write_package_json(&root.join("packages/a"), r#"{"name":"a"}"#)?;
        write_package_json(&root.join("packages/.cache/stale"), r#"{"name":"stale"}"#)?;

        let detected = ProjectDetector::new(root.to_path_buf()).detect()?;
        assert_eq!(detected.len(), 2, "detected: {detected:?}");
        Ok(())
    }

    #[test]
    fn overlapping_patterns_yield_each_manifest_once() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path();

        write_package_json(
            root,
            r#"{"name":"root","workspaces":["packages/*","packages/**","."]}"#,
        )?;
        write_package_json(&root.join("packages/a"), r#"{"name":"a"}"#)?;
        write_package_json(&root.join("packages/b"), r#"{"name":"b"}"#)?;

        let detected = ProjectDetector::new(root.to_path_buf()).detect()?;
        let mut normalized: Vec<_> = detected.iter().map(|d| normalize(&d.path)).collect();
        let total = normalized.len();
        normalized.sort();
        normalized.dedup();

        assert_eq!(total, 3, "detected: {detected:?}");
        assert_eq!(normalized.len(), total, "duplicate manifests: {detected:?}");
        Ok(())
    }

    #[test]
    fn yarn_object_workspaces_are_expanded() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path();

        write_package_json(
            root,
            r#"{"name":"root","workspaces":{"packages":["packages/*"],"nohoist":["**/react"]}}"#,
        )?;
        write_package_json(&root.join("packages/a"), r#"{"name":"a"}"#)?;

        let detected = ProjectDetector::new(root.to_path_buf()).detect()?;
        assert_eq!(detected.len(), 2, "detected: {detected:?}");
        Ok(())
    }

    #[test]
    fn project_inside_node_modules_still_detects_its_own_members() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path().join("node_modules/some-pkg");

        write_package_json(&root, r#"{"name":"some-pkg","workspaces":["packages/*"]}"#)?;
        write_package_json(&root.join("packages/a"), r#"{"name":"a"}"#)?;

        let detected = ProjectDetector::new(root.clone()).detect()?;
        assert_eq!(detected.len(), 2, "detected: {detected:?}");
        Ok(())
    }

    // The project path is data, not pattern. A directory literally named `app[1]`
    // used to be spliced raw into the glob, where `[1]` became a character class
    // matching the single character `1` - so the directory never matched itself and
    // every workspace member vanished.
    #[test]
    fn metacharacters_in_the_project_path_are_literal() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path().join("app[1]");

        write_package_json(&root, r#"{"name":"root","workspaces":["packages/*"]}"#)?;
        write_package_json(&root.join("packages/a"), r#"{"name":"a"}"#)?;

        let detected = ProjectDetector::new(root.clone()).detect()?;
        assert_eq!(detected.len(), 2, "detected: {detected:?}");
        Ok(())
    }

    // `*` and `?` in the project path are the same problem with a subtler symptom:
    // they match, but they also match sibling directories the user never pointed at.
    #[test]
    fn a_star_in_the_project_path_does_not_match_siblings() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path().join("v*");

        write_package_json(&root, r#"{"name":"root","workspaces":["packages/*"]}"#)?;
        write_package_json(&root.join("packages/a"), r#"{"name":"a"}"#)?;
        // A sibling that an unescaped `v*` prefix would sweep in.
        write_package_json(
            &tmp.path().join("v2/packages/intruder"),
            r#"{"name":"intruder"}"#,
        )?;

        let detected = ProjectDetector::new(root.clone()).detect()?;
        let paths: Vec<_> = detected.iter().map(|d| d.path.clone()).collect();
        assert_eq!(paths.len(), 2, "detected: {paths:?}");
        assert!(
            !paths.iter().any(|p| p.to_string_lossy().contains("v2")),
            "detected: {paths:?}"
        );
        Ok(())
    }
}
