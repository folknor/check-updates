use anyhow::{Context, Result};
use ignore::WalkBuilder;
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
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
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
    ///
    /// Deliberately *not* gitignore-filtered, unlike `auto_discover_members`. The
    /// asymmetry is intended: auto-discovery guesses at membership and must not adopt
    /// unrelated vendored checkouts (commit b24f805), whereas an explicit `members` entry
    /// is the user's declaration of what belongs to the workspace. Cargo itself expands
    /// member globs with no ignore awareness, and a gitignored-but-listed member is a real
    /// member whose dependencies we must report. Do not "fix" this to match
    /// auto-discovery.
    fn expand_workspace_member(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();

        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            // Handle glob pattern. Only the member pattern itself is glob syntax; the
            // project path was chosen by the user's filesystem, not by the manifest, so a
            // directory literally named `a[b]` or `v*` must not be reinterpreted as a
            // pattern. Escape the prefix before splicing the member pattern onto it.
            let pattern_str = format!(
                "{}/{}",
                Self::escape_glob_prefix(&self.project_path),
                PathBuf::from(pattern).join("Cargo.toml").to_string_lossy()
            );

            let paths = glob::glob(&pattern_str)
                .with_context(|| format!("Invalid workspace member glob pattern: {pattern}"))?;

            for entry in paths {
                let path = entry
                    .with_context(|| format!("Error reading glob match for pattern: {pattern}"))?;
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
    ///
    /// Honors `.gitignore` (and hidden-file) rules so we don't descend into gitignored
    /// directories such as vendored checkouts or scratch dirs that carry their own,
    /// unrelated `Cargo.toml` files. Also skips `target/`, which is not gitignored in
    /// every project.
    fn auto_discover_members(&self) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();

        let walker = WalkBuilder::new(&self.project_path)
            .filter_entry(|entry| entry.file_name().to_str() != Some("target"))
            .build();

        for entry in walker {
            let entry = entry.context("Failed to walk project directory")?;
            if entry.file_name().to_str() != Some("Cargo.toml") {
                continue;
            }
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                results.push(entry.into_path());
            }
        }

        results.sort();
        Ok(results)
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
                // Glob-based exclude. Same escaping rule as expand_workspace_member: the
                // project path is data, the exclude pattern is syntax.
                let full_pattern = format!(
                    "{}/{}",
                    Self::escape_glob_prefix(&self.project_path),
                    PathBuf::from(*pattern).join("Cargo.toml").to_string_lossy()
                );
                if let Ok(glob_pattern) = glob::Pattern::new(&full_pattern)
                    && glob_pattern.matches_path(path)
                {
                    return true;
                }
            } else {
                // Literal exclude. Cargo excludes the whole subtree rooted at the pattern,
                // not just the directory itself: `exclude = ["vendor"]` also excludes
                // `vendor/foo`. Compare on path components so `vendored` is not treated as
                // a match for `vendor`.
                if Self::path_is_within(&relative, pattern) {
                    return true;
                }
            }
        }

        false
    }

    /// Escape glob metacharacters in a path that is data, not pattern.
    ///
    /// `glob::Pattern::escape` wraps `*`, `?` and `[` in character classes.
    fn escape_glob_prefix(path: &std::path::Path) -> String {
        glob::Pattern::escape(&path.to_string_lossy())
    }

    /// True when `relative` is `pattern` itself or lives underneath it, comparing whole
    /// path components (so `vendored` does not match `vendor`).
    fn path_is_within(relative: &str, pattern: &str) -> bool {
        let rel = std::path::Path::new(relative);
        let pat = std::path::Path::new(pattern);
        rel.strip_prefix(pat).is_ok()
    }

    /// True when `manifest` parses as TOML and carries a `[workspace]` table.
    fn declares_workspace(manifest: &std::path::Path) -> bool {
        let Ok(content) = fs::read_to_string(manifest) else {
            return false;
        };
        let Ok(parsed) = toml::from_str::<Value>(&content) else {
            return false;
        };
        parsed.get("workspace").and_then(|v| v.as_table()).is_some()
    }

    /// Resolve the Cargo workspace root that governs `project_path`.
    ///
    /// Cargo resolves `[workspace.dependencies]` and `Cargo.lock` from the workspace root,
    /// not from the directory the command was run in. Running against a member crate
    /// (`ccu ccu/`) must therefore still find the root manifest, otherwise every
    /// `.workspace = true` dependency resolves to nothing and no installed versions are
    /// available to compare against.
    ///
    /// Resolution order, mirroring cargo:
    /// 1. an explicit `package.workspace = "<path>"` pointer in the local manifest,
    /// 2. the nearest ancestor manifest with a `[workspace]` table that actually claims
    ///    this directory as a member,
    /// 3. `project_path` itself, when nothing above applies.
    pub fn workspace_root(&self) -> PathBuf {
        let start =
            fs::canonicalize(&self.project_path).unwrap_or_else(|_| self.project_path.clone());
        let manifest = start.join("Cargo.toml");

        // The directory we were pointed at is itself a workspace root.
        if Self::declares_workspace(&manifest) {
            return self.project_path.clone();
        }

        // Explicit `package.workspace` pointer wins over the upward walk.
        if let Ok(content) = fs::read_to_string(&manifest)
            && let Ok(parsed) = toml::from_str::<Value>(&content)
            && let Some(rel) = parsed
                .get("package")
                .and_then(|p| p.get("workspace"))
                .and_then(|v| v.as_str())
        {
            let candidate = start.join(rel);
            let candidate = fs::canonicalize(&candidate).unwrap_or(candidate);
            if Self::declares_workspace(&candidate.join("Cargo.toml")) {
                return candidate;
            }
        }

        // Walk upward for the nearest workspace root that claims us.
        for ancestor in start.ancestors().skip(1) {
            let candidate = ancestor.join("Cargo.toml");
            if !Self::declares_workspace(&candidate) {
                continue;
            }
            if Self::workspace_claims(ancestor, &start) {
                return ancestor.to_path_buf();
            }
        }

        self.project_path.clone()
    }

    /// True when the workspace rooted at `root` lists `member_dir` as a member and does
    /// not exclude it. A bare `[workspace]` with no `members` field auto-discovers, so any
    /// non-excluded descendant counts.
    fn workspace_claims(root: &std::path::Path, member_dir: &std::path::Path) -> bool {
        let root_detector = Self::new(root.to_path_buf());
        let Ok(detected) = root_detector.detect() else {
            return false;
        };
        let target = member_dir.join("Cargo.toml");
        detected.iter().any(|d| {
            d.path == target
                || fs::canonicalize(&d.path)
                    .is_ok_and(|p| fs::canonicalize(&target).is_ok_and(|t| p == t))
        })
    }

    /// Check if Cargo.lock exists
    ///
    /// Looked up at the workspace root: cargo keeps a single lockfile per workspace.
    pub fn has_lockfile(&self) -> bool {
        self.lockfile_path().exists()
    }

    /// Get path to Cargo.lock (at the workspace root, see `workspace_root`)
    pub fn lockfile_path(&self) -> PathBuf {
        self.workspace_root().join("Cargo.lock")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        create_cargo_toml(
            tmp.path(),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        );
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
    fn test_exclude_covers_subtree() -> Result<()> {
        let tmp = TempDir::new()?;
        create_cargo_toml(
            tmp.path(),
            "[workspace]\nmembers = [\"vendor/foo\", \"vendored\"]\nexclude = [\"vendor\"]\n",
        );
        let vendor_foo = tmp.path().join("vendor").join("foo");
        fs::create_dir_all(&vendor_foo)?;
        create_cargo_toml(&vendor_foo, "[package]\nname = \"foo\"\n");
        fs::create_dir(tmp.path().join("vendored"))?;
        create_cargo_toml(
            &tmp.path().join("vendored"),
            "[package]\nname = \"vendored\"\n",
        );

        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        // root + vendored; vendor/foo is inside the excluded subtree, `vendored` is not.
        assert_eq!(
            detected.len(),
            2,
            "detected: {:?}",
            detected.iter().map(|d| &d.path).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn test_glob_metacharacters_in_project_path_are_literal() -> Result<()> {
        let tmp = TempDir::new()?;
        // A project directory whose name contains glob syntax the user did not intend.
        let root = tmp.path().join("pro[ject]*");
        fs::create_dir(&root)?;
        create_cargo_toml(&root, "[workspace]\nmembers = [\"crates/*\"]\n");
        let crates = root.join("crates");
        fs::create_dir(&crates)?;
        fs::create_dir(crates.join("foo"))?;
        create_cargo_toml(&crates.join("foo"), "[package]\nname = \"foo\"\n");

        let detector = ProjectDetector::new(root);
        let detected = detector.detect()?;
        assert_eq!(
            detected.len(),
            2,
            "detected: {:?}",
            detected.iter().map(|d| &d.path).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn test_workspace_root_found_from_member() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = fs::canonicalize(tmp.path())?;
        create_cargo_toml(&root, "[workspace]\nmembers = [\"ccu\"]\n");
        fs::write(root.join("Cargo.lock"), "version = 3\n")?;
        let member = root.join("ccu");
        fs::create_dir(&member)?;
        create_cargo_toml(&member, "[package]\nname = \"ccu\"\n");

        let detector = ProjectDetector::new(member.clone());
        assert_eq!(detector.workspace_root(), root);
        assert!(detector.has_lockfile());
        assert_eq!(detector.lockfile_path(), root.join("Cargo.lock"));

        // Detection itself stays scoped to the member the user pointed at.
        assert_eq!(detector.detect()?.len(), 1);
        Ok(())
    }

    #[test]
    fn test_workspace_root_of_root_is_itself() -> Result<()> {
        let tmp = TempDir::new()?;
        create_cargo_toml(tmp.path(), "[workspace]\nmembers = [\"a\"]\n");
        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        assert_eq!(detector.workspace_root(), tmp.path());
        Ok(())
    }

    #[test]
    fn test_workspace_root_ignores_unrelated_parent_workspace() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = fs::canonicalize(tmp.path())?;
        // A parent workspace that does not claim `standalone`.
        create_cargo_toml(&root, "[workspace]\nmembers = [\"a\"]\n");
        fs::create_dir(root.join("a"))?;
        create_cargo_toml(&root.join("a"), "[package]\nname = \"a\"\n");

        let standalone = root.join("standalone");
        fs::create_dir(&standalone)?;
        create_cargo_toml(&standalone, "[package]\nname = \"standalone\"\n");

        let detector = ProjectDetector::new(standalone.clone());
        assert_eq!(detector.workspace_root(), standalone);
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
        assert_eq!(
            detected.len(),
            3,
            "detected: {:?}",
            detected.iter().map(|d| &d.path).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn test_auto_discover_skips_gitignored_dirs() -> Result<()> {
        let tmp = TempDir::new()?;
        // Mark the tree as a git repo so gitignore rules are applied, and ignore research/.
        fs::create_dir(tmp.path().join(".git"))?;
        fs::write(tmp.path().join(".gitignore"), "research\n")?;

        // Bare workspace root with no members field -> triggers auto-discovery.
        create_cargo_toml(tmp.path(), "[workspace]\nresolver = \"2\"\n");

        // A real, tracked member.
        fs::create_dir(tmp.path().join("core"))?;
        create_cargo_toml(&tmp.path().join("core"), "[package]\nname = \"core\"\n");

        // A gitignored directory holding unrelated crates (e.g. vendored checkouts).
        let research = tmp.path().join("research").join("bifrost");
        fs::create_dir_all(&research)?;
        create_cargo_toml(&research, "[package]\nname = \"bifrost\"\n");

        let detector = ProjectDetector::new(tmp.path().to_path_buf());
        let detected = detector.detect()?;
        // root + core only (research/ gitignored)
        assert_eq!(
            detected.len(),
            2,
            "detected: {:?}",
            detected.iter().map(|d| &d.path).collect::<Vec<_>>()
        );
        Ok(())
    }
}
