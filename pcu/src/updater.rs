use crate::detector::PackageManager;
use anyhow::{Context, Result};
use check_updates_core::{DependencyCheck, UpdateSeverity, write_atomically};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Updates dependency files with new versions
pub struct FileUpdater;

impl Default for FileUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl FileUpdater {
    pub fn new() -> Self {
        Self
    }

    /// Apply updates to dependency files based on severity filter
    /// - include_minor: false = patch only, true = patch + minor
    /// - force: true = all severities AND use absolute latest version
    pub fn apply_updates(
        &self,
        checks: &[DependencyCheck],
        include_minor: bool,
        force: bool,
    ) -> Result<UpdateResult> {
        let mut modified_files = HashSet::new();
        let mut package_file_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut package_managers = HashSet::new();

        // Group checks by file, filtering by severity
        let mut file_updates: HashMap<PathBuf, Vec<(&DependencyCheck, String)>> = HashMap::new();

        for check in checks {
            // Determine which version spec to use
            let version_spec = if force {
                // Force mode: use absolute latest for all packages
                check.force_spec.as_ref()
            } else {
                // Normal mode: filter by severity and use target_spec
                match check.severity {
                    Some(UpdateSeverity::Patch) => check.target_spec.as_ref(),
                    Some(UpdateSeverity::Minor) if include_minor => check.target_spec.as_ref(),
                    _ => None, // Skip major updates and minor (if not included)
                }
            };

            if let Some(spec) = version_spec
                && spec.is_rewritable()
            {
                let new_version = spec.to_string();
                file_updates
                    .entry(check.dependency.source_file.clone())
                    .or_default()
                    .push((check, new_version));
            }
        }

        // Update each file. `update_file` reports whether the bytes on disk
        // actually changed; a check whose spec could not be located in its
        // source line leaves the file untouched and must not be counted as a
        // modification, so a no-op run leaves the file and its mtime alone.
        for (file_path, updates) in file_updates {
            let applied = self
                .update_file(&file_path, &updates)
                .with_context(|| format!("Failed to update file: {}", file_path.display()))?;

            if applied.is_empty() {
                continue;
            }

            // Only packages whose spec was really rewritten count towards the
            // "updated in multiple files" note.
            for name in applied {
                package_file_map
                    .entry(name)
                    .or_default()
                    .push(file_path.clone());
            }

            modified_files.insert(file_path.clone());

            // Detect package manager from file name
            if let Some(pm) = detect_package_manager(&file_path) {
                package_managers.insert(pm);
            }
        }

        // Find packages updated in multiple files
        let mut multi_file_packages: Vec<String> = package_file_map
            .iter()
            .filter_map(|(pkg, files)| {
                let unique_files: HashSet<_> = files.iter().collect();
                if unique_files.len() > 1 {
                    Some(pkg.clone())
                } else {
                    None
                }
            })
            .collect();
        multi_file_packages.sort();

        Ok(UpdateResult {
            modified_files,
            multi_file_packages,
            package_managers,
        })
    }

    /// Update a single file with the given dependency updates.
    ///
    /// Returns the names of the packages whose spec was actually rewritten. An
    /// empty result means nothing changed and nothing was written.
    fn update_file(
        &self,
        file_path: &Path,
        updates: &[(&DependencyCheck, String)],
    ) -> Result<Vec<String>> {
        // Read the entire file
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        // Keep each line's own terminator so a CRLF (or mixed, or
        // newline-at-EOF-less) file round-trips byte for byte on the lines we
        // do not touch.
        let mut lines: Vec<SourceLine> = content
            .split_inclusive('\n')
            .map(SourceLine::parse)
            .collect();

        // Sort updates by line number in descending order to avoid offset issues
        let mut sorted_updates: Vec<_> = updates.iter().collect();
        sorted_updates.sort_by_key(|x| std::cmp::Reverse(x.0.dependency.line_number));

        let mut applied: Vec<String> = Vec::new();

        // Apply each update
        for (check, new_version) in sorted_updates {
            // A parser that could not prove where the declaration lives hands
            // us `None`. There is nothing safe to do with that but leave the
            // file alone: guessing a line is how the wave-1 findings got
            // wrong-line rewrites.
            let Some(line_number) = check.dependency.line_number else {
                continue;
            };
            let line_idx = line_number.saturating_sub(1);

            if line_idx >= lines.len() {
                continue; // Skip if line number is out of bounds
            }

            let original_line = &lines[line_idx].text;

            let Some(updated_line) = self.replace_version_in_line(
                original_line,
                &check.dependency.name,
                &check.dependency.version_spec.to_string(),
                new_version,
                file_path,
            ) else {
                // Spec not found on the recorded line: report nothing rather
                // than claiming an update we did not make.
                continue;
            };

            if updated_line == *original_line {
                continue;
            }

            lines[line_idx].text = updated_line;
            applied.push(check.dependency.name.clone());
        }

        if applied.is_empty() {
            // No effective change: do not rewrite the file and disturb its mtime.
            return Ok(Vec::new());
        }

        let mut new_content = String::with_capacity(content.len());
        for line in &lines {
            new_content.push_str(&line.text);
            new_content.push_str(line.terminator);
        }

        write_atomically(file_path, new_content.as_bytes())
            .with_context(|| format!("Failed to write file: {}", file_path.display()))?;

        Ok(applied)
    }

    /// Replace version specification in a line.
    ///
    /// `None` means the spec could not be located on this line; the caller
    /// must then leave the line alone. There is deliberately no unanchored
    /// whole-line `str::replace` fallback: it rewrote trailing comments,
    /// environment markers and unrelated second occurrences of the same
    /// version string.
    fn replace_version_in_line(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
        file_path: &Path,
    ) -> Option<String> {
        let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Determine file type and use appropriate replacement strategy
        if file_name == "pyproject.toml" {
            self.replace_in_pyproject(line, package_name, old_spec, new_spec)
        } else if file_name.starts_with("environment.")
            && (file_name.ends_with(".yml") || file_name.ends_with(".yaml"))
        {
            self.replace_in_conda(line, package_name, old_spec, new_spec)
        } else {
            // requirements.txt style, and the default for anything unknown
            self.replace_in_requirements(line, package_name, old_spec, new_spec)
        }
    }

    /// Replace version in requirements.txt format
    fn replace_in_requirements(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
    ) -> Option<String> {
        // Format: package==1.0.0 or package>=1.0.0,<2.0.0 or package[extras]==1.0.0
        let (body, comment) = split_comment(line);
        let new_body = replace_spec_after_name(body, package_name, old_spec, new_spec)?;
        Some(format!("{new_body}{comment}"))
    }

    /// Replace version in pyproject.toml format
    fn replace_in_pyproject(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
    ) -> Option<String> {
        let (body, comment) = split_comment(line);

        // PEP 621 / PEP 508 string form: "requests>=2.28.0",
        if let Some(new_body) = replace_spec_after_name(body, package_name, old_spec, new_spec) {
            return Some(format!("{new_body}{comment}"));
        }

        // Poetry key form: requests = "^2.28.0" or requests = {version = "^2.28.0", ...}
        let spec_start = name_anchor_end(body, package_name)?;
        let rest = &body[spec_start..];
        if !rest.trim_start().starts_with('=') {
            return None;
        }
        for quote in ['"', '\''] {
            let needle = format!("{quote}{old_spec}{quote}");
            if let Some(at) = rest.find(&needle) {
                let replacement = format!("{quote}{new_spec}{quote}");
                let new_body = format!(
                    "{}{}{}{}",
                    &body[..spec_start],
                    &rest[..at],
                    replacement,
                    &rest[at + needle.len()..]
                );
                return Some(format!("{new_body}{comment}"));
            }
        }

        None
    }

    /// Replace version in conda environment.yml format
    fn replace_in_conda(
        &self,
        line: &str,
        package_name: &str,
        old_spec: &str,
        new_spec: &str,
    ) -> Option<String> {
        // Format: - package==1.0.0 or - package=1.0.0
        let (body, comment) = split_comment(line);

        if let Some(new_body) = replace_spec_after_name(body, package_name, old_spec, new_spec) {
            return Some(format!("{new_body}{comment}"));
        }

        // Conda uses a single `=` where pip uses `==`
        let conda_old_spec = old_spec.replace("==", "=");
        let conda_new_spec = new_spec.replace("==", "=");
        if conda_old_spec == old_spec {
            return None;
        }
        let new_body =
            replace_spec_after_name(body, package_name, &conda_old_spec, &conda_new_spec)?;
        Some(format!("{new_body}{comment}"))
    }
}

/// One physical line plus the terminator it carried in the source file.
struct SourceLine {
    text: String,
    terminator: &'static str,
}

impl SourceLine {
    fn parse(chunk: &str) -> Self {
        if let Some(rest) = chunk.strip_suffix("\r\n") {
            Self {
                text: rest.to_string(),
                terminator: "\r\n",
            }
        } else if let Some(rest) = chunk.strip_suffix('\n') {
            Self {
                text: rest.to_string(),
                terminator: "\n",
            }
        } else {
            Self {
                text: chunk.to_string(),
                terminator: "",
            }
        }
    }
}

/// Split a line into its payload and its trailing comment (including the
/// whitespace that preceded the `#`). A `#` only starts a comment at the start
/// of the line or after whitespace, which keeps `...#egg=name` URL fragments
/// and `#`-bearing markers intact.
fn split_comment(line: &str) -> (&str, &str) {
    let bytes = line.as_bytes();
    for (idx, byte) in bytes.iter().enumerate() {
        if *byte != b'#' {
            continue;
        }
        if idx == 0 || bytes[idx - 1].is_ascii_whitespace() {
            let cut = line[..idx].trim_end_matches([' ', '\t']).len();
            return (&line[..cut], &line[cut..]);
        }
    }
    (line, "")
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// Find the byte offset just past an occurrence of `name` (and any `[extras]`
/// suffix) that is a whole token - not a substring of a longer package name.
fn name_anchor_end(body: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    // `to_ascii_lowercase` preserves byte length, so offsets stay valid.
    let haystack = body.to_ascii_lowercase();
    let needle = name.to_ascii_lowercase();
    let mut from = 0;

    while let Some(rel) = haystack[from..].find(&needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = !body[..start].chars().next_back().is_some_and(is_name_char);
        let after = &body[end..];
        let after_ok = !after.chars().next().is_some_and(is_name_char);

        if before_ok && after_ok {
            if let Some(close) = after.strip_prefix('[').and_then(|r| r.find(']')) {
                return Some(end + close + 2);
            }
            return Some(end);
        }
        from = end;
    }

    None
}

/// Replace `old_spec` with `new_spec` exactly where it sits directly after the
/// package name (optionally separated by whitespace). Returns `None` when the
/// name or the spec is not where it was claimed to be.
fn replace_spec_after_name(
    body: &str,
    name: &str,
    old_spec: &str,
    new_spec: &str,
) -> Option<String> {
    if old_spec.is_empty() {
        return None;
    }
    let pos = name_anchor_end(body, name)?;
    let rest = &body[pos..];
    let gap = rest.len() - rest.trim_start().len();
    let after_gap = &rest[gap..];

    if !after_gap.starts_with(old_spec) {
        return None;
    }

    Some(format!(
        "{}{}{}{}",
        &body[..pos],
        &rest[..gap],
        new_spec,
        &after_gap[old_spec.len()..]
    ))
}

/// Detect package manager from file path
fn detect_package_manager(path: &Path) -> Option<PackageManager> {
    let file_name = path.file_name()?.to_str()?;

    if file_name.starts_with("requirements") {
        Some(PackageManager::Pip)
    } else if file_name == "pyproject.toml" {
        // A pyproject.toml alone does not say which manager owns the project, and
        // telling a Poetry or PDM user to run `uv lock` is actively wrong advice.
        // Read the manifest's tool tables and adjacent lock files instead; fall back
        // to uv (the most common) only when the file cannot be read at all.
        Some(crate::detector::classify_pyproject(path).unwrap_or(PackageManager::Uv))
    } else if file_name.starts_with("environment.")
        && (file_name.ends_with(".yml") || file_name.ends_with(".yaml"))
    {
        Some(PackageManager::Conda)
    } else if file_name == "uv.lock" {
        Some(PackageManager::Uv)
    } else if file_name == "poetry.lock" {
        Some(PackageManager::Poetry)
    } else if file_name == "pdm.lock" {
        Some(PackageManager::Pdm)
    } else {
        None
    }
}

/// Result of applying updates
#[derive(Debug)]
pub struct UpdateResult {
    /// Files that were modified
    pub modified_files: HashSet<PathBuf>,
    /// Packages that were updated in multiple files
    pub multi_file_packages: Vec<String>,
    /// Package managers detected (for sync command suggestions)
    pub package_managers: HashSet<PackageManager>,
}

impl UpdateResult {
    /// Print post-update messages
    pub fn print_summary(&self) {
        if self.multi_file_packages.is_empty() && self.package_managers.is_empty() {
            return;
        }

        println!();

        if !self.multi_file_packages.is_empty() {
            println!(
                "Note: The following packages were updated in multiple files: {}",
                self.multi_file_packages.join(", ")
            );
        }

        for pm in &self.package_managers {
            let cmd = match pm {
                PackageManager::Pip => "pip install -r requirements.txt",
                PackageManager::Uv => "uv lock",
                PackageManager::Poetry => "poetry lock",
                PackageManager::Pdm => "pdm lock",
                PackageManager::Conda => "conda env update",
            };
            println!("Run {cmd} to sync dependencies");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_replace_in_requirements() {
        let updater = FileUpdater::new();

        // Test basic pinned version
        let result = updater
            .replace_in_requirements("requests==2.28.0", "requests", "==2.28.0", "==2.32.3")
            .unwrap();
        assert_eq!(result, "requests==2.32.3");

        // Test range version
        let result = updater
            .replace_in_requirements(
                "numpy>=1.24.0,<2.0.0",
                "numpy",
                ">=1.24.0,<2.0.0",
                ">=1.26.0,<2.0.0",
            )
            .unwrap();
        assert_eq!(result, "numpy>=1.26.0,<2.0.0");

        // Test with extras
        let result = updater
            .replace_in_requirements(
                "requests[security]==2.28.0",
                "requests",
                "==2.28.0",
                "==2.32.3",
            )
            .unwrap();
        assert_eq!(result, "requests[security]==2.32.3");
    }

    #[test]
    fn test_replace_in_pyproject() {
        let updater = FileUpdater::new();

        // Test with double quotes
        let result = updater
            .replace_in_pyproject("requests = \"^2.28.0\"", "requests", "^2.28.0", "^2.32.3")
            .unwrap();
        assert_eq!(result, "requests = \"^2.32.3\"");

        // Test with single quotes
        let result = updater
            .replace_in_pyproject("numpy = '^1.24.0'", "numpy", "^1.24.0", "^1.26.0")
            .unwrap();
        assert_eq!(result, "numpy = '^1.26.0'");
    }

    #[test]
    fn test_replace_in_conda() {
        let updater = FileUpdater::new();

        // Test with == operator
        let result = updater
            .replace_in_conda("  - numpy==1.24.0", "numpy", "==1.24.0", "==1.26.0")
            .unwrap();
        assert_eq!(result, "  - numpy==1.26.0");

        // Test with single = operator
        let result = updater
            .replace_in_conda("  - requests=2.28.0", "requests", "==2.28.0", "==2.32.3")
            .unwrap();
        assert_eq!(result, "  - requests=2.32.3");
    }

    #[test]
    fn test_replace_preserves_trailing_comment() {
        let updater = FileUpdater::new();

        // The old unanchored fallback rewrote the trailing comment too.
        let result = updater
            .replace_in_requirements(
                "flask==2.0.3  # pin matches 2.0.3 in docs",
                "flask",
                "==2.0.3",
                "==2.3.3",
            )
            .unwrap();
        assert_eq!(result, "flask==2.3.3  # pin matches 2.0.3 in docs");
    }

    #[test]
    fn test_replace_does_not_touch_environment_marker() {
        let updater = FileUpdater::new();

        let result = updater
            .replace_in_requirements(
                "backports==1.0.0 ; python_version < \"1.0.0\"",
                "backports",
                "==1.0.0",
                "==1.0.1",
            )
            .unwrap();
        assert_eq!(result, "backports==1.0.1 ; python_version < \"1.0.0\"");
    }

    #[test]
    fn test_replace_requires_the_named_package() {
        let updater = FileUpdater::new();

        // `requests` must not anchor on `requests-oauthlib`.
        assert!(
            updater
                .replace_in_requirements(
                    "requests-oauthlib==2.28.0",
                    "requests",
                    "==2.28.0",
                    "==2.32.3",
                )
                .is_none()
        );

        // A spec the parser normalised differently is reported as not found
        // rather than silently skipped while claiming success.
        assert!(updater.replace_in_requirements(
            "numpy >= 1.0, < 2.0",
            "numpy",
            ">=1.0,<2.0",
            ">=1.5,<2.0",
        ).is_none());
    }

    #[test]
    fn test_replace_in_pyproject_pep621_string() {
        let updater = FileUpdater::new();

        let result = updater
            .replace_in_pyproject(
                "    \"requests>=2.28.0\",",
                "requests",
                ">=2.28.0",
                ">=2.32.3",
            )
            .unwrap();
        assert_eq!(result, "    \"requests>=2.32.3\",");
    }

    #[test]
    fn test_replace_in_pyproject_inline_table() {
        let updater = FileUpdater::new();

        let result = updater
            .replace_in_pyproject(
                "requests = { version = \"^2.28.0\", optional = true }",
                "requests",
                "^2.28.0",
                "^2.32.3",
            )
            .unwrap();
        assert_eq!(
            result,
            "requests = { version = \"^2.32.3\", optional = true }"
        );
    }

    #[test]
    fn test_crlf_line_endings_survive() -> Result<()> {
        use crate::parsers::Dependency;
        use check_updates_core::{Version, VersionSpec};

        let mut file = NamedTempFile::new()?;
        file.write_all(b"# deps\r\nrequests==2.28.0\r\nnumpy==1.24.0\r\n")?;
        file.flush()?;
        let temp_path = file.path().to_path_buf();

        let check = DependencyCheck {
            dependency: Dependency {
                name: "requests".to_string(),
                version_spec: VersionSpec::Pinned(Version::new(2, 28, 0)),
                source_file: temp_path.clone(),
                line_number: Some(2),
                original_line: "requests==2.28.0".to_string(),
                manifest_key: None,
                section: None,
            },
            installed: Some(Version::new(2, 28, 0)),
            in_range: Some(Version::new(2, 32, 3)),
            latest: Version::new(2, 32, 3),
            target: Some(Version::new(2, 32, 3)),
            target_spec: Some(VersionSpec::Pinned(Version::new(2, 32, 3))),
            severity: Some(UpdateSeverity::Minor),
            force_spec: Some(VersionSpec::Pinned(Version::new(2, 32, 3))),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        };

        let updater = FileUpdater::new();
        updater.update_file(&temp_path, &[(&check, "==2.32.3".to_string())])?;

        let content = fs::read_to_string(&temp_path)?;
        assert_eq!(content, "# deps\r\nrequests==2.32.3\r\nnumpy==1.24.0\r\n");

        Ok(())
    }

    #[test]
    fn test_no_match_reports_no_modified_file() -> Result<()> {
        use crate::parsers::Dependency;
        use check_updates_core::{Version, VersionSpec};

        let mut file = NamedTempFile::new()?;
        writeln!(file, "requests==2.28.0")?;
        file.flush()?;
        let temp_path = file.path().to_path_buf();
        let before = fs::read_to_string(&temp_path)?;

        // The parser could not locate the declaration.
        let check = DependencyCheck {
            dependency: Dependency {
                name: "requests".to_string(),
                version_spec: VersionSpec::Pinned(Version::new(2, 28, 0)),
                source_file: temp_path.clone(),
                line_number: None,
                original_line: "requests==2.28.0".to_string(),
                manifest_key: None,
                section: None,
            },
            installed: Some(Version::new(2, 28, 0)),
            in_range: Some(Version::new(2, 28, 1)),
            latest: Version::new(2, 28, 1),
            target: Some(Version::new(2, 28, 1)),
            target_spec: Some(VersionSpec::Pinned(Version::new(2, 28, 1))),
            severity: Some(UpdateSeverity::Patch),
            force_spec: Some(VersionSpec::Pinned(Version::new(2, 28, 1))),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        };

        let updater = FileUpdater::new();
        let result = updater.apply_updates(&[check], false, false)?;

        assert!(
            result.modified_files.is_empty(),
            "nothing was written, so nothing may be reported"
        );
        assert_eq!(fs::read_to_string(&temp_path)?, before);

        Ok(())
    }

    #[test]
    fn test_detect_package_manager() {
        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/requirements.txt")),
            Some(PackageManager::Pip)
        );

        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/requirements-dev.txt")),
            Some(PackageManager::Pip)
        );

        // A pyproject.toml that cannot be read falls back to uv.
        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/pyproject.toml")),
            Some(PackageManager::Uv)
        );

        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/environment.yml")),
            Some(PackageManager::Conda)
        );

        assert_eq!(
            detect_package_manager(&PathBuf::from("/path/to/poetry.lock")),
            Some(PackageManager::Poetry)
        );
    }

    #[test]
    fn test_detect_package_manager_reads_pyproject_tool_table() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let poetry = tmp.path().join("pyproject.toml");
        fs::write(
            &poetry,
            "[tool.poetry.dependencies]\nrequests = \"^2.28\"\n",
        )?;
        assert_eq!(
            detect_package_manager(&poetry),
            Some(PackageManager::Poetry)
        );

        fs::write(&poetry, "[tool.pdm.dev-dependencies]\ntest = []\n")?;
        assert_eq!(detect_package_manager(&poetry), Some(PackageManager::Pdm));

        fs::write(&poetry, "[project]\nname = \"x\"\n")?;
        assert_eq!(detect_package_manager(&poetry), Some(PackageManager::Uv));

        Ok(())
    }

    #[test]
    fn test_update_file_integration() -> Result<()> {
        use crate::parsers::Dependency;
        use check_updates_core::{Version, VersionSpec};

        let updater = FileUpdater::new();

        // Create a temporary requirements.txt file
        let mut temp_file = NamedTempFile::new()?;
        writeln!(temp_file, "requests==2.28.0")?;
        writeln!(temp_file, "numpy>=1.24.0,<2.0.0")?;
        writeln!(temp_file, "flask==2.0.3")?;
        temp_file.flush()?;

        let temp_path = temp_file.path().to_path_buf();

        // Create mock dependency checks
        let check1 = DependencyCheck {
            dependency: Dependency {
                name: "requests".to_string(),
                version_spec: VersionSpec::Pinned(Version::new(2, 28, 0)),
                source_file: temp_path.clone(),
                line_number: Some(1),
                original_line: "requests==2.28.0".to_string(),
                manifest_key: None,
                section: None,
            },
            installed: Some(Version::new(2, 28, 0)),
            in_range: Some(Version::new(2, 32, 3)),
            latest: Version::new(2, 32, 3),
            target: Some(Version::new(2, 32, 3)),
            target_spec: Some(VersionSpec::Pinned(Version::new(2, 32, 3))),
            severity: Some(UpdateSeverity::Minor),
            force_spec: Some(VersionSpec::Pinned(Version::new(2, 32, 3))),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        };
        let check2 = DependencyCheck {
            dependency: Dependency {
                name: "flask".to_string(),
                version_spec: VersionSpec::Pinned(Version::new(2, 0, 3)),
                source_file: temp_path.clone(),
                line_number: Some(3),
                original_line: "flask==2.0.3".to_string(),
                manifest_key: None,
                section: None,
            },
            installed: Some(Version::new(2, 0, 3)),
            in_range: Some(Version::new(2, 3, 3)),
            latest: Version::new(2, 3, 3),
            target: Some(Version::new(2, 3, 3)),
            target_spec: Some(VersionSpec::Pinned(Version::new(2, 3, 3))),
            severity: Some(UpdateSeverity::Minor),
            force_spec: Some(VersionSpec::Pinned(Version::new(2, 3, 3))),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        };

        // Create updates with version strings
        let updates: Vec<(&DependencyCheck, String)> = vec![
            (&check1, "==2.32.3".to_string()),
            (&check2, "==2.3.3".to_string()),
        ];

        // Apply updates
        updater.update_file(&temp_path, &updates)?;

        // Read the updated file
        let updated_content = fs::read_to_string(&temp_path)?;
        let lines: Vec<&str> = updated_content.lines().collect();

        // Verify updates
        assert_eq!(lines[0], "requests==2.32.3");
        assert_eq!(lines[1], "numpy>=1.24.0,<2.0.0"); // Unchanged
        assert_eq!(lines[2], "flask==2.3.3");

        Ok(())
    }

    #[test]
    fn test_update_patch_only() -> Result<()> {
        use crate::parsers::Dependency;
        use check_updates_core::{Version, VersionSpec};

        let mut file = NamedTempFile::new()?;
        writeln!(file, "serde==1.0.0")?;
        writeln!(file, "tokio==1.0.0")?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            DependencyCheck {
                dependency: Dependency {
                    name: "serde".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: Some(1),
                    original_line: "serde==1.0.0".to_string(),
                    manifest_key: None,
                    section: None,
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 0, 200)),
                latest: Version::new(1, 0, 200),
                target: Some(Version::new(1, 0, 200)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
                severity: Some(UpdateSeverity::Patch),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
                installed_released_at: None,
                target_released_at: None,
                latest_released_at: None,
            },
            DependencyCheck {
                dependency: Dependency {
                    name: "tokio".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: Some(2),
                    original_line: "tokio==1.0.0".to_string(),
                    manifest_key: None,
                    section: None,
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 5, 0)),
                latest: Version::new(1, 5, 0),
                target: Some(Version::new(1, 5, 0)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
                severity: Some(UpdateSeverity::Minor),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
                installed_released_at: None,
                target_released_at: None,
                latest_released_at: None,
            },
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, false, false)?; // patch only

        let content = fs::read_to_string(&temp_path)?;
        assert!(
            content.contains("==1.0.200"),
            "serde should be updated: {content}"
        );
        assert!(
            !content.contains("==1.5.0"),
            "tokio should NOT be updated: {content}"
        );

        Ok(())
    }

    #[test]
    fn test_update_patch_and_minor() -> Result<()> {
        use crate::parsers::Dependency;
        use check_updates_core::{Version, VersionSpec};

        let mut file = NamedTempFile::new()?;
        writeln!(file, "serde==1.0.0")?;
        writeln!(file, "tokio==1.0.0")?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            DependencyCheck {
                dependency: Dependency {
                    name: "serde".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: Some(1),
                    original_line: "serde==1.0.0".to_string(),
                    manifest_key: None,
                    section: None,
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 0, 200)),
                latest: Version::new(1, 0, 200),
                target: Some(Version::new(1, 0, 200)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
                severity: Some(UpdateSeverity::Patch),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 0, 200))),
                installed_released_at: None,
                target_released_at: None,
                latest_released_at: None,
            },
            DependencyCheck {
                dependency: Dependency {
                    name: "tokio".to_string(),
                    version_spec: VersionSpec::Pinned(Version::new(1, 0, 0)),
                    source_file: temp_path.clone(),
                    line_number: Some(2),
                    original_line: "tokio==1.0.0".to_string(),
                    manifest_key: None,
                    section: None,
                },
                installed: Some(Version::new(1, 0, 0)),
                in_range: Some(Version::new(1, 5, 0)),
                latest: Version::new(1, 5, 0),
                target: Some(Version::new(1, 5, 0)),
                target_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
                severity: Some(UpdateSeverity::Minor),
                force_spec: Some(VersionSpec::Pinned(Version::new(1, 5, 0))),
                installed_released_at: None,
                target_released_at: None,
                latest_released_at: None,
            },
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, true, false)?; // patch + minor

        let content = fs::read_to_string(&temp_path)?;
        assert!(
            content.contains("==1.0.200"),
            "serde should be updated: {content}"
        );
        assert!(
            content.contains("==1.5.0"),
            "tokio should be updated: {content}"
        );

        Ok(())
    }
}
