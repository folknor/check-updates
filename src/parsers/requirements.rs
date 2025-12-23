use super::{Dependency, DependencyParser};
use crate::version::VersionSpec;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Parser for requirements.txt files
pub struct RequirementsParser;

impl RequirementsParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse a single line from requirements.txt
    fn parse_line(line: &str, line_number: usize, source_file: &PathBuf) -> Option<Dependency> {
        let original_line = line.to_string();
        let line = line.trim();

        // Skip empty lines
        if line.is_empty() {
            return None;
        }

        // Skip comments
        if line.starts_with('#') {
            return None;
        }

        // Skip -r includes (don't follow them)
        if line.starts_with("-r ") || line.starts_with("-r\t") {
            return None;
        }

        // Skip --index-url and other pip options
        if line.starts_with("--") || line.starts_with('-') {
            return None;
        }

        // Handle environment markers - split on semicolon
        // e.g., "package>=1.0; python_version >= '3.8'"
        let line_without_marker = if let Some(idx) = line.find(';') {
            line[..idx].trim()
        } else {
            line
        };

        // Handle inline comments
        let line_clean = if let Some(idx) = line_without_marker.find('#') {
            line_without_marker[..idx].trim()
        } else {
            line_without_marker
        };

        if line_clean.is_empty() {
            return None;
        }

        // Parse package name and version specifier
        // Package name can include extras: package[extra1,extra2]>=1.0
        let (package_with_extras, version_str) = Self::split_package_version(line_clean)?;

        // Extract package name (remove extras)
        let package_name = if let Some(bracket_idx) = package_with_extras.find('[') {
            package_with_extras[..bracket_idx].trim()
        } else {
            package_with_extras
        };

        // Normalize package name to lowercase with underscores/hyphens handled
        let normalized_name = package_name.to_lowercase().replace('_', "-");

        // Parse version specification
        let version_spec = if version_str.is_empty() {
            VersionSpec::Any
        } else {
            match VersionSpec::parse(version_str) {
                Ok(spec) => spec,
                Err(_) => {
                    // If parsing fails, store as complex constraint
                    VersionSpec::Complex(version_str.to_string())
                }
            }
        };

        Some(Dependency {
            name: normalized_name,
            version_spec,
            source_file: source_file.clone(),
            line_number,
            original_line,
        })
    }

    /// Split a package specification into name (with extras) and version
    /// Returns (package_with_extras, version_spec)
    fn split_package_version(spec: &str) -> Option<(&str, &str)> {
        // Try to find version operators
        // Order matters: check two-char operators first
        let operators = ["==", ">=", "<=", "~=", "!=", ">", "<"];

        // Find the first operator
        let mut first_op_idx = None;
        for op in &operators {
            if let Some(idx) = spec.find(op) {
                // Make sure we're not inside brackets (extras)
                let before = &spec[..idx];
                let open_brackets = before.matches('[').count();
                let close_brackets = before.matches(']').count();

                // Only consider this operator if we're not inside brackets
                if open_brackets == close_brackets {
                    first_op_idx = Some(idx);
                    break;
                }
            }
        }

        if let Some(idx) = first_op_idx {
            let package = spec[..idx].trim();
            let version = spec[idx..].trim();
            Some((package, version))
        } else {
            // No version specifier - just package name
            Some((spec.trim(), ""))
        }
    }
}

impl DependencyParser for RequirementsParser {
    fn parse(&self, path: &PathBuf) -> Result<Vec<Dependency>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read requirements file: {:?}", path))?;

        let dependencies: Vec<Dependency> = content
            .lines()
            .enumerate()
            .filter_map(|(idx, line)| {
                // Line numbers are 1-indexed
                Self::parse_line(line, idx + 1, path)
            })
            .collect();

        Ok(dependencies)
    }

    fn can_parse(&self, path: &PathBuf) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("requirements") && n.ends_with(".txt"))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_simple_package() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "requests==2.28.0").unwrap();
        writeln!(file, "numpy>=1.24.0").unwrap();
        writeln!(file, "flask").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(&file.path().to_path_buf()).unwrap();

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
        let deps = parser.parse(&file.path().to_path_buf()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "requests");
        assert_eq!(deps[1].name, "celery");
    }

    #[test]
    fn test_parse_with_comments() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "# This is a comment").unwrap();
        writeln!(file, "requests==2.28.0  # inline comment").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "numpy>=1.24.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(&file.path().to_path_buf()).unwrap();

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
        let deps = parser.parse(&file.path().to_path_buf()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "dataclasses");
        assert_eq!(deps[1].name, "typing-extensions");
    }

    #[test]
    fn test_parse_skip_directives() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "--index-url https://pypi.org/simple").unwrap();
        writeln!(file, "-r requirements-dev.txt").unwrap();
        writeln!(file, "requests==2.28.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(&file.path().to_path_buf()).unwrap();

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
        let deps = parser.parse(&file.path().to_path_buf()).unwrap();

        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "django");
        assert!(matches!(deps[0].version_spec, VersionSpec::Range { .. }));
        assert_eq!(deps[1].name, "pytest");
        assert_eq!(deps[2].name, "click");
    }

    #[test]
    fn test_line_numbers() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "# Comment line").unwrap();
        writeln!(file, "requests==2.28.0").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "numpy>=1.24.0").unwrap();

        let parser = RequirementsParser::new();
        let deps = parser.parse(&file.path().to_path_buf()).unwrap();

        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].line_number, 2);
        assert_eq!(deps[1].line_number, 4);
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
