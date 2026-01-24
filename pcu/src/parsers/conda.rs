use super::{Dependency, DependencyParser};
use check_updates_core::VersionSpec;
use anyhow::{Context, Result};
use serde_yaml::Value;
use std::fs;
use std::path::PathBuf;

/// Parser for conda environment.yml files
pub struct CondaParser;

impl CondaParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse a single dependency string from conda format
    /// Examples:
    /// - "numpy" -> (numpy, Any)
    /// - "numpy=1.24.0" -> (numpy, ==1.24.0)
    /// - "numpy>=1.24.0" -> (numpy, >=1.24.0)
    /// - "python=3.9.*" -> (python, ==3.9.*)
    fn parse_conda_dependency(dep_str: &str) -> Option<(String, VersionSpec)> {
        let dep_str = dep_str.trim();

        // Skip empty strings or comments
        if dep_str.is_empty() || dep_str.starts_with('#') {
            return None;
        }

        // Conda uses = for exact version, >= for minimum, etc.
        // Examples: numpy=1.24.0, numpy>=1.24, numpy, python=3.9.*

        // Check for version operators (order matters - check >= before =)
        if let Some(idx) = dep_str.find(">=") {
            let name = dep_str[..idx].trim().to_lowercase();
            let version_str = dep_str[idx + 2..].trim();
            return match VersionSpec::parse(&format!(">={}", version_str)) {
                Ok(spec) => Some((name, spec)),
                Err(_) => Some((name, VersionSpec::Any)),
            };
        }

        if let Some(idx) = dep_str.find("<=") {
            let name = dep_str[..idx].trim().to_lowercase();
            let version_str = dep_str[idx + 2..].trim();
            return match VersionSpec::parse(&format!("<={}", version_str)) {
                Ok(spec) => Some((name, spec)),
                Err(_) => Some((name, VersionSpec::Any)),
            };
        }

        if let Some(idx) = dep_str.find("!=") {
            let name = dep_str[..idx].trim().to_lowercase();
            let version_str = dep_str[idx + 2..].trim();
            return match VersionSpec::parse(&format!("!={}", version_str)) {
                Ok(spec) => Some((name, spec)),
                Err(_) => Some((name, VersionSpec::Any)),
            };
        }

        if let Some(idx) = dep_str.find('>') {
            let name = dep_str[..idx].trim().to_lowercase();
            let version_str = dep_str[idx + 1..].trim();
            return match VersionSpec::parse(&format!(">{}", version_str)) {
                Ok(spec) => Some((name, spec)),
                Err(_) => Some((name, VersionSpec::Any)),
            };
        }

        if let Some(idx) = dep_str.find('<') {
            let name = dep_str[..idx].trim().to_lowercase();
            let version_str = dep_str[idx + 1..].trim();
            return match VersionSpec::parse(&format!("<{}", version_str)) {
                Ok(spec) => Some((name, spec)),
                Err(_) => Some((name, VersionSpec::Any)),
            };
        }

        if let Some(idx) = dep_str.find('=') {
            let name = dep_str[..idx].trim().to_lowercase();
            let version_str = dep_str[idx + 1..].trim();

            // Conda uses = for pinning, convert to ==
            return match VersionSpec::parse(&format!("=={}", version_str)) {
                Ok(spec) => Some((name, spec)),
                Err(_) => Some((name, VersionSpec::Any)),
            };
        }

        // No version specified - just package name
        let name = dep_str.to_lowercase();
        Some((name, VersionSpec::Any))
    }

    /// Parse a pip dependency string (these follow pip format, not conda format)
    /// Examples:
    /// - "numpy" -> (numpy, Any)
    /// - "numpy==1.24.0" -> (numpy, ==1.24.0)
    /// - "numpy>=1.24.0,<2.0.0" -> (numpy, >=1.24.0,<2.0.0)
    fn parse_pip_dependency(dep_str: &str) -> Option<(String, VersionSpec)> {
        let dep_str = dep_str.trim();

        // Skip empty strings or comments
        if dep_str.is_empty() || dep_str.starts_with('#') {
            return None;
        }

        // Pip format uses various operators: ==, >=, <=, ~=, !=, <, >
        // and can have multiple constraints separated by commas

        // Find where the version spec starts (first operator character)
        let operators = ["==", ">=", "<=", "~=", "!=", "<", ">", "^", "~"];
        let mut split_pos = None;

        for op in &operators {
            if let Some(pos) = dep_str.find(op) {
                if split_pos.is_none() || pos < split_pos.unwrap() {
                    split_pos = Some(pos);
                }
            }
        }

        if let Some(pos) = split_pos {
            let name = dep_str[..pos].trim().to_lowercase();
            let version_str = dep_str[pos..].trim();

            return match VersionSpec::parse(version_str) {
                Ok(spec) => Some((name, spec)),
                Err(_) => Some((name, VersionSpec::Any)),
            };
        }

        // No version specified - just package name
        let name = dep_str.to_lowercase();
        Some((name, VersionSpec::Any))
    }
}

impl DependencyParser for CondaParser {
    fn parse(&self, path: &PathBuf) -> Result<Vec<Dependency>> {
        let content = fs::read_to_string(path)
            .context(format!("Failed to read file: {}", path.display()))?;

        let yaml: Value = serde_yaml::from_str(&content)
            .context(format!("Failed to parse YAML: {}", path.display()))?;

        let mut dependencies = Vec::new();

        // Get the dependencies list
        if let Some(deps) = yaml.get("dependencies").and_then(|v| v.as_sequence()) {
            for (idx, dep) in deps.iter().enumerate() {
                // Line number is approximate - YAML line numbers are tricky
                // We'll use the array index + 1 (assuming dependencies: starts at line 1)
                let line_number = idx + 2; // +2 because: 1 for "dependencies:" line, 1 for 0-based index

                // Dependencies can be either strings or objects (for pip section)
                if let Some(dep_str) = dep.as_str() {
                    // Regular conda dependency as a string
                    if let Some((name, version_spec)) = Self::parse_conda_dependency(dep_str) {
                        dependencies.push(Dependency {
                            name,
                            version_spec,
                            source_file: path.clone(),
                            line_number,
                            original_line: format!("  - {}", dep_str),
                        });
                    }
                } else if let Some(pip_section) = dep.as_mapping() {
                    // This might be a pip section: { pip: [...] }
                    if let Some(pip_deps) = pip_section.get("pip").and_then(|v| v.as_sequence()) {
                        for (pip_idx, pip_dep) in pip_deps.iter().enumerate() {
                            if let Some(pip_dep_str) = pip_dep.as_str() {
                                if let Some((name, version_spec)) = Self::parse_pip_dependency(pip_dep_str) {
                                    dependencies.push(Dependency {
                                        name,
                                        version_spec,
                                        source_file: path.clone(),
                                        line_number: line_number + pip_idx + 1, // Approximate line number
                                        original_line: format!("    - {}", pip_dep_str),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(dependencies)
    }

    fn can_parse(&self, path: &PathBuf) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "environment.yml" || n == "environment.yaml")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
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
        // Test simple package name
        let (name, spec) = CondaParser::parse_conda_dependency("numpy").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Any));

        // Test conda-style pinned version (=)
        let (name, spec) = CondaParser::parse_conda_dependency("numpy=1.24.0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Pinned(_)));

        // Test minimum version
        let (name, spec) = CondaParser::parse_conda_dependency("numpy>=1.24.0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Minimum(_)));

        // Test wildcard version
        let (name, spec) = CondaParser::parse_conda_dependency("python=3.9.*").unwrap();
        assert_eq!(name, "python");
        assert!(matches!(spec, VersionSpec::Wildcard { .. }));
    }

    #[test]
    fn test_parse_pip_dependency() {
        // Test simple package name
        let (name, spec) = CondaParser::parse_pip_dependency("requests").unwrap();
        assert_eq!(name, "requests");
        assert!(matches!(spec, VersionSpec::Any));

        // Test pip-style pinned version (==)
        let (name, spec) = CondaParser::parse_pip_dependency("requests==2.28.0").unwrap();
        assert_eq!(name, "requests");
        assert!(matches!(spec, VersionSpec::Pinned(_)));

        // Test range
        let (name, spec) = CondaParser::parse_pip_dependency("numpy>=1.24.0,<2.0.0").unwrap();
        assert_eq!(name, "numpy");
        assert!(matches!(spec, VersionSpec::Range { .. }));

        // Test compatible release
        let (name, spec) = CondaParser::parse_pip_dependency("flask~=2.0.0").unwrap();
        assert_eq!(name, "flask");
        assert!(matches!(spec, VersionSpec::Compatible(_)));
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
        write!(temp_file, "{}", yaml_content).unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = CondaParser::new();
        let dependencies = parser.parse(&path).unwrap();

        // Should find 7 dependencies total (4 conda + 3 pip)
        assert_eq!(dependencies.len(), 7);

        // Check conda dependencies
        let python_dep = dependencies.iter().find(|d| d.name == "python").unwrap();
        assert!(matches!(python_dep.version_spec, VersionSpec::Wildcard { .. }));

        let numpy_dep = dependencies.iter().find(|d| d.name == "numpy").unwrap();
        assert!(matches!(numpy_dep.version_spec, VersionSpec::Pinned(_)));

        let pandas_dep = dependencies.iter().find(|d| d.name == "pandas").unwrap();
        assert!(matches!(pandas_dep.version_spec, VersionSpec::Minimum(_)));

        let sklearn_dep = dependencies.iter().find(|d| d.name == "scikit-learn").unwrap();
        assert!(matches!(sklearn_dep.version_spec, VersionSpec::Any));

        // Check pip dependencies
        let requests_dep = dependencies.iter().find(|d| d.name == "requests").unwrap();
        assert!(matches!(requests_dep.version_spec, VersionSpec::Pinned(_)));

        let flask_dep = dependencies.iter().find(|d| d.name == "flask").unwrap();
        assert!(matches!(flask_dep.version_spec, VersionSpec::Range { .. }));

        let django_dep = dependencies.iter().find(|d| d.name == "django").unwrap();
        assert!(matches!(django_dep.version_spec, VersionSpec::Any));
    }

    #[test]
    fn test_parse_environment_yaml() {
        let yaml_content = r#"
dependencies:
  - numpy=1.24.0
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "{}", yaml_content).unwrap();

        // Rename to .yaml extension
        let temp_path = temp_file.path().to_path_buf();
        let yaml_path = temp_path.parent().unwrap().join("environment.yaml");
        std::fs::write(&yaml_path, yaml_content).unwrap();

        let parser = CondaParser::new();
        assert!(parser.can_parse(&yaml_path));

        let dependencies = parser.parse(&yaml_path).unwrap();
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].name, "numpy");

        // Clean up
        std::fs::remove_file(&yaml_path).ok();
    }

    #[test]
    fn test_empty_dependencies() {
        let yaml_content = r#"
name: myenv
dependencies: []
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "{}", yaml_content).unwrap();
        let path = temp_file.path().to_path_buf();

        let parser = CondaParser::new();
        let dependencies = parser.parse(&path).unwrap();

        assert_eq!(dependencies.len(), 0);
    }
}
