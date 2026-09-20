use super::pep508;
use super::{Dependency, DependencyParser};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// How many levels of `-r` / `-c` includes to follow before giving up. Deep
/// requirement trees are rare; a cycle is not, and the visited set already
/// catches those, so this is a belt-and-braces bound on pathological input.
const MAX_INCLUDE_DEPTH: usize = 16;

/// Parser for requirements.txt files
pub struct RequirementsParser;

impl Default for RequirementsParser {
    fn default() -> Self {
        Self::new()
    }
}

/// One logical requirement line: physical line-continuations (`\` at end of
/// line) joined into a single string, tagged with the 1-indexed line number the
/// logical line *started* on.
struct LogicalLine {
    line_number: usize,
    text: String,
    /// The first physical line, verbatim, for display and JSON.
    raw: String,
}

impl RequirementsParser {
    pub fn new() -> Self {
        Self
    }

    /// Read one requirements file and append its dependencies, following
    /// `-r` / `-c` includes.
    fn parse_into(
        &self,
        path: &Path,
        visited: &mut HashSet<PathBuf>,
        out: &mut Vec<Dependency>,
        depth: usize,
    ) -> Result<()> {
        // Canonicalize so `a.txt` and `./a.txt` are the same node, and so a
        // mutual include (`base.txt` -> `dev.txt` -> `base.txt`) terminates.
        let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !visited.insert(key) {
            return Ok(());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read requirements file: {path:?}"))?;

        for logical in logical_lines(&content) {
            let body = strip_comment(&logical.text).trim();
            if body.is_empty() {
                continue;
            }

            if let Some(include) = include_target(body) {
                if depth >= MAX_INCLUDE_DEPTH {
                    continue;
                }
                let base = path.parent().unwrap_or_else(|| Path::new("."));
                let included = base.join(include);
                if included.is_file() {
                    self.parse_into(&included, visited, out, depth + 1)?;
                }
                continue;
            }

            if body.starts_with('-') {
                // Every remaining option line is deliberately not a PyPI
                // requirement:
                //
                // - `-e .` / `-e git+...` install from a path or a VCS ref, so
                //   there is no index version to compare against. Resolving
                //   the `#egg=` name against PyPI would report updates for a
                //   package the project is explicitly *not* taking from PyPI.
                // - `--index-url`, `--extra-index-url`, `--find-links`,
                //   `--trusted-host`, `--no-binary`, `--pre` and friends
                //   configure pip, they do not name a dependency.
                //
                // They are skipped on purpose rather than by a blanket
                // `starts_with('-')` that also ate the include directives
                // handled above.
                continue;
            }

            // `--hash=sha256:...` arrives as a continuation of the requirement
            // itself once physical lines are joined; strip it before parsing so
            // it is not mistaken for trailing garbage.
            let body = strip_hashes(body);
            let body = body.trim();
            if body.is_empty() {
                continue;
            }

            let Some(req) = pep508::parse(body) else {
                continue;
            };

            if req.name.is_empty() || req.is_direct_reference() {
                // A bare URL or `name @ url`: the version is pinned by the URL,
                // not by the index. Reporting it as a PyPI dependency would
                // offer an update that means nothing here, and the old parser's
                // habit of turning the whole string into a "package name" sent
                // that string to PyPI as a query.
                continue;
            }

            // Two entries for the same distribution under different markers
            // (`pkg==1.0; python_version<'3.8'` and `pkg==2.0` otherwise) are
            // distinct requirements on distinct lines. Both are kept so each
            // can be reported and rewritten where it actually lives.
            out.push(Dependency {
                name: req.normalized_name(),
                version_spec: req.version_spec(),
                source_file: path.to_path_buf(),
                // The line a logical requirement *starts* on. When a `\`
                // continuation puts the specifier on a later physical line the
                // updater will not find it here and declines to write, which is
                // the correct outcome for a line-based rewriter.
                line_number: Some(logical.line_number),
                original_line: logical.raw.clone(),
                manifest_key: None,
                // requirements.txt is a flat list; there is no section to scope to.
                section: None,
            });
        }

        Ok(())
    }
}

/// Join physical lines ending in `\` into logical lines.
fn logical_lines(content: &str) -> Vec<LogicalLine> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start = 0usize;
    let mut raw = String::new();

    for (idx, physical) in content.lines().enumerate() {
        if buf.is_empty() {
            start = idx + 1;
            raw = physical.to_string();
        }

        let trimmed = physical.trim_end();
        if let Some(head) = trimmed.strip_suffix('\\') {
            buf.push_str(head);
            buf.push(' ');
            continue;
        }

        buf.push_str(trimmed);
        out.push(LogicalLine {
            line_number: start,
            text: std::mem::take(&mut buf),
            raw: std::mem::take(&mut raw),
        });
    }

    if !buf.is_empty() {
        out.push(LogicalLine {
            line_number: start,
            text: buf,
            raw,
        });
    }

    out
}

/// Strip an inline comment.
///
/// PEP 508 requires whitespace before a `#` for it to start a comment, and pip
/// follows the same rule. Cutting at the first `#` anywhere truncates the
/// `#egg=` and `#sha256=` fragments that are a legitimate part of a URL.
fn strip_comment(line: &str) -> &str {
    if line.trim_start().starts_with('#') {
        return "";
    }

    let bytes = line.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'#' && i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
            return &line[..i];
        }
    }

    line
}

/// Remove `--hash=...` tokens from a joined requirement line.
fn strip_hashes(line: &str) -> String {
    line.split_whitespace()
        .filter(|token| !token.starts_with("--hash"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The file named by a `-r` / `--requirement` / `-c` / `--constraint`
/// directive, if this line is one.
fn include_target(line: &str) -> Option<&str> {
    for flag in ["--requirement", "--constraint"] {
        if let Some(rest) = line.strip_prefix(flag) {
            let rest = rest.trim_start_matches('=').trim();
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }

    for flag in ["-r", "-c"] {
        if let Some(rest) = line.strip_prefix(flag) {
            // pip accepts both `-r file` and `-rfile`, but `-c` must not match
            // a long option such as `--constraint` (already handled) and `-r`
            // must not match, say, a hypothetical `-require`. Requiring the
            // remainder to be either whitespace-separated or a plain path is
            // enough in practice.
            let rest = rest.trim();
            if !rest.is_empty() && !rest.starts_with('-') {
                return Some(rest);
            }
        }
    }

    None
}

impl DependencyParser for RequirementsParser {
    fn parse(&self, path: &Path) -> Result<Vec<Dependency>> {
        let mut visited = HashSet::new();
        let mut out = Vec::new();
        self.parse_into(path, &mut visited, &mut out, 0)?;
        Ok(out)
    }

    fn can_parse(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("requirements") && n.ends_with(".txt"))
            .unwrap_or(false)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use check_updates_core::VersionSpec;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn test_parse_simple_package() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "requests==2.28.0").unwrap();
        writeln!(file, "numpy>=1.24.0").unwrap();
        writeln!(file, "flask").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "requests");
        assert!(matches!(deps[0].version_spec, VersionSpec::Pinned(_)));
        assert_eq!(deps[1].name, "numpy");
        assert!(matches!(deps[1].version_spec, VersionSpec::Minimum(_)));
        assert_eq!(deps[2].name, "flask");
        assert!(matches!(deps[2].version_spec, VersionSpec::Any));
    }

    #[test]
    fn test_parse_with_extras() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "requests[security]>=2.0.0").unwrap();
        writeln!(file, "celery[redis,msgpack]==5.2.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "requests");
        assert!(matches!(deps[0].version_spec, VersionSpec::Minimum(_)));
        assert_eq!(deps[1].name, "celery");
        assert!(matches!(deps[1].version_spec, VersionSpec::Pinned(_)));
    }

    #[test]
    fn test_parse_with_comments() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "# This is a comment").unwrap();
        writeln!(file, "requests==2.28.0  # inline comment").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "numpy>=1.24.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "requests");
        assert_eq!(deps[1].name, "numpy");
    }

    #[test]
    fn test_parse_with_environment_markers() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "dataclasses>=0.6; python_version < '3.7'").unwrap();
        writeln!(file, "typing-extensions>=3.7; python_version >= '3.8'").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "dataclasses");
        assert_eq!(deps[1].name, "typing-extensions");
    }

    #[test]
    fn test_parse_skip_directives() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "--index-url https://pypi.org/simple").unwrap();
        writeln!(file, "-e .").unwrap();
        writeln!(file, "--find-links ./wheels").unwrap();
        writeln!(file, "requests==2.28.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
    }

    #[test]
    fn test_parse_complex_version_specs() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "django>=2.0,<3.0").unwrap();
        writeln!(file, "pytest~=7.0").unwrap();
        writeln!(file, "click!=8.0.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "django");
        assert!(matches!(deps[0].version_spec, VersionSpec::Range { .. }));
        assert_eq!(deps[1].name, "pytest");
        assert_eq!(deps[2].name, "click");
    }

    /// An operator-list scan names this package `django<3.0,`.
    #[test]
    fn test_reversed_range_does_not_corrupt_the_name() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "django<3.0,>=2.0").unwrap();
        writeln!(file, "pkg>1.0,<=2.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "django");
        assert_eq!(deps[1].name, "pkg");
    }

    #[test]
    fn test_line_numbers() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "# Comment line").unwrap();
        writeln!(file, "requests==2.28.0").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "numpy>=1.24.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].line_number, Some(2));
        assert_eq!(deps[1].line_number, Some(4));
    }

    /// A `\` continuation used to leave the backslash inside the
    /// version string. The joined line parses, and the recorded line number is
    /// where the requirement starts.
    #[test]
    fn test_line_continuations_are_joined() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "requests==2.28.0 \\").unwrap();
        writeln!(file, "    --hash=sha256:abc \\").unwrap();
        writeln!(file, "    --hash=sha256:def").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
        assert_eq!(deps[0].line_number, Some(1));
        assert!(matches!(deps[0].version_spec, VersionSpec::Pinned(_)));
    }

    /// `#egg=` fragments used to be truncated by a first-`#` cut.
    #[test]
    fn test_url_requirements_are_not_turned_into_package_names() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "https://example.com/pkg-1.0.whl").unwrap();
        writeln!(file, "mypkg @ git+https://github.com/o/r.git#egg=mypkg").unwrap();
        writeln!(file, "requests==2.28.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(file.path()).unwrap();

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
    }

    /// `-r` includes are followed, and each dependency keeps the file
    /// it actually lives in so the updater rewrites the right one.
    #[test]
    fn test_includes_are_followed() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("base.txt"), "requests==2.28.0\n").unwrap();
        fs::write(
            dir.path().join("requirements.txt"),
            "-r base.txt\nnumpy>=1.24.0\n",
        )
        .unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(&dir.path().join("requirements.txt")).unwrap();

        assert_eq!(deps.len(), 2);
        let requests = deps.iter().find(|d| d.name == "requests").unwrap();
        assert!(requests.source_file.ends_with("base.txt"));
        assert_eq!(requests.line_number, Some(1));
        let numpy = deps.iter().find(|d| d.name == "numpy").unwrap();
        assert!(numpy.source_file.ends_with("requirements.txt"));
        assert_eq!(numpy.line_number, Some(2));
    }

    #[test]
    fn test_include_cycles_terminate() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "-r b.txt\nrequests==2.28.0\n").unwrap();
        fs::write(dir.path().join("b.txt"), "-r a.txt\nnumpy>=1.24.0\n").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(&dir.path().join("a.txt")).unwrap();

        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn test_can_parse() {
        let parser = RequirementsParser::new();
        assert!(parser.can_parse(&PathBuf::from("requirements.txt")));
        assert!(parser.can_parse(&PathBuf::from("requirements-dev.txt")));
        assert!(parser.can_parse(&PathBuf::from("requirements-test.txt")));
        assert!(!parser.can_parse(&PathBuf::from("pyproject.toml")));
        assert!(!parser.can_parse(&PathBuf::from("setup.py")));
    }
}
