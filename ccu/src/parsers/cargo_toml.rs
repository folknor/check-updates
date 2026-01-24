use super::{Dependency, DependencyParser};
use check_updates_core::VersionSpec;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use toml::Value;

/// Parser for Cargo.toml files
pub struct CargoTomlParser;

impl CargoTomlParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse dependencies from a TOML table
    fn parse_deps_table(
        &self,
        table: &toml::map::Map<String, Value>,
        source_file: &PathBuf,
        content: &str,
    ) -> Vec<Dependency> {
        let mut deps = Vec::new();

        for (name, value) in table {
            if let Some(version_str) = self.extract_version(value) {
                // Find line number for this dependency
                let line_number = self.find_line_number(content, name, &version_str);
                let original_line = content
                    .lines()
                    .nth(line_number.saturating_sub(1))
                    .unwrap_or("")
                    .to_string();

                if let Ok(version_spec) = Self::parse_cargo_version(&version_str) {
                    deps.push(Dependency {
                        name: name.clone(),
                        version_spec,
                        source_file: source_file.clone(),
                        line_number,
                        original_line,
                    });
                }
            }
        }

        deps
    }

    /// Parse a Cargo version spec (bare versions are caret in Cargo semantics)
    fn parse_cargo_version(s: &str) -> Result<VersionSpec> {
        let s = s.trim();

        // If it has an operator, use standard parsing
        if s.starts_with('^') || s.starts_with('~') || s.starts_with('>')
            || s.starts_with('<') || s.starts_with('=') || s.contains('*')
            || s.contains(',')
        {
            return VersionSpec::parse(s).map_err(|e| anyhow::anyhow!("{}", e));
        }

        // Bare version in Cargo means caret (^)
        // e.g., "1.0" means "^1.0" which allows 1.x but not 2.0
        VersionSpec::parse(&format!("^{}", s)).map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Extract version string from a dependency value
    fn extract_version(&self, value: &Value) -> Option<String> {
        match value {
            // Simple string version: serde = "1.0"
            Value::String(s) => Some(s.clone()),
            // Table with version: serde = { version = "1.0", features = [...] }
            Value::Table(table) => {
                // Skip dependencies with git or path (no version from crates.io)
                if table.contains_key("git") || table.contains_key("path") {
                    return None;
                }
                table.get("version").and_then(|v| v.as_str()).map(String::from)
            }
            _ => None,
        }
    }

    /// Find the line number for a dependency
    fn find_line_number(&self, content: &str, name: &str, _version: &str) -> usize {
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            // Match lines like: name = "version" or name = { version = "..." }
            if trimmed.starts_with(name)
                && (trimmed.contains('=') || trimmed.contains('{'))
            {
                // Make sure it's not a substring match
                let after_name = &trimmed[name.len()..].trim_start();
                if after_name.starts_with('=') || after_name.starts_with('.') {
                    return idx + 1;
                }
            }
        }
        1 // Default to line 1 if not found
    }
}

impl Default for CargoTomlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl DependencyParser for CargoTomlParser {
    fn parse(&self, path: &PathBuf) -> Result<Vec<Dependency>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: Value = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML in {}", path.display()))?;

        let mut all_deps = Vec::new();

        // Parse [dependencies]
        if let Some(deps) = parsed.get("dependencies").and_then(|v| v.as_table()) {
            all_deps.extend(self.parse_deps_table(deps, path, &content));
        }

        // Parse [dev-dependencies]
        if let Some(deps) = parsed.get("dev-dependencies").and_then(|v| v.as_table()) {
            all_deps.extend(self.parse_deps_table(deps, path, &content));
        }

        // Parse [build-dependencies]
        if let Some(deps) = parsed.get("build-dependencies").and_then(|v| v.as_table()) {
            all_deps.extend(self.parse_deps_table(deps, path, &content));
        }

        // Parse [workspace.dependencies]
        if let Some(workspace) = parsed.get("workspace").and_then(|v| v.as_table()) {
            if let Some(deps) = workspace.get("dependencies").and_then(|v| v.as_table()) {
                all_deps.extend(self.parse_deps_table(deps, path, &content));
            }
        }

        // Parse [target.'cfg(...)'.dependencies]
        if let Some(target) = parsed.get("target").and_then(|v| v.as_table()) {
            for (_target_name, target_value) in target {
                if let Some(target_table) = target_value.as_table() {
                    if let Some(deps) = target_table.get("dependencies").and_then(|v| v.as_table()) {
                        all_deps.extend(self.parse_deps_table(deps, path, &content));
                    }
                    if let Some(deps) = target_table.get("dev-dependencies").and_then(|v| v.as_table()) {
                        all_deps.extend(self.parse_deps_table(deps, path, &content));
                    }
                }
            }
        }

        Ok(all_deps)
    }

    fn can_parse(&self, path: &PathBuf) -> bool {
        path.file_name()
            .map(|n| n == "Cargo.toml")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_simple_deps() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
serde = "1.0"
tokio = {{ version = "1.0", features = ["full"] }}
"#
        )?;

        let parser = CargoTomlParser::new();
        let deps = parser.parse(&file.path().to_path_buf())?;

        assert_eq!(deps.len(), 2);

        let serde_dep = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde_dep.version_spec.version_string().unwrap(), "1.0");

        let tokio_dep = deps.iter().find(|d| d.name == "tokio").unwrap();
        assert_eq!(tokio_dep.version_spec.version_string().unwrap(), "1.0");

        Ok(())
    }

    #[test]
    fn test_skip_git_deps() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"
[dependencies]
serde = "1.0"
my-crate = {{ git = "https://github.com/foo/bar" }}
local-crate = {{ path = "../local" }}
"#
        )?;

        let parser = CargoTomlParser::new();
        let deps = parser.parse(&file.path().to_path_buf())?;

        // Should only have serde, not git/path deps
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");

        Ok(())
    }

    #[test]
    fn test_parse_dev_and_build_deps() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"
[dependencies]
serde = "1.0"

[dev-dependencies]
tempfile = "3.0"

[build-dependencies]
cc = "1.0"
"#
        )?;

        let parser = CargoTomlParser::new();
        let deps = parser.parse(&file.path().to_path_buf())?;

        assert_eq!(deps.len(), 3);
        assert!(deps.iter().any(|d| d.name == "serde"));
        assert!(deps.iter().any(|d| d.name == "tempfile"));
        assert!(deps.iter().any(|d| d.name == "cc"));

        Ok(())
    }
}
