use super::pep508;
use super::{Dependency, DependencyParser};
use anyhow::{Context, Result};
use check_updates_core::VersionSpec;
use serde_yaml::Value;
use std::fs;
use std::path::Path;

/// Parser for conda environment.yml files
///
/// Line numbers come from a forward cursor over the raw text (see
/// `YamlCursor`), and a dependency whose line cannot be proven carries
/// `line_number: None`. The previous parser derived `line_number = idx + 2`
/// from the *array index*, ignoring the `name:` and `channels:` blocks above
/// it entirely - in the parser's own test fixture the first dependency sits on
/// file line 6 and was reported as line 2. `pcu/src/updater.rs` indexes
/// `lines[line_number - 1]`, so that was a licence to rewrite arbitrary lines
/// of the file.
pub struct CondaParser;

impl Default for CondaParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Walks the raw YAML text in step with the parsed sequence, handing out the
/// real line number of each list item.
///
/// `serde_yaml::Value` carries no spans, but a sequence is emitted in document
/// order, so a forward-only cursor over the text lines matches items to lines
/// exactly - including repeated items, which a whole-file search would collapse
/// onto the first occurrence.
struct YamlCursor<'a> {
    lines: Vec<&'a str>,
    cursor: usize,
}

impl<'a> YamlCursor<'a> {
    fn new(content: &'a str) -> Self {
        Self {
            lines: content.lines().collect(),
            cursor: 0,
        }
    }

    /// Advance to the next list item whose scalar value is `item`.
    fn next_item(&mut self, item: &str) -> (Option<usize>, String) {
        self.advance(|body| body == item)
    }

    /// Advance to the next list item that opens the mapping `key:`.
    fn next_mapping_key(&mut self, key: &str) -> (Option<usize>, String) {
        let wanted = format!("{key}:");
        self.advance(|body| body == wanted)
    }

    fn advance<F: Fn(&str) -> bool>(&mut self, matches: F) -> (Option<usize>, String) {
        for idx in self.cursor..self.lines.len() {
            let line = self.lines[idx];
            let Some(body) = list_item_body(line) else {
                continue;
            };
            if matches(body) {
                self.cursor = idx + 1;
                return (Some(idx + 1), line.trim_end().to_string());
            }
        }

        (None, String::new())
    }
}

/// The scalar text of a YAML list item line, with quotes and an inline comment
/// removed. `None` when the line is not a list item.
fn list_item_body(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let rest =
        trimmed
            .strip_prefix("- ")
            .or_else(|| if trimmed == "-" { Some("") } else { None })?;

    let rest = strip_yaml_comment(rest).trim();
    let rest = rest
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(rest);

    Some(rest)
}

/// YAML starts a comment at a `#` preceded by whitespace (or at line start).
fn strip_yaml_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'#' && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
            return &s[..i];
        }
    }
    s
}

impl CondaParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse one conda MatchSpec.
    ///
    /// Conda's grammar is not pip's, and treating it as pip's is how a hard pin
    /// turned into "no constraint": the old code searched for `=` *after* the
    /// two-character operators had failed, so `numpy==1.24.0` hit the first `=`
    /// of `==`, built the nonsense string `"===1.24.0"`, failed to parse it,
    /// and fell through to `VersionSpec::Any`.
    ///
    /// What is modelled here:
    ///
    /// - `channel::name` and `channel/subdir::name` - the channel is stripped
    ///   from the name rather than being queried as part of it.
    /// - `name==1.24.0` - an exact pin, `Pinned`.
    /// - `name=1.24.0` - conda's `=` is a *prefix* match, but a prefix that is
    ///   already a full three-segment release can only match builds of that
    ///   one release, so it is `Pinned` too. That keeps it updatable: the
    ///   updater writes it back in the same `name=X.Y.Z` form (see
    ///   `replace_in_conda`), and `Wildcard` has no base version, which would
    ///   have made every `=`-pinned conda dependency silently unupdatable.
    /// - `name=1.24` - a genuine prefix (fewer than three segments) maps to
    ///   `Wildcard` (`==1.24.*`). Mapping it to `Pinned` made `-u` willing to
    ///   rewrite a prefix constraint as an exact one, which narrows the
    ///   project's dependency without being asked.
    /// - `>=`, `<=`, `!=`, `>`, `<` with the leftmost operator winning.
    /// - `name=1.24.0=py39h1234` (build string) and `name 1.24.0 py39_0`
    ///   (space-separated MatchSpec) - kept as `Complex`, which reports them
    ///   verbatim and refuses to rewrite them. The build string is part of the
    ///   constraint and `VersionSpec` has nowhere to put it, so any rewrite
    ///   would drop it.
    ///
    /// Every failure preserves the source text as `Complex`. The old code
    /// collapsed every failure to `Any`, which claims the file stated no
    /// constraint at all - a lie about the input, and a rewritable one.
    fn parse_conda_dependency(dep_str: &str) -> Option<(String, VersionSpec)> {
        let dep_str = strip_yaml_comment(dep_str).trim();
        if dep_str.is_empty() || dep_str.starts_with('#') {
            return None;
        }

        // Drop any `channel::` or `channel/subdir::` qualifier.
        let spec = match dep_str.rfind("::") {
            Some(idx) => dep_str[idx + 2..].trim(),
            None => dep_str,
        };
        if spec.is_empty() {
            return None;
        }

        // Space-separated MatchSpec: `numpy 1.24.0 py39_0`.
        if spec.split_whitespace().count() > 1 {
            let name = spec.split_whitespace().next()?.to_lowercase();
            return Some((name, VersionSpec::Complex(spec.to_string())));
        }

        // Bracketed MatchSpec: `numpy[version='>=1.24',build=py39*]`.
        if let Some(idx) = spec.find('[') {
            let name = spec[..idx].trim().to_lowercase();
            if name.is_empty() {
                return None;
            }
            return Some((name, VersionSpec::Complex(spec[idx..].to_string())));
        }

        let Some((op_idx, op)) = leftmost_operator(spec) else {
            return Some((spec.to_lowercase(), VersionSpec::Any));
        };

        let name = spec[..op_idx].trim().to_lowercase();
        if name.is_empty() {
            return None;
        }
        let rest = spec[op_idx + op.len()..].trim();
        if rest.is_empty() {
            return Some((name, VersionSpec::Complex(spec[op_idx..].to_string())));
        }

        let spec_text = &spec[op_idx..];

        let version_spec = match op {
            // `=` is a prefix match unless a build string follows.
            "=" => {
                if rest.contains('=') {
                    // `name=1.24.0=py39h1234`
                    VersionSpec::Complex(spec_text.to_string())
                } else if rest.contains('*') {
                    parse_or_complex(&format!("=={rest}"), spec_text)
                } else if is_full_release(rest) {
                    // `name=1.24.0`: a prefix on a complete release is a pin.
                    parse_or_complex(&format!("=={rest}"), spec_text)
                } else {
                    parse_or_complex(&format!("=={rest}.*"), spec_text)
                }
            }
            _ => parse_or_complex(&format!("{op}{rest}"), spec_text),
        };

        Some((name, version_spec))
    }

    /// Parse a pip requirement from the `pip:` section.
    ///
    /// These really are PEP 508 requirements, so they go through the shared
    /// parser - which also means names here are normalized the same way as
    /// everywhere else in pcu. They previously were not, so `typing_extensions`
    /// in a conda pip section produced a key that could never match the
    /// `typing-extensions` a lock file or a requirements.txt produced.
    fn parse_pip_dependency(dep_str: &str) -> Option<(String, VersionSpec)> {
        let req = pep508::parse(strip_yaml_comment(dep_str))?;
        if req.name.is_empty() || req.is_direct_reference() {
            return None;
        }
        Some((req.normalized_name(), req.version_spec()))
    }
}

/// True for `X.Y.Z` (three or more purely numeric segments): a version that a
/// conda `=` prefix can only match by build string, never by a later release.
fn is_full_release(version: &str) -> bool {
    let segments: Vec<&str> = version.split('.').collect();
    segments.len() >= 3
        && segments
            .iter()
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

/// Parse `s`, or keep `raw` verbatim as a non-rewritable `Complex`.
fn parse_or_complex(s: &str, raw: &str) -> VersionSpec {
    VersionSpec::parse(s).unwrap_or_else(|_| VersionSpec::Complex(raw.to_string()))
}

/// The leftmost version operator in a MatchSpec, preferring the longer operator
/// at the same position so `==` is never read as `=`.
fn leftmost_operator(spec: &str) -> Option<(usize, &'static str)> {
    const OPERATORS: [&str; 7] = ["==", ">=", "<=", "!=", ">", "<", "="];

    let mut best: Option<(usize, &'static str)> = None;
    for op in OPERATORS {
        let Some(idx) = spec.find(op) else {
            continue;
        };
        let better = match best {
            None => true,
            Some((best_idx, best_op)) => {
                idx < best_idx || (idx == best_idx && op.len() > best_op.len())
            }
        };
        if better {
            best = Some((idx, op));
        }
    }
    best
}

impl DependencyParser for CondaParser {
    fn parse(&self, path: &Path) -> Result<Vec<Dependency>> {
        let content =
            fs::read_to_string(path).context(format!("Failed to read file: {}", path.display()))?;

        let yaml: Value = serde_yaml::from_str(&content)
            .context(format!("Failed to parse YAML: {}", path.display()))?;

        let mut dependencies = Vec::new();
        let mut cursor = YamlCursor::new(&content);

        if let Some(deps) = yaml.get("dependencies").and_then(|v| v.as_sequence()) {
            for dep in deps {
                if let Some(dep_str) = dep.as_str() {
                    let (line_number, original_line) = cursor.next_item(dep_str);
                    if let Some((name, version_spec)) = Self::parse_conda_dependency(dep_str) {
                        dependencies.push(Dependency {
                            name,
                            version_spec,
                            source_file: path.to_path_buf(),
                            line_number,
                            original_line,
                            manifest_key: None,
                            // Conda packages come from conda channels, pip
                            // packages from PyPI; the section records which so a
                            // caller can tell them apart.
                            section: Some("dependencies".to_string()),
                        });
                    }
                } else if let Some(pip_section) = dep.as_mapping()
                    && let Some(pip_deps) = pip_section.get("pip").and_then(|v| v.as_sequence())
                {
                    cursor.next_mapping_key("pip");
                    for pip_dep in pip_deps {
                        if let Some(pip_dep_str) = pip_dep.as_str() {
                            let (line_number, original_line) = cursor.next_item(pip_dep_str);
                            if let Some((name, version_spec)) =
                                Self::parse_pip_dependency(pip_dep_str)
                            {
                                dependencies.push(Dependency {
                                    name,
                                    version_spec,
                                    source_file: path.to_path_buf(),
                                    line_number,
                                    original_line,
                                    manifest_key: None,
                                    section: Some("dependencies.pip".to_string()),
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(dependencies)
    }

    fn can_parse(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "environment.yml" || n == "environment.yaml")
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

    #[test]
    fn test_can_parse() {
        let parser = CondaParser::new();
        assert!(parser.can_parse(&PathBuf::from("environment.yml")));
        assert!(parser.can_parse(&PathBuf::from("environment.yaml")));
        assert!(!parser.can_parse(&PathBuf::from("requirements.txt")));
        assert!(!parser.can_parse(&PathBuf::from("pyproject.toml")));
    }

    #[test]
    fn test_parse_conda_dependency() {
        let (name, spec) = CondaParser::parse_conda_dependency("numpy").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Any));

        // Conda `=` on a full release can only ever match that release: a pin,
        // and one the updater knows how to write back as `numpy=1.26.0`.
        let (name, spec) = CondaParser::parse_conda_dependency("numpy=1.24.0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Pinned(_)));

        // Conda `=` on a partial version is a prefix match: a wildcard, not a
        // pin, so `-u` never narrows it to an exact version.
        let (name, spec) = CondaParser::parse_conda_dependency("numpy=1.24").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Wildcard { .. }));

        let (name, spec) = CondaParser::parse_conda_dependency("numpy>=1.24.0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Minimum(_)));

        let (name, spec) = CondaParser::parse_conda_dependency("python=3.9.*").unwrap();
        assert_eq!(name, "python");
        assert!(matches!(spec, VersionSpec::Wildcard { .. }));
    }

    /// A hard pin reported as
    /// "no constraint".
    #[test]
    fn test_double_equals_is_a_pin_not_any() {
        let (name, spec) = CondaParser::parse_conda_dependency("numpy==1.24.0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Pinned(_)));
    }

    /// The channel qualifier is not part of the package name.
    #[test]
    fn test_channel_qualifier_is_stripped() {
        let (name, spec) = CondaParser::parse_conda_dependency("conda-forge::numpy=1.24").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Wildcard { .. }));

        let (name, _) =
            CondaParser::parse_conda_dependency("conda-forge/linux-64::numpy>=1.24").unwrap();
        assert_eq!(name, "numpy");
    }

    /// A build string is part of the constraint and cannot be dropped,
    /// so the spec stays verbatim and non-rewritable.
    #[test]
    fn test_build_strings_are_preserved_not_rewritable() {
        let (name, spec) = CondaParser::parse_conda_dependency("numpy=1.24.0=py39h1234").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Complex(_)));
        assert!(!spec.is_rewritable());
    }

    /// Space-separated MatchSpec used to make the whole string the
    /// package name.
    #[test]
    fn test_space_separated_matchspec() {
        let (name, spec) = CondaParser::parse_conda_dependency("numpy 1.24.0 py39_0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Complex(_)));
    }

    #[test]
    fn test_parse_pip_dependency() {
        let (name, spec) = CondaParser::parse_pip_dependency("requests").unwrap();
        assert_eq!(name, "requests");
        assert!(matches!(spec, VersionSpec::Any));

        let (name, spec) = CondaParser::parse_pip_dependency("requests==2.28.0").unwrap();
        assert_eq!(name, "requests");
        assert!(matches!(spec, VersionSpec::Pinned(_)));

        let (name, spec) = CondaParser::parse_pip_dependency("numpy>=1.24.0,<2.0.0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Range { .. }));

        let (name, spec) = CondaParser::parse_pip_dependency("flask~=2.0.0").unwrap();
        assert_eq!(name, "flask");
        assert!(matches!(spec, VersionSpec::Compatible(_)));
    }

    /// Pip-section names must normalize like every other pip source.
    #[test]
    fn test_pip_names_are_normalized() {
        let (name, _) = CondaParser::parse_pip_dependency("typing_extensions>=4.0").unwrap();
        assert_eq!(name, "typing-extensions");
    }

    /// A reversed range keeps its name, via the shared PEP 508 parser: an
    /// operator-list scan would have named this package `django<3.0,`.
    #[test]
    fn test_pip_reversed_range_keeps_the_name() {
        let (name, _) = CondaParser::parse_pip_dependency("pkg<3.0,>=2.0").unwrap();
        assert_eq!(name, "pkg");
    }

    #[test]
    fn test_parse_environment_yml() {
        let yaml_content = r#"
name: myenv
channels:
  - conda-forge
  - defaults
dependencies:
  - python=3.9.*
  - numpy=1.24.0
  - pandas>=1.5.0
  - scikit-learn
  - pip:
    - requests==2.28.0
    - flask>=2.0.0,<3.0.0
    - django
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "{yaml_content}").unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = CondaParser::new();
        let dependencies = parser.parse(&path).unwrap();

        assert_eq!(dependencies.len(), 7);

        let python_dep = dependencies.iter().find(|d| d.name == "python").unwrap();
        assert!(matches!(
            python_dep.version_spec,
            VersionSpec::Wildcard { .. }
        ));

        let numpy_dep = dependencies.iter().find(|d| d.name == "numpy").unwrap();
        assert!(matches!(numpy_dep.version_spec, VersionSpec::Pinned(_)));

        let pandas_dep = dependencies.iter().find(|d| d.name == "pandas").unwrap();
        assert!(matches!(pandas_dep.version_spec, VersionSpec::Minimum(_)));

        let sklearn_dep = dependencies
            .iter()
            .find(|d| d.name == "scikit-learn")
            .unwrap();
        assert!(matches!(sklearn_dep.version_spec, VersionSpec::Any));

        let requests_dep = dependencies.iter().find(|d| d.name == "requests").unwrap();
        assert!(matches!(requests_dep.version_spec, VersionSpec::Pinned(_)));

        let flask_dep = dependencies.iter().find(|d| d.name == "flask").unwrap();
        assert!(matches!(flask_dep.version_spec, VersionSpec::Range { .. }));

        let django_dep = dependencies.iter().find(|d| d.name == "django").unwrap();
        assert!(matches!(django_dep.version_spec, VersionSpec::Any));
    }

    /// The fixture's `channels:` block used to be invisible to the
    /// line numbering, so the first dependency - on file line 7 - was reported
    /// as line 2.
    #[test]
    fn test_line_numbers_are_real_file_lines() {
        let yaml_content = r#"name: myenv
channels:
  - conda-forge
  - defaults
dependencies:
  - python=3.9.*
  - numpy=1.24.0
  - pip:
    - requests==2.28.0
    - flask>=2.0.0
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "{yaml_content}").unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = CondaParser::new();
        let dependencies = parser.parse(&path).unwrap();

        let by_name = |n: &str| {
            dependencies
                .iter()
                .find(|d| d.name == n)
                .unwrap()
                .line_number
        };

        assert_eq!(by_name("python"), Some(6));
        assert_eq!(by_name("numpy"), Some(7));
        assert_eq!(by_name("requests"), Some(9));
        assert_eq!(by_name("flask"), Some(10));

        // And the channel entries are not mistaken for dependencies.
        assert!(!dependencies.iter().any(|d| d.name == "conda-forge"));
    }

    #[test]
    fn test_parse_environment_yaml() {
        let yaml_content = r#"
dependencies:
  - numpy=1.24.0
"#;

        let dir = tempfile::TempDir::new().unwrap();
        let yaml_path = dir.path().join("environment.yaml");
        std::fs::write(&yaml_path, yaml_content).unwrap();

        let parser = CondaParser::new();
        assert!(parser.can_parse(&yaml_path));

        let dependencies = parser.parse(&yaml_path).unwrap();
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].name, "numpy");
        assert_eq!(dependencies[0].line_number, Some(3));
    }

    #[test]
    fn test_empty_dependencies() {
        let yaml_content = r#"
name: myenv
dependencies: []
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "{yaml_content}").unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = CondaParser::new();
        let dependencies = parser.parse(&path).unwrap();

        assert_eq!(dependencies.len(), 0);
    }
}
