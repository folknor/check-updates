use anyhow::{Context, Result};
use check_updates_core::{Version, VersionSpec};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::parsers::pep508;

/// Lock files pcu can read, in the order it prefers them when a project
/// contains more than one.
///
/// `uv.lock`, `poetry.lock`, `pdm.lock` and `pylock.toml` are TOML with a
/// package array; `Pipfile.lock` is JSON; `conda-lock.yml` is YAML;
/// `requirements.lock` (rye, pip-compile) is a pinned requirements file.
const LOCK_FILES: [&str; 7] = [
    "uv.lock",
    "poetry.lock",
    "pdm.lock",
    "pylock.toml",
    "Pipfile.lock",
    "requirements.lock",
    "conda-lock.yml",
];

/// How deep below the project root `find_and_parse` looks for a lock file when
/// the root itself has none. Two levels covers the common `src/`, `backend/`,
/// `packages/<member>/` layouts without walking a whole virtualenv.
const MAX_SEARCH_DEPTH: usize = 2;

/// Directory names never worth descending into while looking for a lock file.
const SKIP_DIRS: [&str; 10] = [
    ".git",
    ".venv",
    "venv",
    "env",
    "node_modules",
    "site-packages",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    "target",
];

/// Parser for various lock files to get installed versions
pub struct LockfileParser;

/// Shared resolution rule for every lock-file format.
///
/// A Python lock file can record several resolved copies of the same
/// distribution: `uv.lock` and `poetry.lock` fork on environment markers
/// (`sys_platform`, `python_version`), so `numpy` can legitimately appear twice
/// with different versions. pcu reports one installed version per dependency,
/// so the copy it picks has to be the one that answers "which resolution does
/// this project actually get".
///
/// The rule, applied identically by every format here, mirrors
/// `ncu/src/parsers/lockfiles.rs`:
///
/// 1. If the lock file states the root project's own resolved copy, use it.
///    `uv.lock` names the version on a forked root dependency edge, and
///    `Pipfile.lock` records exactly one pin per distribution under
///    `default`/`develop`.
/// 2. Otherwise, if the lock file records the root project's declared range
///    (`uv.lock`'s `[package.metadata] requires-dist`), keep only the
///    candidates satisfying it and take the highest.
/// 3. Otherwise - `poetry.lock`, `pdm.lock` and `conda-lock.yml` do not say who
///    asked - take the highest of all candidates.
///
/// Steps 2 and 3 are fallbacks, not the rule. "Highest satisfying the range"
/// alone picks whichever fork happens to be newest rather than the one the root
/// resolved to. All three steps are deterministic, which the previous
/// last-`insert`-wins behaviour was not.
#[derive(Default)]
struct Candidates {
    /// Every resolved version seen for a distribution name.
    versions: HashMap<String, Vec<Version>>,
    /// The copy the root project resolved to, where the format states it.
    direct: HashMap<String, Version>,
    /// The root project's declared range, where the format records it.
    root_specs: HashMap<String, VersionSpec>,
    /// Version strings the lock file contained that could not be parsed.
    ///
    /// `Version::from_str` is strict, so a malformed or non-PEP-440 pin is an
    /// error rather than a silent zero. Dropping those quietly would make the
    /// distribution look uninstalled, and an uninstalled-looking dependency is
    /// compared against its declared spec instead of what is on disk.
    unparsed: Vec<String>,
}

impl Candidates {
    fn add(&mut self, name: &str, version_str: &str) {
        let name = pep508::normalize_name(name);
        match Version::from_str(version_str) {
            Ok(version) => self.versions.entry(name).or_default().push(version),
            Err(_) => self.unparsed.push(format!("{name}@{version_str}")),
        }
    }

    /// Record the copy the root project itself resolved to. Also counted as a
    /// candidate, so a distribution with only that copy still resolves.
    fn add_direct(&mut self, name: &str, version_str: &str) {
        self.add(name, version_str);
        if let Ok(version) = Version::from_str(version_str) {
            self.direct.insert(pep508::normalize_name(name), version);
        }
    }

    fn add_root_spec(&mut self, name: &str, spec_str: &str) {
        if let Ok(spec) = VersionSpec::parse(spec_str) {
            self.root_specs.insert(pep508::normalize_name(name), spec);
        }
    }

    fn resolve(self, path: &Path) -> HashMap<String, Version> {
        warn_unparsed(path, &self.unparsed);

        let mut resolved = HashMap::with_capacity(self.versions.len());
        for (name, mut candidates) in self.versions {
            if let Some(direct) = self.direct.get(&name) {
                resolved.insert(name, direct.clone());
                continue;
            }
            candidates.sort();
            let best = match self.root_specs.get(&name) {
                Some(spec) => candidates
                    .iter()
                    .rev()
                    .find(|v| spec.satisfies(v))
                    .or_else(|| candidates.last()),
                None => candidates.last(),
            };
            if let Some(best) = best {
                resolved.insert(name, best.clone());
            }
        }
        resolved
    }
}

/// Report lock entries whose version could not be read, on stderr so `--json`
/// on stdout stays valid. Silence here is indistinguishable from "not
/// installed", which changes what the tool reports as available.
fn warn_unparsed(path: &Path, unparsed: &[String]) {
    if unparsed.is_empty() {
        return;
    }
    let shown: Vec<&str> = unparsed.iter().take(3).map(String::as_str).collect();
    let more = unparsed.len().saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(" (and {more} more)")
    } else {
        String::new()
    };
    eprintln!(
        "warning: {}: {} entr{} had an unreadable version and were skipped: {}{}",
        path.display(),
        unparsed.len(),
        if unparsed.len() == 1 { "y" } else { "ies" },
        shown.join(", "),
        suffix
    );
}

/// A `[[package]]` entry as it appears in `uv.lock`, `poetry.lock` and
/// `pdm.lock`. The three formats share the `name`/`version` pair; everything
/// else is optional because each format fills in a different subset.
///
/// `source`, `dependencies` and `metadata` are kept as raw `toml::Value`
/// because their shapes disagree across formats - poetry writes
/// `dependencies` as a table of name -> constraint, pdm as an array of
/// requirement strings, uv as an array of `{ name, version }` tables - and a
/// typed field would make the *other* formats fail to deserialize entirely.
#[derive(Debug, Deserialize)]
struct TomlPackage {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    source: Option<toml::Value>,
    #[serde(default)]
    dependencies: Option<toml::Value>,
    #[serde(default)]
    metadata: Option<toml::Value>,
}

/// TOML lock files with a package array. `package` is uv/poetry/pdm;
/// `packages` is PEP 751 `pylock.toml`.
#[derive(Debug, Deserialize)]
struct TomlLockFile {
    #[serde(default)]
    package: Vec<TomlPackage>,
    #[serde(default)]
    packages: Vec<TomlPackage>,
}

impl LockfileParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse a lock file and return a map of package name -> installed version.
    ///
    /// Every name `can_parse` accepts has an arm here; the two must agree, or a
    /// caller that checked first still gets a hard error.
    pub fn parse(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid lock file path")?;

        match filename {
            "uv.lock" | "poetry.lock" | "pdm.lock" | "pylock.toml" => self.parse_toml_lock(path),
            "Pipfile.lock" => self.parse_pipfile_lock(path),
            "requirements.lock" => self.parse_pinned_requirements(path),
            "conda-lock.yml" | "conda-lock.yaml" => self.parse_conda_lock(path),
            _ => anyhow::bail!("Unsupported lock file: {filename}"),
        }
    }

    /// Try to find and parse a lock file for the given project.
    ///
    /// The project root is checked first, in preference order. If it has none,
    /// the search continues into subdirectories to a bounded depth rather than
    /// concluding that nothing is installed - deciding "no lock file" when one
    /// exists two lines away means every row is compared against its declared
    /// spec instead of against what is installed.
    ///
    /// The descent is deliberately narrow. `detect` only finds manifests at the
    /// project root, so a lock file belonging to a *sibling distribution* one
    /// directory down would be matched against the root's dependency list and
    /// report versions this project does not have. A subdirectory is therefore
    /// only eligible if it carries no manifest of its own: `src/`, `app/` and
    /// their kin, which are layout, not a separate project. Using one is
    /// reported on stderr, because the user should know where the installed
    /// column came from.
    pub fn find_and_parse(&self, dir: &Path) -> Result<HashMap<String, Version>> {
        match Self::locate(dir) {
            Some(path) => {
                if path.parent() != Some(dir) {
                    eprintln!(
                        "note: no lock file in {}; using {} for installed versions",
                        dir.display(),
                        path.display()
                    );
                }
                self.parse(&path)
            }
            None => Ok(HashMap::new()),
        }
    }

    /// Files whose presence makes a directory its own Python project. This
    /// must recognise at least everything `detector.rs` does, or a sibling
    /// distribution declared only through one of the shapes missing here would
    /// slip through the gate and lend its lock file to the root.
    const MANIFESTS: [&'static str; 7] = [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "Pipfile",
        "requirements.txt",
        "environment.yml",
        "environment.yaml",
    ];

    /// True when `dir` carries a manifest of its own. `requirements*.txt` is a
    /// family, not one name, so it is matched by prefix like the detector does.
    fn has_manifest(dir: &Path) -> bool {
        if Self::MANIFESTS.iter().any(|m| dir.join(m).is_file()) {
            return true;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("requirements") && name.ends_with(".txt") && entry.path().is_file()
        })
    }

    /// The lock file `find_and_parse` will read, if any.
    fn locate(dir: &Path) -> Option<PathBuf> {
        for filename in LOCK_FILES {
            let candidate = dir.join(filename);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        Self::locate_below(dir, MAX_SEARCH_DEPTH)
    }

    /// Breadth-first so a shallower lock file always beats a deeper one, and
    /// preference order still decides between two at the same depth.
    fn locate_below(dir: &Path, depth: usize) -> Option<PathBuf> {
        if depth == 0 {
            return None;
        }
        let mut subdirs = Vec::new();
        let entries = fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || SKIP_DIRS.contains(&name) {
                continue;
            }
            // A directory with its own manifest is a separate distribution.
            // Its lock file answers a question about *its* dependencies, not
            // the ones pcu is checking.
            if Self::has_manifest(&path) {
                continue;
            }
            subdirs.push(path);
        }
        subdirs.sort();

        for subdir in &subdirs {
            for filename in LOCK_FILES {
                let candidate = subdir.join(filename);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        for subdir in &subdirs {
            if let Some(found) = Self::locate_below(subdir, depth - 1) {
                return Some(found);
            }
        }
        None
    }

    /// Check if we can parse this lock file
    pub fn can_parse(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| LOCK_FILES.contains(&n) || n == "conda-lock.yaml")
            .unwrap_or(false)
    }

    /// Parse a TOML lock file with a package array: `uv.lock`, `poetry.lock`,
    /// `pdm.lock` and PEP 751 `pylock.toml`.
    ///
    /// One function, not three: the three formats differ only in which optional
    /// fields they populate, and the shared resolution rule reads whichever are
    /// present.
    fn parse_toml_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let lock_file: TomlLockFile = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let mut candidates = Candidates::default();
        let packages = if lock_file.package.is_empty() {
            &lock_file.packages
        } else {
            &lock_file.package
        };

        for package in packages {
            if Self::is_root_project(package) {
                Self::collect_uv_root(&mut candidates, package);
                continue;
            }
            if let Some(version) = &package.version {
                candidates.add(&package.name, version);
            }
        }

        Ok(candidates.resolve(path))
    }

    /// True for the entry describing the project being checked rather than one
    /// of its dependencies. uv writes the workspace root as an `editable` or
    /// `virtual` source; no other format emits a self entry.
    fn is_root_project(package: &TomlPackage) -> bool {
        package
            .source
            .as_ref()
            .and_then(|s| s.as_table())
            .is_some_and(|t| t.contains_key("editable") || t.contains_key("virtual"))
    }

    /// Read the root entry of a `uv.lock`: the declared ranges from
    /// `[package.metadata] requires-dist`, and any dependency edge that names a
    /// version outright (uv does this when the resolution forked, which is
    /// exactly the case where the duplicate matters).
    fn collect_uv_root(candidates: &mut Candidates, package: &TomlPackage) {
        if let Some(requires) = package
            .metadata
            .as_ref()
            .and_then(|m| m.get("requires-dist"))
            .and_then(|r| r.as_array())
        {
            for entry in requires {
                let Some(name) = entry.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                if let Some(spec) = entry.get("specifier").and_then(|s| s.as_str())
                    && !spec.trim().is_empty()
                {
                    candidates.add_root_spec(name, spec);
                }
            }
        }

        // uv's dependency edges are an array of `{ name, version?, source? }`.
        // poetry and pdm use other shapes here, which `as_array` rejects.
        if let Some(deps) = package.dependencies.as_ref().and_then(|d| d.as_array()) {
            for dep in deps {
                if let Some(name) = dep.get("name").and_then(|n| n.as_str())
                    && let Some(version) = dep.get("version").and_then(|v| v.as_str())
                {
                    candidates.add_direct(name, version);
                }
            }
        }
    }

    /// Parse `Pipfile.lock` (JSON). `default` and `develop` each hold one pin
    /// per distribution, written as a `==` specifier, so every entry is the
    /// copy the project resolved to.
    fn parse_pipfile_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let mut candidates = Candidates::default();
        for section in ["default", "develop"] {
            let Some(entries) = parsed.get(section).and_then(|v| v.as_object()) else {
                continue;
            };
            for (name, entry) in entries {
                let Some(version) = entry.get("version").and_then(|v| v.as_str()) else {
                    // A VCS or path pin has `git`/`path` instead of `version`.
                    // There is no installed version to report for it.
                    continue;
                };
                candidates.add_direct(name, version.trim_start_matches('='));
            }
        }

        Ok(candidates.resolve(path))
    }

    /// Parse a fully pinned requirements file used as a lock file: rye's
    /// `requirements.lock`, and anything `pip-compile` produced.
    fn parse_pinned_requirements(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let mut candidates = Candidates::default();
        for line in content.lines() {
            let line = line.split(" #").next().unwrap_or(line).trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
                continue;
            }
            let Some(req) = pep508::parse(line) else {
                continue;
            };
            let Some(pinned) = req.specifier.strip_prefix("==") else {
                // Not a pin. A lock file that is not pinned is not telling us
                // what is installed, so there is nothing to record.
                continue;
            };
            candidates.add_direct(&req.name, pinned.trim());
        }

        Ok(candidates.resolve(path))
    }

    /// Parse `conda-lock.yml`. Its `package` list is per-platform, so the same
    /// distribution appears once per locked platform; the shared rule takes the
    /// highest, since the file does not say which platform is this machine.
    fn parse_conda_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let mut candidates = Candidates::default();
        if let Some(packages) = parsed.get("package").and_then(|p| p.as_sequence()) {
            for package in packages {
                let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let Some(version) = package.get("version").and_then(|v| v.as_str()) else {
                    continue;
                };
                candidates.add(name, version);
            }
        }

        Ok(candidates.resolve(path))
    }
}

impl Default for LockfileParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_lock(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

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
        let versions = parser.parse_toml_lock(&path).unwrap();

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

[package.dependencies]
urllib3 = ">=1.21.1,<3"

[[package]]
name = "Django"
version = "4.2.0"
description = "A high-level Python Web framework"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = LockfileParser::new();
        let versions = parser.parse_toml_lock(&path).unwrap();

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
dependencies = ["colorama; platform_system == \"Windows\""]

[[package]]
name = "Flask"
version = "2.3.0"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = LockfileParser::new();
        let versions = parser.parse_toml_lock(&path).unwrap();

        assert_eq!(versions.len(), 2);
        assert_eq!(versions.get("click").unwrap().to_string(), "8.1.3");
        assert_eq!(versions.get("flask").unwrap().to_string(), "2.3.0");
    }

    #[test]
    fn pylock_packages_array_is_read() {
        let lock_content = r#"
lock-version = "1.0"

[[packages]]
name = "attrs"
version = "25.1.0"
"#;
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();

        let versions = LockfileParser::new()
            .parse_toml_lock(temp_file.path())
            .unwrap();
        assert_eq!(versions.get("attrs").unwrap().to_string(), "25.1.0");
    }

    #[test]
    fn test_can_parse_matches_parse() {
        let parser = LockfileParser::new();
        let temp_dir = tempfile::tempdir().unwrap();

        for name in LOCK_FILES {
            assert!(parser.can_parse(&PathBuf::from(name)), "{name}");
            // Every accepted name must have a parse arm. The content below is
            // an empty but valid document in each format, so the only way this
            // errors is a missing arm.
            let empty = if name == "Pipfile.lock" { "{}" } else { "" };
            let path = write_lock(temp_dir.path(), name, empty);
            assert!(
                parser.parse(&path).is_ok(),
                "{name} is accepted by can_parse but parse refused it"
            );
        }

        assert!(!parser.can_parse(&PathBuf::from("requirements.txt")));
    }

    #[test]
    fn test_find_and_parse() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_lock(
            temp_dir.path(),
            "uv.lock",
            "[[package]]\nname = \"requests\"\nversion = \"2.31.0\"\n",
        );

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions.get("requests").unwrap().to_string(), "2.31.0");
    }

    #[test]
    fn test_find_and_parse_no_lockfile() {
        let temp_dir = tempfile::tempdir().unwrap();

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();

        // Should return empty map, not an error
        assert_eq!(versions.len(), 0);
    }

    #[test]
    fn a_lock_file_one_directory_down_is_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let nested = temp_dir.path().join("src");
        fs::create_dir(&nested).unwrap();
        write_lock(
            &nested,
            "poetry.lock",
            "[[package]]\nname = \"requests\"\nversion = \"2.31.0\"\n",
        );

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();
        assert_eq!(versions.get("requests").unwrap().to_string(), "2.31.0");
    }

    #[test]
    fn a_sibling_distributions_lock_file_is_not_borrowed() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sibling = temp_dir.path().join("other-project");
        fs::create_dir(&sibling).unwrap();
        write_lock(&sibling, "pyproject.toml", "[project]\nname = \"other\"\n");
        write_lock(
            &sibling,
            "poetry.lock",
            "[[package]]\nname = \"requests\"\nversion = \"2.31.0\"\n",
        );

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();
        assert!(versions.is_empty());
    }

    // The gate has to recognise every manifest shape the detector does. A
    // sibling declared only through `requirements-dev.txt` or
    // `environment.yaml` is still a separate distribution.
    #[test]
    fn a_sibling_declared_by_a_requirements_variant_is_not_borrowed() {
        for manifest in ["requirements-dev.txt", "environment.yaml"] {
            let temp_dir = tempfile::tempdir().unwrap();
            let sibling = temp_dir.path().join("other-project");
            fs::create_dir(&sibling).unwrap();
            write_lock(&sibling, manifest, "");
            write_lock(
                &sibling,
                "poetry.lock",
                "[[package]]\nname = \"requests\"\nversion = \"2.31.0\"\n",
            );

            let versions = LockfileParser::new()
                .find_and_parse(temp_dir.path())
                .unwrap();
            assert!(versions.is_empty(), "{manifest} did not gate the descent");
        }
    }

    #[test]
    fn virtualenvs_are_not_searched() {
        let temp_dir = tempfile::tempdir().unwrap();
        let venv = temp_dir.path().join(".venv");
        fs::create_dir(&venv).unwrap();
        write_lock(
            &venv,
            "poetry.lock",
            "[[package]]\nname = \"requests\"\nversion = \"2.31.0\"\n",
        );

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();
        assert!(versions.is_empty());
    }

    #[test]
    fn pipfile_lock_pins_are_read() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_lock(
            temp_dir.path(),
            "Pipfile.lock",
            r#"{
  "_meta": {},
  "default": {
    "requests": { "version": "==2.31.0" },
    "from-git": { "git": "https://example.invalid/x.git" }
  },
  "develop": { "pytest": { "version": "==8.0.0" } }
}"#,
        );

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();
        assert_eq!(versions.get("requests").unwrap().to_string(), "2.31.0");
        assert_eq!(versions.get("pytest").unwrap().to_string(), "8.0.0");
        assert!(!versions.contains_key("from-git"));
    }

    #[test]
    fn conda_lock_keeps_the_highest_platform_copy() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_lock(
            temp_dir.path(),
            "conda-lock.yml",
            "version: 1\npackage:\n  - name: numpy\n    version: 1.24.3\n    platform: linux-64\n  - name: numpy\n    version: 1.24.4\n    platform: osx-64\n",
        );

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();
        assert_eq!(versions.get("numpy").unwrap().to_string(), "1.24.4");
    }

    #[test]
    fn pinned_requirements_lock_is_read() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_lock(
            temp_dir.path(),
            "requirements.lock",
            "# generated by rye\n-e file:.\nrequests==2.31.0\nflask==2.3.0  # via -r\nunpinned>=1.0\n",
        );

        let versions = LockfileParser::new()
            .find_and_parse(temp_dir.path())
            .unwrap();
        assert_eq!(versions.get("requests").unwrap().to_string(), "2.31.0");
        assert_eq!(versions.get("flask").unwrap().to_string(), "2.3.0");
        assert!(!versions.contains_key("unpinned"));
    }

    #[test]
    fn a_marker_split_duplicate_resolves_against_the_root_range() {
        // uv forks on markers, so numpy appears twice. Last-insert-wins made
        // the answer depend on file order; the root's declared range decides.
        let lock_content = r#"
version = 1

[[package]]
name = "myproject"
version = "0.1.0"
source = { editable = "." }

[package.metadata]
requires-dist = [{ name = "numpy", specifier = "<2" }]

[[package]]
name = "numpy"
version = "1.26.4"

[[package]]
name = "numpy"
version = "2.1.0"
"#;
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();

        let versions = LockfileParser::new()
            .parse_toml_lock(temp_file.path())
            .unwrap();
        assert_eq!(versions.get("numpy").unwrap().to_string(), "1.26.4");
        // The project's own entry is not one of its dependencies.
        assert!(!versions.contains_key("myproject"));
    }

    #[test]
    fn the_root_resolved_edge_beats_a_newer_fork_in_range() {
        let lock_content = r#"
version = 1

[[package]]
name = "myproject"
version = "0.1.0"
source = { virtual = "." }
dependencies = [{ name = "numpy", version = "1.26.4" }]

[package.metadata]
requires-dist = [{ name = "numpy", specifier = ">=1.26" }]

[[package]]
name = "numpy"
version = "1.26.4"

[[package]]
name = "numpy"
version = "2.1.0"
"#;
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();

        let versions = LockfileParser::new()
            .parse_toml_lock(temp_file.path())
            .unwrap();
        // Highest-satisfying alone would report 2.1.0.
        assert_eq!(versions.get("numpy").unwrap().to_string(), "1.26.4");
    }

    #[test]
    fn duplicates_without_a_root_range_resolve_to_the_highest() {
        let lock_content = r#"
[[package]]
name = "numpy"
version = "1.26.4"

[[package]]
name = "numpy"
version = "1.24.3"
"#;
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();

        let versions = LockfileParser::new()
            .parse_toml_lock(temp_file.path())
            .unwrap();
        assert_eq!(versions.get("numpy").unwrap().to_string(), "1.26.4");
    }

    #[test]
    fn unreadable_versions_are_skipped_and_the_rest_survive() {
        let lock_content = r#"
[[package]]
name = "broken"
version = "not a version"

[[package]]
name = "fine"
version = "1.2.3"
"#;
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();

        let versions = LockfileParser::new()
            .parse_toml_lock(temp_file.path())
            .unwrap();
        assert!(!versions.contains_key("broken"));
        assert_eq!(versions.get("fine").unwrap().to_string(), "1.2.3");
    }

    #[test]
    fn names_are_normalized_the_same_way_as_the_manifest_parsers() {
        let lock_content = r#"
[[package]]
name = "Typing_Extensions"
version = "4.12.2"
"#;
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(lock_content.as_bytes()).unwrap();

        let versions = LockfileParser::new()
            .parse_toml_lock(temp_file.path())
            .unwrap();
        assert_eq!(
            versions
                .get(&pep508::normalize_name("typing-extensions"))
                .unwrap()
                .to_string(),
            "4.12.2"
        );
    }
}
