use super::pep508;
use super::{Dependency, DependencyParser};
use anyhow::{Context, Result};
use check_updates_core::VersionSpec;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use toml::Value;

/// Parser for pyproject.toml files (PEP 621, Poetry, PDM, uv)
///
/// Line numbers are only ever reported when the declaration was found as
/// written. `pcu/src/updater.rs` indexes `lines[line_number - 1]`, so a
/// guessed line number is a wrong-line rewrite waiting to happen. The previous
/// locator guessed twice over: a case-insensitive whole-file substring search
/// (which matched comments, the `name = "..."` key, and `requests-oauthlib`
/// when looking for `requests`) and, failing that, line 1 paired with a
/// synthesized `pkg = "spec"` string that never existed in the file. An
/// unlocated declaration now carries `line_number: None`: it is still
/// reported, and `-u` declines to touch it rather than editing something else.
pub struct PyProjectParser;

impl Default for PyProjectParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Locates dependency declarations in the raw file text.
///
/// Two shapes have to be found, and they need different anchors:
///
/// - A PEP 508 array item is a TOML string whose contents are the requirement
///   verbatim, so it is found by searching for the quoted literal. When the
///   same literal appears more than once, successive lookups hand out
///   successive occurrences, so N declarations map to N distinct lines instead
///   of all collapsing onto the first.
/// - A Poetry-style key is `name = ...` inside a named table, so it is found by
///   scanning that table's lines for a line whose *key* is the name. Anchoring
///   on the key rather than on "contains the name somewhere" is what stops a
///   comment or a longer package name from winning.
///
/// Anything not found this way gets `None`, never a guess.
struct SourceIndex<'a> {
    content: &'a str,
    /// How many times each literal has already been handed out.
    seen: HashMap<String, usize>,
}

impl<'a> SourceIndex<'a> {
    fn new(content: &'a str) -> Self {
        Self {
            content,
            seen: HashMap::new(),
        }
    }

    /// Locate the next unused occurrence of a PEP 508 array item.
    fn locate_array_item(&mut self, dep_str: &str) -> (Option<usize>, String) {
        let nth = self.seen.entry(dep_str.to_string()).or_insert(0);
        let wanted = *nth;
        *nth += 1;

        let mut hits = 0usize;
        for (idx, line) in self.content.lines().enumerate() {
            if line_contains_quoted(line, dep_str) {
                if hits == wanted {
                    return (Some(idx + 1), line.trim().to_string());
                }
                hits += 1;
            }
        }

        // Fewer occurrences than requests: the extra declarations genuinely are
        // not findable as written (a multi-line TOML string, an escape). Do not
        // reuse an earlier line for them.
        (None, dep_str.to_string())
    }

    /// Locate a `key = ...` line, preferring the named table's own span.
    fn locate_key(&self, section: &str, key: &str) -> (Option<usize>, String) {
        let header = format!("[{section}]");
        let mut in_section = false;
        let mut saw_section = false;

        for (idx, line) in self.content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_section = trimmed == header;
                saw_section |= in_section;
                continue;
            }
            if in_section && line_key_is(trimmed, key) {
                return (Some(idx + 1), trimmed.to_string());
            }
        }

        if saw_section {
            // The table exists and the key is not in it: nothing to anchor on.
            return (None, key.to_string());
        }

        // The table was never written as a `[header]` - it may be an inline
        // table or a dotted key. Fall back to a whole-file scan that is still
        // anchored on the key, which is weaker but cannot match a comment or a
        // longer neighbouring package name.
        for (idx, line) in self.content.lines().enumerate() {
            let trimmed = line.trim();
            if line_key_is(trimmed, key) {
                return (Some(idx + 1), trimmed.to_string());
            }
        }

        (None, key.to_string())
    }
}

/// Whether `line` contains `needle` as a complete TOML string literal.
fn line_contains_quoted(line: &str, needle: &str) -> bool {
    line.contains(&format!("\"{needle}\"")) || line.contains(&format!("'{needle}'"))
}

/// Whether a trimmed line declares `key` as a TOML key (bare or quoted).
fn line_key_is(trimmed: &str, key: &str) -> bool {
    if trimmed.starts_with('#') {
        return false;
    }

    let rest = if let Some(r) = trimmed.strip_prefix(key) {
        r
    } else if let Some(r) = trimmed
        .strip_prefix(&format!("\"{key}\""))
        .or_else(|| trimmed.strip_prefix(&format!("'{key}'")))
    {
        r
    } else {
        return false;
    };

    rest.trim_start().starts_with('=')
}

impl PyProjectParser {
    pub fn new() -> Self {
        Self
    }

    /// Read a table of arrays (`group = ["pkg>=1", ...]`) into dependencies.
    fn parse_group_table(
        &self,
        groups: &toml::map::Map<String, Value>,
        section_prefix: &str,
        path: &Path,
        index: &mut SourceIndex<'_>,
        out: &mut Vec<Dependency>,
    ) {
        for (group_name, deps_value) in groups {
            let section = format!("{section_prefix}.{group_name}");
            if let Some(deps) = deps_value.as_array() {
                self.parse_array(deps, &section, path, index, out);
            }
        }
    }

    /// Read an array of PEP 508 requirement strings into dependencies.
    fn parse_array(
        &self,
        deps: &[Value],
        section: &str,
        path: &Path,
        index: &mut SourceIndex<'_>,
        out: &mut Vec<Dependency>,
    ) {
        for dep_value in deps {
            if let Some(dep_str) = dep_value.as_str()
                && let Some(dep) = self.parse_dependency_string(dep_str, path, index, section)
            {
                out.push(dep);
            }
        }
    }

    /// Parse PEP 621 format dependencies
    fn parse_pep621_dependencies(
        &self,
        toml_value: &Value,
        path: &Path,
        index: &mut SourceIndex<'_>,
        out: &mut Vec<Dependency>,
    ) {
        // [project.dependencies] - array of strings
        if let Some(deps) = toml_value
            .get("project")
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_array())
        {
            self.parse_array(deps, "project.dependencies", path, index, out);
        }

        // [project.optional-dependencies] - tables of arrays
        if let Some(optional_deps) = toml_value
            .get("project")
            .and_then(|p| p.get("optional-dependencies"))
            .and_then(|d| d.as_table())
        {
            self.parse_group_table(
                optional_deps,
                "project.optional-dependencies",
                path,
                index,
                out,
            );
        }

        // [build-system].requires - PEP 518. These are real, resolvable,
        // rewritable PyPI requirements (setuptools, hatchling, poetry-core) and
        // go stale exactly like the rest.
        if let Some(requires) = toml_value
            .get("build-system")
            .and_then(|b| b.get("requires"))
            .and_then(|r| r.as_array())
        {
            self.parse_array(requires, "build-system.requires", path, index, out);
        }
    }

    /// Parse Poetry format dependencies
    fn parse_poetry_dependencies(
        &self,
        toml_value: &Value,
        path: &Path,
        index: &mut SourceIndex<'_>,
        out: &mut Vec<Dependency>,
    ) {
        let poetry = toml_value.get("tool").and_then(|t| t.get("poetry"));
        let Some(poetry) = poetry else {
            return;
        };

        for key in ["dependencies", "dev-dependencies"] {
            if let Some(deps) = poetry.get(key).and_then(|d| d.as_table()) {
                let section = format!("tool.poetry.{key}");
                for (pkg_name, version_value) in deps {
                    // Poetry's `python` key constrains the interpreter, not a
                    // PyPI distribution.
                    if pkg_name == "python" {
                        continue;
                    }
                    if let Some(dep) =
                        self.parse_poetry_dependency(pkg_name, version_value, path, index, &section)
                    {
                        out.push(dep);
                    }
                }
            }
        }

        // [tool.poetry.group.*.dependencies]
        if let Some(groups) = poetry.get("group").and_then(|g| g.as_table()) {
            for (group_name, group_value) in groups {
                let section = format!("tool.poetry.group.{group_name}.dependencies");
                if let Some(deps) = group_value.get("dependencies").and_then(|d| d.as_table()) {
                    for (pkg_name, version_value) in deps {
                        if pkg_name == "python" {
                            continue;
                        }
                        if let Some(dep) = self.parse_poetry_dependency(
                            pkg_name,
                            version_value,
                            path,
                            index,
                            &section,
                        ) {
                            out.push(dep);
                        }
                    }
                }
            }
        }
    }

    /// Parse PDM format dependencies
    fn parse_pdm_dependencies(
        &self,
        toml_value: &Value,
        path: &Path,
        index: &mut SourceIndex<'_>,
        out: &mut Vec<Dependency>,
    ) {
        let pdm = toml_value.get("tool").and_then(|t| t.get("pdm"));
        let Some(pdm) = pdm else {
            return;
        };

        if let Some(deps) = pdm.get("dependencies").and_then(|d| d.as_array()) {
            self.parse_array(deps, "tool.pdm.dependencies", path, index, out);
        }

        if let Some(dev_deps) = pdm.get("dev-dependencies").and_then(|d| d.as_table()) {
            self.parse_group_table(dev_deps, "tool.pdm.dev-dependencies", path, index, out);
        }
    }

    /// Parse uv's pre-PEP-735 dev dependencies.
    ///
    /// `[tool.uv] dev-dependencies = [...]` is still widespread, and pcu prints
    /// "uv" as the detected manager for these projects, so silently reporting
    /// none of their dev dependencies is the worst of both worlds.
    /// `[tool.uv.sources]` is deliberately not read: it redirects a dependency
    /// to a path, a git ref or an alternate index, where the version does not
    /// come from PyPI and there is nothing for pcu to compare against.
    fn parse_uv_dependencies(
        &self,
        toml_value: &Value,
        path: &Path,
        index: &mut SourceIndex<'_>,
        out: &mut Vec<Dependency>,
    ) {
        let Some(uv) = toml_value.get("tool").and_then(|t| t.get("uv")) else {
            return;
        };

        match uv.get("dev-dependencies") {
            Some(Value::Array(deps)) => {
                self.parse_array(deps, "tool.uv.dev-dependencies", path, index, out);
            }
            Some(Value::Table(groups)) => {
                self.parse_group_table(groups, "tool.uv.dev-dependencies", path, index, out);
            }
            _ => {}
        }
    }

    /// Parse PEP 735 dependency-groups format
    fn parse_dependency_groups(
        &self,
        toml_value: &Value,
        path: &Path,
        index: &mut SourceIndex<'_>,
        out: &mut Vec<Dependency>,
    ) {
        if let Some(groups) = toml_value
            .get("dependency-groups")
            .and_then(|d| d.as_table())
        {
            self.parse_group_table(groups, "dependency-groups", path, index, out);
        }
    }

    /// Parse a Poetry dependency entry which can be a string or inline table
    fn parse_poetry_dependency(
        &self,
        name: &str,
        value: &Value,
        path: &Path,
        index: &SourceIndex<'_>,
        section: &str,
    ) -> Option<Dependency> {
        let version_str = match value {
            // Simple string version: package = "^1.0"
            Value::String(s) => s.clone(),
            // Inline table: package = {version = "^1.0", optional = true}
            // A table with no `version` key is `{git = ...}`, `{path = ...}` or
            // `{url = ...}`: the version is pinned by the source, not by PyPI.
            // Nothing to check, and nothing to rewrite.
            Value::Table(table) => table.get("version").and_then(Value::as_str)?.to_string(),
            // Multi-constraint form: `pkg = [{version = "^1", python = "<3.9"},
            // {version = "^2", python = ">=3.9"}]`. Each element is a distinct
            // constraint under a distinct marker, which `Dependency` cannot
            // represent - it has one spec and no marker field - so reporting
            // any single element would misstate the file.
            Value::Array(_) => return None,
            _ => return None,
        };

        // A spec the core model cannot represent is preserved as `Complex`
        // rather than making the dependency disappear from the report. Complex
        // is non-rewritable, so it is reported and left alone, which is the
        // honest pair of outcomes.
        let version_spec = if version_str.trim().is_empty() {
            VersionSpec::Any
        } else {
            VersionSpec::parse(&version_str)
                .unwrap_or_else(|_| VersionSpec::Complex(version_str.clone()))
        };

        let (line_number, original_line) = index.locate_key(section, name);

        Some(Dependency {
            name: pep508::normalize_name(name),
            version_spec,
            source_file: path.to_path_buf(),
            line_number,
            original_line,
            manifest_key: None,
            section: Some(section.to_string()),
        })
    }

    /// Parse a PEP 508 requirement string from a dependency array.
    fn parse_dependency_string(
        &self,
        dep_str: &str,
        path: &Path,
        index: &mut SourceIndex<'_>,
        section: &str,
    ) -> Option<Dependency> {
        let req = pep508::parse(dep_str)?;

        if req.name.is_empty() || req.is_direct_reference() {
            // `name @ git+https://...` takes its version from the URL. The old
            // parser produced a package literally named that whole string and
            // sent it to PyPI.
            return None;
        }

        let (line_number, original_line) = index.locate_array_item(dep_str);

        Some(Dependency {
            name: req.normalized_name(),
            version_spec: req.version_spec(),
            source_file: path.to_path_buf(),
            line_number,
            original_line,
            manifest_key: None,
            section: Some(section.to_string()),
        })
    }
}

impl DependencyParser for PyProjectParser {
    fn parse(&self, path: &Path) -> Result<Vec<Dependency>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        let toml_value: Value = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML: {}", path.display()))?;

        let mut index = SourceIndex::new(&content);
        let mut all_dependencies = Vec::new();

        // A file may legitimately carry several of these at once - PEP 621
        // metadata plus a `[tool.pdm]` or `[tool.uv]` block is routine.
        self.parse_pep621_dependencies(&toml_value, path, &mut index, &mut all_dependencies);
        self.parse_poetry_dependencies(&toml_value, path, &mut index, &mut all_dependencies);
        self.parse_pdm_dependencies(&toml_value, path, &mut index, &mut all_dependencies);
        self.parse_uv_dependencies(&toml_value, path, &mut index, &mut all_dependencies);
        self.parse_dependency_groups(&toml_value, path, &mut index, &mut all_dependencies);

        // Deduplicate only *identical* declarations - same name, same section,
        // same spec - which can only arise from two formats describing one
        // entry. The previous rule deduplicated by name across every section,
        // which silently discarded the deliberately different constraints an
        // optional-dependency group carries, and left the survivor pointing at
        // the first occurrence's line, so `-u` updated one of N and left the
        // rest stale. Display-level deduplication already happens in `main.rs`.
        let mut seen = std::collections::HashSet::new();
        all_dependencies.retain(|dep| {
            seen.insert((
                dep.name.clone(),
                dep.section.clone(),
                dep.version_spec.to_string(),
            ))
        });

        Ok(all_dependencies)
    }

    fn can_parse(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "pyproject.toml")
            .unwrap_or(false)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn parse_str(content: &str) -> Vec<Dependency> {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let path = PathBuf::from(file.path());
        PyProjectParser::new().parse(&path).unwrap()
    }

    #[test]
    fn test_can_parse() {
        let parser = PyProjectParser::new();
        assert!(parser.can_parse(&PathBuf::from("pyproject.toml")));
        assert!(parser.can_parse(&PathBuf::from("/path/to/pyproject.toml")));
        assert!(!parser.can_parse(&PathBuf::from("requirements.txt")));
    }

    #[test]
    fn test_parse_pep621_dependencies() {
        let deps = parse_str(
            r#"
[project]
name = "myproject"
dependencies = [
    "requests>=2.28.0",
    "numpy==1.24.0",
    "flask~=2.0.0",
]

[project.optional-dependencies]
dev = [
    "pytest>=7.0.0",
    "black>=22.0.0",
]
"#,
        );

        assert_eq!(deps.len(), 5);
        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "numpy"));
        assert!(deps.iter().any(|d| d.name == "flask"));
        assert!(deps.iter().any(|d| d.name == "pytest"));
        assert!(deps.iter().any(|d| d.name == "black"));

        // Sections survive (they are what scopes a rewrite).
        let requests = deps.iter().find(|d| d.name == "requests").unwrap();
        assert_eq!(requests.section.as_deref(), Some("project.dependencies"));
        let pytest = deps.iter().find(|d| d.name == "pytest").unwrap();
        assert_eq!(
            pytest.section.as_deref(),
            Some("project.optional-dependencies.dev")
        );
    }

    #[test]
    fn test_parse_poetry_dependencies() {
        let deps = parse_str(
            r#"
[tool.poetry]
name = "myproject"

[tool.poetry.dependencies]
python = "^3.8"
requests = "^2.28.0"
numpy = "1.24.0"

[tool.poetry.group.dev.dependencies]
pytest = "^7.0.0"
black = {version = "^22.0.0", optional = true}
"#,
        );

        assert!(!deps.iter().any(|d| d.name == "python"));
        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "numpy"));
        assert!(deps.iter().any(|d| d.name == "pytest"));
        assert!(deps.iter().any(|d| d.name == "black"));

        let requests_dep = deps.iter().find(|d| d.name == "requests").unwrap();
        assert!(matches!(requests_dep.version_spec, VersionSpec::Caret(_)));
        assert_eq!(
            requests_dep.section.as_deref(),
            Some("tool.poetry.dependencies")
        );
        assert_eq!(requests_dep.line_number, Some(7));

        let pytest_dep = deps.iter().find(|d| d.name == "pytest").unwrap();
        assert_eq!(pytest_dep.line_number, Some(11));
        assert_eq!(
            pytest_dep.section.as_deref(),
            Some("tool.poetry.group.dev.dependencies")
        );
    }

    #[test]
    fn test_parse_pdm_dependencies() {
        let deps = parse_str(
            r#"
[project]
name = "myproject"
dependencies = [
    "requests>=2.28.0",
    "numpy==1.24.0",
]

[tool.pdm.dev-dependencies]
test = [
    "pytest>=7.0.0",
]
"#,
        );

        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "numpy"));
        assert!(deps.iter().any(|d| d.name == "pytest"));
    }

    /// `[tool.uv] dev-dependencies` used to be read by nothing at all
    /// while the CLI announced "uv" as the manager.
    #[test]
    fn test_parse_uv_dev_dependencies() {
        let deps = parse_str(
            r#"
[project]
name = "myproject"
dependencies = ["requests>=2.28.0"]

[tool.uv]
dev-dependencies = [
    "pytest>=7.0.0",
    "ruff>=0.1.0",
]
"#,
        );

        let pytest = deps.iter().find(|d| d.name == "pytest").unwrap();
        assert_eq!(pytest.section.as_deref(), Some("tool.uv.dev-dependencies"));
        assert!(deps.iter().any(|d| d.name == "ruff"));
    }

    /// PEP 518 build requirements were never read.
    #[test]
    fn test_parse_build_system_requires() {
        let deps = parse_str(
            r#"
[build-system]
requires = ["setuptools>=68.0.0", "wheel"]
build-backend = "setuptools.build_meta"
"#,
        );

        let setuptools = deps.iter().find(|d| d.name == "setuptools").unwrap();
        assert_eq!(setuptools.section.as_deref(), Some("build-system.requires"));
        assert!(matches!(setuptools.version_spec, VersionSpec::Minimum(_)));
        assert_eq!(setuptools.line_number, Some(3));
    }

    /// The constraint used to be thrown away along with the extras.
    #[test]
    fn test_parse_dependency_with_extras_keeps_the_spec() {
        let deps = parse_str(
            r#"
[project]
dependencies = [
    "requests[security]>=2.28.0",
]
"#,
        );

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
        assert!(matches!(deps[0].version_spec, VersionSpec::Minimum(_)));
        assert!(deps[0].version_spec.is_rewritable());
    }

    /// An operator-list scan names this `django<3.0,`.
    #[test]
    fn test_reversed_range_does_not_corrupt_the_name() {
        let deps = parse_str(
            r#"
[project]
dependencies = [
    "django<3.0,>=2.0",
]
"#,
        );

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "django");
    }

    #[test]
    fn test_parse_dependency_with_markers() {
        let deps = parse_str(
            r#"
[project]
dependencies = [
    "requests>=2.28.0; python_version >= '3.8'",
]
"#,
        );

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
        assert!(matches!(deps[0].version_spec, VersionSpec::Minimum(_)));
    }

    /// The same package in two sections carries two deliberately
    /// different constraints. Both are kept, each pointing at its own line.
    #[test]
    fn test_sections_are_not_collapsed_by_name() {
        let deps = parse_str(
            r#"
[project]
dependencies = [
    "requests>=2.28.0",
]

[project.optional-dependencies]
dev = [
    "requests>=2.30.0",
]
"#,
        );

        let requests: Vec<_> = deps.iter().filter(|d| d.name == "requests").collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].line_number, Some(4));
        assert_eq!(requests[1].line_number, Some(9));
    }

    /// The locator must not match a comment, nor a longer package name
    /// that contains the one we are looking for, nor the project's own `name`
    /// key, and must never fall back to line 1.
    #[test]
    fn test_locator_does_not_match_comments_or_prefixes() {
        let deps = parse_str(
            r#"
[tool.poetry]
name = "requests"

[tool.poetry.dependencies]
# requests = "^1.0.0"
requests-oauthlib = "^1.3.0"
requests = "^2.28.0"
"#,
        );

        let requests = deps
            .iter()
            .find(|d| d.name == "requests" && d.section.is_some())
            .unwrap();
        assert_eq!(requests.line_number, Some(8));

        let oauth = deps.iter().find(|d| d.name == "requests-oauthlib").unwrap();
        assert_eq!(oauth.line_number, Some(7));
    }

    /// An unlocatable declaration carries no line number at all, so
    /// the updater skips it, instead of line 1 plus a fabricated spec string.
    #[test]
    fn test_unlocatable_declaration_is_not_pointed_at_line_one() {
        let deps = parse_str(
            r#"
[project]
dependencies = [ "requests>=2.28.0" ]
[tool.poetry]
dependencies = { numpy = "1.24.0" }
"#,
        );

        // `numpy` is declared in an inline table under a `[tool.poetry]`
        // header, so there is no `[tool.poetry.dependencies]` span and no
        // `numpy = ...` line: the whole-file key scan finds nothing either.
        let numpy = deps.iter().find(|d| d.name == "numpy").unwrap();
        assert_eq!(numpy.line_number, None);
        let requests = deps.iter().find(|d| d.name == "requests").unwrap();
        assert_eq!(requests.line_number, Some(3));
    }

    /// A spec the core model cannot represent must be reported as
    /// `Complex`, not vanish from the output.
    #[test]
    fn test_unmodelled_poetry_spec_survives_as_complex() {
        let deps = parse_str(
            r#"
[tool.poetry.dependencies]
weird = "===1.0+local"
"#,
        );

        let weird = deps.iter().find(|d| d.name == "weird").unwrap();
        assert!(matches!(weird.version_spec, VersionSpec::Complex(_)));
        assert!(!weird.version_spec.is_rewritable());
    }

    /// Git/path/multi-constraint Poetry entries have no PyPI version
    /// to check, and must not be reported with a fabricated one.
    #[test]
    fn test_non_pypi_poetry_sources_are_skipped() {
        let deps = parse_str(
            r#"
[tool.poetry.dependencies]
fromgit = {git = "https://github.com/o/r.git", rev = "abc"}
frompath = {path = "../local"}
multi = [{version = "^1.0", python = "<3.9"}, {version = "^2.0", python = ">=3.9"}]
normal = "^3.0.0"
"#,
        );

        assert!(!deps.iter().any(|d| d.name == "fromgit"));
        assert!(!deps.iter().any(|d| d.name == "frompath"));
        assert!(!deps.iter().any(|d| d.name == "multi"));
        assert!(deps.iter().any(|d| d.name == "normal"));
    }

    #[test]
    fn test_direct_reference_is_not_a_package_name() {
        let deps = parse_str(
            r#"
[project]
dependencies = [
    "mypkg @ git+https://github.com/o/r.git",
    "requests>=2.28.0",
]
"#,
        );

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
    }
}
