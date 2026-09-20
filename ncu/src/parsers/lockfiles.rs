use anyhow::{Context, Result};
use check_updates_core::{Version, VersionSpec};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use crate::detector::LockfileType;

/// Shared resolution rule for every lock-file format.
///
/// A lock file can record several installed copies of the same package (npm
/// hoists one and nests the rest, yarn keys one entry per declared range, pnpm
/// lists every resolution in `packages`/`snapshots`). ncu reports one installed
/// version per dependency, and severity is computed against it, so the copy we
/// pick has to be the one that answers "which installed copy satisfies the root
/// project's declared range".
///
/// The rule, applied identically by all four parsers:
///
/// 1. If the lock file records which copy the root project itself resolved
///    to, use it. npm and bun install the root's own dependencies at the top
///    level of `node_modules` by construction (a nested copy exists only
///    because some *other* package wanted a different version), and pnpm's
///    `importers["."]` names the resolved version outright. This is the only
///    step that answers the question exactly.
/// 2. Otherwise, if the lock file records the root project's declared range
///    for the package (npm's `packages[""]`, bun's `workspaces[""]`), keep only
///    the candidates that satisfy it and take the highest.
/// 3. If nothing satisfies the range - or the range is unknown, which is the
///    yarn case, since `yarn.lock` does not record who asked - fall back to the
///    highest of all candidates.
///
/// Steps 2 and 3 are fallbacks, not the rule: "highest satisfying" picks a
/// nested copy whenever a transitive dependency pulled in something newer than
/// the root's own hoisted copy. They are deterministic, which the previous
/// per-format tie-breaks (`insert` last-wins, `or_insert` first-wins, and map
/// iteration order for npm) were not.
#[derive(Default)]
struct Candidates {
    /// Every installed version seen for a package name.
    versions: HashMap<String, Vec<Version>>,
    /// The copy the root project resolved to, where the format records it.
    direct: HashMap<String, Version>,
    /// The root project's declared range, where the format records it.
    root_specs: HashMap<String, VersionSpec>,
    /// Version strings the lock file contained that could not be parsed.
    ///
    /// `core::Version::from_str` is strict: a segment that will not parse is an
    /// error rather than a silent zero. Dropping those on the floor would make
    /// a malformed lock file look like an empty one, so they are counted and
    /// reported.
    unparsed: Vec<String>,
}

impl Candidates {
    fn add(&mut self, name: &str, version_str: &str) {
        match Version::from_str(version_str) {
            Ok(version) => self
                .versions
                .entry(name.to_string())
                .or_default()
                .push(version),
            Err(_) => self.unparsed.push(format!("{name}@{version_str}")),
        }
    }

    /// Record the copy the root project itself resolved to. Also counted as a
    /// candidate, so a package with only a top-level copy still resolves.
    fn add_direct(&mut self, name: &str, version_str: &str) {
        self.add(name, version_str);
        if let Ok(version) = Version::from_str(version_str) {
            self.direct.insert(name.to_string(), version);
        }
    }

    fn add_root_spec(&mut self, name: &str, spec_str: &str) {
        if let Ok(spec) = VersionSpec::parse(spec_str) {
            self.root_specs.insert(name.to_string(), spec);
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

/// Collect the declared ranges of a package.json-shaped dependency block.
fn collect_root_specs(candidates: &mut Candidates, container: &serde_json::Value) {
    const FIELDS: [&str; 4] = [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ];
    for field in FIELDS {
        if let Some(map) = container.get(field).and_then(|v| v.as_object()) {
            for (name, spec) in map {
                if let Some(spec) = spec.as_str() {
                    candidates.add_root_spec(name, spec);
                }
            }
        }
    }
}

pub struct LockfileParser;

impl LockfileParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse installed versions from a lock file
    pub fn parse(
        &self,
        path: &Path,
        lockfile_type: LockfileType,
    ) -> Result<HashMap<String, Version>> {
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

        let mut candidates = Candidates::default();

        // npm v7+ format: packages field with "" for root and "node_modules/pkg" for deps
        if let Some(packages) = parsed.get("packages").and_then(|v| v.as_object()) {
            // The root entry carries the declared ranges - the thing severity is
            // actually computed against.
            if let Some(root) = packages.get("") {
                collect_root_specs(&mut candidates, root);
            }

            for (key, pkg_data) in packages {
                if key.is_empty() {
                    continue;
                }

                // Extract package name from path (node_modules/name or node_modules/@scope/name)
                let Some(name) = key.strip_prefix("node_modules/") else {
                    // A workspace member directory, not an install path.
                    continue;
                };

                // A top-level `node_modules/<name>` is the copy the root
                // project resolves to. Nested copies (a/node_modules/b) are
                // still candidates for packages the root does not hoist.
                let Some(version_str) = pkg_data.get("version").and_then(|v| v.as_str()) else {
                    continue;
                };
                match name.rsplit_once("node_modules/") {
                    Some((_, nested)) => candidates.add(nested, version_str),
                    None => candidates.add_direct(name, version_str),
                }
            }
        }
        // npm v6 format: dependencies field
        else if let Some(dependencies) = parsed.get("dependencies").and_then(|v| v.as_object()) {
            Self::parse_npm_v6_deps(dependencies, &mut candidates, true);
        }

        Ok(candidates.resolve(path))
    }

    /// `top_level` is true for the root `dependencies` map, whose entries are
    /// the hoisted copies the root project resolves to.
    fn parse_npm_v6_deps(
        deps: &serde_json::Map<String, serde_json::Value>,
        candidates: &mut Candidates,
        top_level: bool,
    ) {
        for (name, data) in deps {
            if let Some(version_str) = data.get("version").and_then(|v| v.as_str()) {
                if top_level {
                    candidates.add_direct(name, version_str);
                } else {
                    candidates.add(name, version_str);
                }
            }
            // v6 nests transitive copies under each entry's `dependencies`.
            if let Some(nested) = data.get("dependencies").and_then(|v| v.as_object()) {
                Self::parse_npm_v6_deps(nested, candidates, false);
            }
        }
    }

    /// Parse pnpm-lock.yaml
    fn parse_pnpm_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let mut candidates = Candidates::default();

        // importers["."] records the root project's declared specifiers.
        if let Some(importers) = parsed.get("importers").and_then(|v| v.as_mapping())
            && let Some(root) = importers.get(serde_yaml::Value::from("."))
        {
            Self::collect_pnpm_root_specs(&mut candidates, root);
        }

        // pnpm v9 format: snapshots or packages
        // Package entries like: "express@4.18.2" or "express@4.18.2(supports-color@8.0.0)"
        for section in ["packages", "snapshots"] {
            if let Some(entries) = parsed.get(section).and_then(|v| v.as_mapping()) {
                for (key, _) in entries {
                    if let Some(key_str) = key.as_str()
                        && let Some((name, version_str)) = Self::split_pnpm_package_key(key_str)
                    {
                        candidates.add(&name, version_str);
                    }
                }
            }
        }

        Ok(candidates.resolve(path))
    }

    fn collect_pnpm_root_specs(candidates: &mut Candidates, root: &serde_yaml::Value) {
        for field in ["dependencies", "devDependencies", "optionalDependencies"] {
            if let Some(map) = root.get(field).and_then(|v| v.as_mapping()) {
                for (name, entry) in map {
                    let Some(name) = name.as_str() else { continue };
                    // pnpm v9: { specifier: "^4.18.2", version: "4.18.2(peer@1)" }
                    // pnpm v6 and earlier used a bare version string.
                    if let Some(spec) = entry.get("specifier").and_then(|v| v.as_str()) {
                        candidates.add_root_spec(name, spec);
                    }
                    let resolved = entry
                        .get("version")
                        .and_then(|v| v.as_str())
                        .or_else(|| entry.as_str());
                    if let Some(resolved) = resolved {
                        let version_str = resolved.split('(').next().unwrap_or(resolved);
                        candidates.add_direct(name, version_str);
                    }
                }
            }
        }
    }

    /// Split a pnpm package key like "express@4.18.2" or "@types/node@20.0.0"
    fn split_pnpm_package_key(key: &str) -> Option<(String, &str)> {
        // Handle scoped packages: @scope/name@version
        let (name, version_str) = if let Some(rest) = key.strip_prefix('@') {
            // Find the second @ which separates name from version
            let at_pos = rest.find('@')?;
            let name = &key[..at_pos + 1];
            let version_part = &rest[at_pos + 1..];
            // Remove any peer dep suffix like (supports-color@8.0.0)
            let version_str = version_part.split('(').next().unwrap_or(version_part);
            (name.to_string(), version_str)
        } else {
            // Regular package: name@version
            let (name, version_part) = key.split_once('@')?;
            let version_str = version_part.split('(').next().unwrap_or(version_part);
            (name.to_string(), version_str)
        };

        Some((name, version_str))
    }

    /// Parse pnpm package key like "express@4.18.2" or "@types/node@20.0.0"
    #[cfg(test)]
    fn parse_pnpm_package_key(key: &str) -> Option<(String, Version)> {
        let (name, version_str) = Self::split_pnpm_package_key(key)?;
        Version::from_str(version_str).ok().map(|v| (name, v))
    }

    /// Parse yarn.lock (yarn classic and berry)
    ///
    /// `yarn.lock` records the declared range in each entry header but never
    /// says which of them the root project asked for, so this is the one format
    /// where the shared rule falls through to "highest candidate wins".
    fn parse_yarn_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let mut candidates = Candidates::default();

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
            if trimmed.starts_with("version")
                && let Some(version_str) = Self::parse_yarn_version_line(trimmed)
            {
                for pkg in &current_packages {
                    candidates.add(pkg, version_str);
                }
            }
        }

        Ok(candidates.resolve(path))
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
        if let Some(rest) = spec.strip_prefix('@') {
            if let Some(at_pos) = rest.find('@') {
                return Some(spec[..at_pos + 1].to_string());
            }
        } else if let Some(at_pos) = spec.find('@') {
            return Some(spec[..at_pos].to_string());
        }
        None
    }

    fn parse_yarn_version_line(line: &str) -> Option<&str> {
        // version "1.2.3" or version: "1.2.3"
        let line = line.trim_start_matches("version").trim();
        let line = line.trim_start_matches(':').trim();
        let version_str = line.trim_matches('"');
        if version_str.is_empty() {
            return None;
        }
        Some(version_str)
    }

    /// Parse bun's lock file.
    ///
    /// Bun has two formats. The newer `bun.lock` is text and is parsed here;
    /// the older `bun.lockb` is binary and is not parseable without bun itself.
    /// The binary case warns instead of returning an empty map quietly: an empty
    /// map is indistinguishable from "nothing installed", and downstream every
    /// dependency then falls back to its spec's base version with no sign that
    /// the reported "installed" column is a guess.
    fn parse_bun_lock(&self, path: &Path) -> Result<HashMap<String, Version>> {
        if path.extension().is_some_and(|ext| ext == "lockb") {
            eprintln!(
                "warning: {} is bun's binary lock file, which ncu cannot read; \
                 installed versions shown are the ranges' base versions, not what is installed. \
                 Run `bun install --save-text-lockfile` to produce a readable bun.lock.",
                path.display()
            );
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        // bun.lock is JSONC: comments and trailing commas are legal.
        let json = strip_jsonc(&content);
        let parsed: serde_json::Value = serde_json::from_str(&json)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let mut candidates = Candidates::default();

        // workspaces[""] is the root project's package.json, ranges included.
        if let Some(workspaces) = parsed.get("workspaces").and_then(|v| v.as_object())
            && let Some(root) = workspaces.get("")
        {
            collect_root_specs(&mut candidates, root);
        }

        // packages: { "<install path>": ["<name>@<version>", registry, meta, hash] }
        // The install path is the package name for a top-level copy and
        // "<parent>/<name>" for a nested one, so a key equal to the descriptor's
        // own name is the copy the root project resolves to.
        if let Some(packages) = parsed.get("packages").and_then(|v| v.as_object()) {
            for (key, entry) in packages {
                let Some(descriptor) = entry
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                else {
                    continue;
                };
                // Reuse the pnpm key splitter: same "name@version" shape.
                if let Some((name, version_str)) = Self::split_pnpm_package_key(descriptor) {
                    if *key == name {
                        candidates.add_direct(&name, version_str);
                    } else {
                        candidates.add(&name, version_str);
                    }
                }
            }
        }

        Ok(candidates.resolve(path))
    }
}

/// Strip JSONC comments and trailing commas so `serde_json` can read the text.
///
/// Only the two extensions bun actually emits are handled: `//` and `/* */`
/// comments, and a trailing comma before `}` or `]`. String literals and their
/// escapes are respected, so a `//` inside a URL, a `/*` inside a hash, or a
/// `,` at the end of a string all survive.
///
/// Two passes on purpose: comments are removed first, then trailing commas are
/// found on comment-free text. A single pass would have to look ahead through
/// a comment sitting between a comma and its closing bracket, and getting that
/// wrong turns a legal lock file into a parse error.
fn strip_jsonc(input: &str) -> String {
    strip_trailing_commas(&strip_comments(input))
}

/// Remove `//` and `/* */` comments outside string literals. Line comments
/// keep their newline so serde_json's error positions still point at the
/// right line of the original file.
fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }

    out
}

/// Remove a `,` that is followed, after optional whitespace, by `}` or `]`.
/// Runs on comment-free text; string literals are skipped verbatim.
fn strip_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                let next = chars.clone().find(|c| !c.is_whitespace());
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }

    out
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

        let (name, version) = LockfileParser::parse_pnpm_package_key("@types/node@20.0.0").unwrap();
        assert_eq!(name, "@types/node");
        assert_eq!(version.to_string(), "20.0.0");
    }

    #[test]
    fn duplicate_copies_resolve_against_the_root_range() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".json")?;
        write!(
            file,
            r#"{{
  "lockfileVersion": 3,
  "packages": {{
    "": {{ "dependencies": {{ "lodash": "^3.10.0" }} }},
    "node_modules/lodash": {{ "version": "3.10.1" }},
    "node_modules/gulp/node_modules/lodash": {{ "version": "4.17.21" }}
  }}
}}"#
        )?;

        let versions = LockfileParser::new().parse(file.path(), LockfileType::Npm)?;
        // Highest-wins alone would pick 4.17.21; the root range picks 3.10.1.
        assert_eq!(versions.get("lodash").unwrap().to_string(), "3.10.1");
        Ok(())
    }

    #[test]
    fn hoisted_top_level_copy_beats_a_newer_nested_copy_in_range() -> Result<()> {
        // Both copies satisfy ^4.0.0. The root resolves to the hoisted one;
        // the nested one exists because `gulp` asked for something newer.
        // "Highest satisfying" would report 4.17.21 as installed.
        let mut file = NamedTempFile::with_suffix(".json")?;
        write!(
            file,
            r#"{{
  "lockfileVersion": 3,
  "packages": {{
    "": {{ "dependencies": {{ "lodash": "^4.0.0" }} }},
    "node_modules/lodash": {{ "version": "4.17.20" }},
    "node_modules/gulp/node_modules/lodash": {{ "version": "4.17.21" }},
    "node_modules/gulp/node_modules/only-nested": {{ "version": "1.0.0" }}
  }}
}}"#
        )?;

        let versions = LockfileParser::new().parse(file.path(), LockfileType::Npm)?;
        assert_eq!(versions.get("lodash").unwrap().to_string(), "4.17.20");
        // A package with no top-level copy still resolves from its nested one.
        assert_eq!(versions.get("only-nested").unwrap().to_string(), "1.0.0");
        Ok(())
    }

    #[test]
    fn npm_v6_top_level_copy_wins_over_nested() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".json")?;
        write!(
            file,
            r#"{{
  "lockfileVersion": 1,
  "dependencies": {{
    "lodash": {{ "version": "4.17.20" }},
    "gulp": {{
      "version": "4.0.2",
      "dependencies": {{ "lodash": {{ "version": "4.17.21" }} }}
    }}
  }}
}}"#
        )?;

        let versions = LockfileParser::new().parse(file.path(), LockfileType::Npm)?;
        assert_eq!(versions.get("lodash").unwrap().to_string(), "4.17.20");
        assert_eq!(versions.get("gulp").unwrap().to_string(), "4.0.2");
        Ok(())
    }

    #[test]
    fn pnpm_importer_resolution_is_authoritative() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".yaml")?;
        write!(
            file,
            "lockfileVersion: '9.0'\n\
             importers:\n  .:\n    dependencies:\n      lodash:\n        specifier: ^4.0.0\n        version: 4.17.20(peer@1.0.0)\n\
             packages:\n  lodash@4.17.20: {{}}\n  lodash@4.17.21: {{}}\n"
        )?;

        let versions = LockfileParser::new().parse(file.path(), LockfileType::Pnpm)?;
        assert_eq!(versions.get("lodash").unwrap().to_string(), "4.17.20");
        Ok(())
    }

    #[test]
    fn without_a_root_range_the_highest_copy_wins_deterministically() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".lock")?;
        write!(
            file,
            "lodash@^4.0.0:\n  version \"4.0.0\"\n\nlodash@^4.17.0:\n  version \"4.17.21\"\n"
        )?;

        let versions = LockfileParser::new().parse(file.path(), LockfileType::Yarn)?;
        assert_eq!(versions.get("lodash").unwrap().to_string(), "4.17.21");
        Ok(())
    }

    #[test]
    fn parses_text_bun_lock_with_comments_and_trailing_commas() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".lock")?;
        write!(
            file,
            r#"{{
  // bun lockfile
  "lockfileVersion": 1,
  "workspaces": {{
    "": {{
      "name": "demo",
      "dependencies": {{ "lodash": "^4.17.0" }},
    }},
  }},
  /* resolved packages */
  "packages": {{
    "lodash": ["lodash@4.17.21", "", {{}}, "sha512-abc"],
    "@types/node": ["@types/node@20.11.0", "", {{}}, "sha512-def"],
  }},
}}"#
        )?;

        let versions = LockfileParser::new().parse(file.path(), LockfileType::Bun)?;
        assert_eq!(versions.get("lodash").unwrap().to_string(), "4.17.21");
        assert_eq!(versions.get("@types/node").unwrap().to_string(), "20.11.0");
        Ok(())
    }

    #[test]
    fn binary_bun_lockfile_yields_no_versions() -> Result<()> {
        let file = NamedTempFile::with_suffix(".lockb")?;
        let versions = LockfileParser::new().parse(file.path(), LockfileType::Bun)?;
        assert!(versions.is_empty());
        Ok(())
    }

    #[test]
    fn strip_jsonc_leaves_string_contents_alone() {
        let input = r#"{
  // leading
  "url": "https://x/y",
  /* c */ "a": [1, 2,],
}"#;
        let parsed: serde_json::Value = serde_json::from_str(&strip_jsonc(input)).unwrap();
        assert_eq!(parsed["url"], "https://x/y");
        assert_eq!(parsed["a"][1], 2);
    }

    #[test]
    fn strip_jsonc_survives_adversarial_strings_and_comment_placement() {
        // `/*` and `//` inside strings, an escaped quote, a comma that is the
        // last character of a string, and a comment sitting between a trailing
        // comma and its closing bracket.
        let input = r#"{
  "hash": "sha512-ab/*cd*/ef//gh",
  "quoted": "say \"hi\", // not a comment",
  "trailing": "ends with a comma,",
  "list": [1, 2, // last
  ],
  "obj": { "k": "v", /* done */ },
}"#;
        let parsed: serde_json::Value = serde_json::from_str(&strip_jsonc(input)).unwrap();
        assert_eq!(parsed["hash"], "sha512-ab/*cd*/ef//gh");
        assert_eq!(parsed["quoted"], "say \"hi\", // not a comment");
        assert_eq!(parsed["trailing"], "ends with a comma,");
        assert_eq!(parsed["list"][1], 2);
        assert_eq!(parsed["obj"]["k"], "v");
    }

    #[test]
    fn unreadable_lock_versions_are_skipped_not_zeroed() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".json")?;
        write!(
            file,
            r#"{{
  "lockfileVersion": 3,
  "packages": {{
    "": {{}},
    "node_modules/broken": {{ "version": "not-a-version" }},
    "node_modules/fine": {{ "version": "1.2.3" }}
  }}
}}"#
        )?;

        let versions = LockfileParser::new().parse(file.path(), LockfileType::Npm)?;
        assert!(!versions.contains_key("broken"));
        assert_eq!(versions.get("fine").unwrap().to_string(), "1.2.3");
        Ok(())
    }
}
