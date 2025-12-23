use super::{Dependency, DependencyParser};
use crate::version::VersionSpec;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use toml::Value;

/// Parser for pyproject.toml files (PEP 621, Poetry, PDM)
pub struct PyProjectParser;

impl PyProjectParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse PEP 621 format dependencies
    fn parse_pep621_dependencies(
        &self,
        toml_value: &Value,
        path: &PathBuf,
        content: &str,
    ) -> Result<Vec<Dependency>> {
        let mut dependencies = Vec::new();

        // Parse [project.dependencies] - array of strings
        if let Some(deps) = toml_value
            .get("project")
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_array())
        {
            for dep_value in deps {
                if let Some(dep_str) = dep_value.as_str() {
                    if let Some(dep) = self.parse_dependency_string(dep_str, path, content) {
                        dependencies.push(dep);
                    }
                }
            }
        }

        // Parse [project.optional-dependencies] - tables of arrays
        if let Some(optional_deps) = toml_value
            .get("project")
            .and_then(|p| p.get("optional-dependencies"))
            .and_then(|d| d.as_table())
        {
            for (_group_name, deps_value) in optional_deps {
                if let Some(deps) = deps_value.as_array() {
                    for dep_value in deps {
                        if let Some(dep_str) = dep_value.as_str() {
                            if let Some(dep) = self.parse_dependency_string(dep_str, path, content)
                            {
                                dependencies.push(dep);
                            }
                        }
                    }
                }
            }
        }

        Ok(dependencies)
    }

    /// Parse Poetry format dependencies
    fn parse_poetry_dependencies(
        &self,
        toml_value: &Value,
        path: &PathBuf,
        content: &str,
    ) -> Result<Vec<Dependency>> {
        let mut dependencies = Vec::new();

        // Parse [tool.poetry.dependencies]
        if let Some(deps) = toml_value
            .get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_table())
        {
            for (pkg_name, version_value) in deps {
                // Skip python itself
                if pkg_name == "python" {
                    continue;
                }
                if let Some(dep) = self.parse_poetry_dependency(pkg_name, version_value, path, content)
                {
                    dependencies.push(dep);
                }
            }
        }

        // Parse [tool.poetry.dev-dependencies] (legacy)
        if let Some(deps) = toml_value
            .get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("dev-dependencies"))
            .and_then(|d| d.as_table())
        {
            for (pkg_name, version_value) in deps {
                if let Some(dep) = self.parse_poetry_dependency(pkg_name, version_value, path, content)
                {
                    dependencies.push(dep);
                }
            }
        }

        // Parse [tool.poetry.group.*.dependencies]
        if let Some(groups) = toml_value
            .get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("group"))
            .and_then(|g| g.as_table())
        {
            for (_group_name, group_value) in groups {
                if let Some(deps) = group_value.get("dependencies").and_then(|d| d.as_table()) {
                    for (pkg_name, version_value) in deps {
                        if pkg_name == "python" {
                            continue;
                        }
                        if let Some(dep) = self.parse_poetry_dependency(pkg_name, version_value, path, content)
                        {
                            dependencies.push(dep);
                        }
                    }
                }
            }
        }

        Ok(dependencies)
    }

    /// Parse PDM format dependencies
    fn parse_pdm_dependencies(
        &self,
        toml_value: &Value,
        path: &PathBuf,
        content: &str,
    ) -> Result<Vec<Dependency>> {
        let mut dependencies = Vec::new();

        // Parse [tool.pdm.dependencies] - similar to PEP 621 but in tool.pdm
        if let Some(deps) = toml_value
            .get("tool")
            .and_then(|t| t.get("pdm"))
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_array())
        {
            for dep_value in deps {
                if let Some(dep_str) = dep_value.as_str() {
                    if let Some(dep) = self.parse_dependency_string(dep_str, path, content) {
                        dependencies.push(dep);
                    }
                }
            }
        }

        // Parse [tool.pdm.dev-dependencies] - table of arrays
        if let Some(dev_deps) = toml_value
            .get("tool")
            .and_then(|t| t.get("pdm"))
            .and_then(|p| p.get("dev-dependencies"))
            .and_then(|d| d.as_table())
        {
            for (_group_name, deps_value) in dev_deps {
                if let Some(deps) = deps_value.as_array() {
                    for dep_value in deps {
                        if let Some(dep_str) = dep_value.as_str() {
                            if let Some(dep) = self.parse_dependency_string(dep_str, path, content)
                            {
                                dependencies.push(dep);
                            }
                        }
                    }
                }
            }
        }

        Ok(dependencies)
    }

    /// Parse PEP 735 dependency-groups format
    fn parse_dependency_groups(
        &self,
        toml_value: &Value,
        path: &PathBuf,
        content: &str,
    ) -> Result<Vec<Dependency>> {
        let mut dependencies = Vec::new();

        // Parse [dependency-groups] - tables of arrays
        if let Some(groups) = toml_value
            .get("dependency-groups")
            .and_then(|d| d.as_table())
        {
            for (_group_name, deps_value) in groups {
                if let Some(deps) = deps_value.as_array() {
                    for dep_value in deps {
                        if let Some(dep_str) = dep_value.as_str() {
                            if let Some(dep) = self.parse_dependency_string(dep_str, path, content)
                            {
                                dependencies.push(dep);
                            }
                        }
                    }
                }
            }
        }

        Ok(dependencies)
    }

    /// Parse a Poetry dependency entry which can be a string or inline table
    fn parse_poetry_dependency(
        &self,
        name: &str,
        value: &Value,
        path: &PathBuf,
        content: &str,
    ) -> Option<Dependency> {
        let version_str = match value {
            // Simple string version: package = "^1.0"
            Value::String(s) => s.clone(),
            // Inline table: package = {version = "^1.0", optional = true}
            Value::Table(table) => {
                // Get version from the table
                table.get("version")?.as_str()?.to_string()
            }
            _ => return None,
        };

        // Find the line number and original line text
        let (line_number, original_line) = self.find_line_in_content(content, name, &version_str);

        // Parse the version spec
        let version_spec = VersionSpec::parse(&version_str).ok()?;

        Some(Dependency {
            name: name.to_lowercase().replace('_', "-"),
            version_spec,
            source_file: path.clone(),
            line_number,
            original_line,
        })
    }

    /// Parse a dependency string like "requests>=2.28.0" or "numpy==1.24.0"
    fn parse_dependency_string(
        &self,
        dep_str: &str,
        path: &PathBuf,
        content: &str,
    ) -> Option<Dependency> {
        // Split by comparison operators
        let dep_str = dep_str.trim();

        // Handle markers (like ; python_version >= "3.8") by splitting on semicolon
        let dep_str = dep_str.split(';').next()?.trim();

        // Handle extras (like requests[security]) - extract package name
        let dep_str_no_extras = if let Some(idx) = dep_str.find('[') {
            &dep_str[..idx]
        } else {
            dep_str
        };

        // Find the package name and version spec
        let operators = [">=", "<=", "==", "!=", "~=", ">", "<", "^", "~"];

        for op in &operators {
            if let Some(idx) = dep_str_no_extras.find(op) {
                let pkg_name = dep_str_no_extras[..idx].trim();
                let version_part = dep_str_no_extras[idx..].trim();

                // Parse version spec
                let version_spec = VersionSpec::parse(version_part).ok()?;

                // Find line number and original line
                let (line_number, original_line) = self.find_line_in_content(content, pkg_name, version_part);

                return Some(Dependency {
                    name: pkg_name.to_lowercase().replace('_', "-"),
                    version_spec,
                    source_file: path.clone(),
                    line_number,
                    original_line,
                });
            }
        }

        // No version specifier found - might be just package name
        if !dep_str_no_extras.is_empty() {
            let pkg_name = dep_str_no_extras.trim();
            let (line_number, original_line) = self.find_line_in_content(content, pkg_name, "");

            return Some(Dependency {
                name: pkg_name.to_lowercase().replace('_', "-"),
                version_spec: VersionSpec::Any,
                source_file: path.clone(),
                line_number,
                original_line,
            });
        }

        None
    }

    /// Find the line number and original line text for a dependency in the file content
    fn find_line_in_content(&self, content: &str, pkg_name: &str, version_str: &str) -> (usize, String) {
        // Search for the line containing the package name
        for (i, line) in content.lines().enumerate() {
            let line_lower = line.to_lowercase();
            let pkg_lower = pkg_name.to_lowercase();

            // Check if line contains the package name
            if line_lower.contains(&pkg_lower) {
                // For TOML tables, look for the key
                if line.contains('=') || line.contains(pkg_name) {
                    // Make sure it's not a comment
                    if let Some(comment_idx) = line.find('#') {
                        if line[..comment_idx].to_lowercase().contains(&pkg_lower) {
                            return (i + 1, line.trim().to_string());
                        }
                    } else if !version_str.is_empty() && line.contains(version_str) {
                        return (i + 1, line.trim().to_string());
                    } else if line_lower.contains(&pkg_lower) {
                        return (i + 1, line.trim().to_string());
                    }
                }
            }
        }

        // Default if not found
        (1, format!("{} = \"{}\"", pkg_name, version_str))
    }
}

impl DependencyParser for PyProjectParser {
    fn parse(&self, path: &PathBuf) -> Result<Vec<Dependency>> {
        // Read file content
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        // Parse TOML
        let toml_value: Value = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML: {}", path.display()))?;

        let mut all_dependencies = Vec::new();

        // Try parsing all formats - a file might have multiple formats

        // PEP 621 format
        if let Ok(deps) = self.parse_pep621_dependencies(&toml_value, path, &content) {
            all_dependencies.extend(deps);
        }

        // Poetry format
        if let Ok(deps) = self.parse_poetry_dependencies(&toml_value, path, &content) {
            all_dependencies.extend(deps);
        }

        // PDM format
        if let Ok(deps) = self.parse_pdm_dependencies(&toml_value, path, &content) {
            all_dependencies.extend(deps);
        }

        // PEP 735 dependency-groups format
        if let Ok(deps) = self.parse_dependency_groups(&toml_value, path, &content) {
            all_dependencies.extend(deps);
        }

        // Deduplicate dependencies by name (keep first occurrence)
        let mut seen = std::collections::HashSet::new();
        all_dependencies.retain(|dep| seen.insert(dep.name.clone()));

        Ok(all_dependencies)
    }

    fn can_parse(&self, path: &PathBuf) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "pyproject.toml")
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
        let parser = PyProjectParser::new();
        assert!(parser.can_parse(&PathBuf::from("pyproject.toml")));
        assert!(parser.can_parse(&PathBuf::from("/path/to/pyproject.toml")));
        assert!(!parser.can_parse(&PathBuf::from("requirements.txt")));
    }

    #[test]
    fn test_parse_pep621_dependencies() {
        let content = r#"
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
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let path = PathBuf::from(file.path());

        let parser = PyProjectParser::new();
        let deps = parser.parse(&path).unwrap();

        assert_eq!(deps.len(), 5);
        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "numpy"));
        assert!(deps.iter().any(|d| d.name == "flask"));
        assert!(deps.iter().any(|d| d.name == "pytest"));
        assert!(deps.iter().any(|d| d.name == "black"));
    }

    #[test]
    fn test_parse_poetry_dependencies() {
        let content = r#"
[tool.poetry]
name = "myproject"

[tool.poetry.dependencies]
python = "^3.8"
requests = "^2.28.0"
numpy = "1.24.0"

[tool.poetry.group.dev.dependencies]
pytest = "^7.0.0"
black = {version = "^22.0.0", optional = true}
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let path = PathBuf::from(file.path());

        let parser = PyProjectParser::new();
        let deps = parser.parse(&path).unwrap();

        // Should not include python itself
        assert!(!deps.iter().any(|d| d.name == "python"));
        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "numpy"));
        assert!(deps.iter().any(|d| d.name == "pytest"));
        assert!(deps.iter().any(|d| d.name == "black"));

        // Check version specs are parsed correctly
        let requests_dep = deps.iter().find(|d| d.name == "requests").unwrap();
        assert!(matches!(requests_dep.version_spec, VersionSpec::Caret(_)));
    }

    #[test]
    fn test_parse_pdm_dependencies() {
        // PDM uses PEP 621 format for main dependencies
        // and [tool.pdm.dev-dependencies] for dev dependencies
        let content = r#"
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
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let path = PathBuf::from(file.path());

        let parser = PyProjectParser::new();
        let deps = parser.parse(&path).unwrap();

        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "numpy"));
        assert!(deps.iter().any(|d| d.name == "pytest"));
    }

    #[test]
    fn test_parse_dependency_with_extras() {
        let content = r#"
[project]
dependencies = [
    "requests[security]>=2.28.0",
]
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let path = PathBuf::from(file.path());

        let parser = PyProjectParser::new();
        let deps = parser.parse(&path).unwrap();

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
    }

    #[test]
    fn test_parse_dependency_with_markers() {
        let content = r#"
[project]
dependencies = [
    "requests>=2.28.0; python_version >= '3.8'",
]
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let path = PathBuf::from(file.path());

        let parser = PyProjectParser::new();
        let deps = parser.parse(&path).unwrap();

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
    }

    #[test]
    fn test_deduplication() {
        // If same package appears in multiple sections, keep first one
        let content = r#"
[project]
dependencies = [
    "requests>=2.28.0",
]

[project.optional-dependencies]
dev = [
    "requests>=2.30.0",
]
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let path = PathBuf::from(file.path());

        let parser = PyProjectParser::new();
        let deps = parser.parse(&path).unwrap();

        // Should only have one requests entry (the first one)
        assert_eq!(deps.iter().filter(|d| d.name == "requests").count(), 1);
    }
}
