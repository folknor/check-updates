use super::{Dependency, DependencyParser};
use check_updates_core::VersionSpec;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use toml::Value;

/// Parser for Cargo.toml files
pub struct CargoTomlParser {
    /// Workspace dependency versions resolved from root Cargo.toml [workspace.dependencies]
    workspace_deps: HashMap<String, String>,
    /// Path to the root Cargo.toml (for correct source_file attribution on workspace deps)
    workspace_root: Option<std::path::PathBuf>,
}

impl CargoTomlParser {
    pub fn new() -> Self {
        Self {
            workspace_deps: HashMap::new(),
            workspace_root: None,
        }
    }

    /// Extract [workspace.dependencies] version map from a root Cargo.toml path.
    /// Call this before parsing member crates so `.workspace = true` deps can be resolved.
    pub fn load_workspace_deps(&mut self, root_cargo_toml: &Path) -> Result<()> {
        let content = fs::read_to_string(root_cargo_toml)
            .with_context(|| format!("Failed to read {}", root_cargo_toml.display()))?;

        let parsed: Value = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML in {}", root_cargo_toml.display()))?;

        if let Some(workspace) = parsed.get("workspace").and_then(|v| v.as_table())
            && let Some(deps) = workspace.get("dependencies").and_then(|v| v.as_table())
        {
            for (name, value) in deps {
                if let Some(version) = self.extract_version(value) {
                    self.workspace_deps.insert(name.clone(), version);
                }
            }
        }

        if !self.workspace_deps.is_empty() {
            self.workspace_root = Some(root_cargo_toml.to_path_buf());
        }

        Ok(())
    }

    /// Parse dependencies from a TOML table
    fn parse_deps_table(
        &self,
        table: &toml::map::Map<String, Value>,
        source_file: &Path,
        content: &str,
    ) -> Vec<Dependency> {
        let mut deps = Vec::new();

        for (name, value) in table {
            let is_workspace_ref = Self::is_workspace_reference(value);

            if let Some(version_str) = self.extract_version_or_workspace(name, value) {
                // For workspace references resolved from root, point source_file
                // to the root Cargo.toml where the version is actually defined
                let effective_source = if is_workspace_ref {
                    self.workspace_root.as_deref().unwrap_or(source_file)
                } else {
                    source_file
                };

                // For workspace refs, find the line in the root content instead
                let (effective_content, line_number) = if is_workspace_ref
                    && let Some(root_path) = &self.workspace_root
                    && root_path != source_file
                {
                    if let Ok(root_content) = fs::read_to_string(root_path) {
                        let ln = self.find_line_number(&root_content, name, &version_str);
                        (Some(root_content), ln)
                    } else {
                        (None, self.find_line_number(content, name, &version_str))
                    }
                } else {
                    (None, self.find_line_number(content, name, &version_str))
                };

                let line_content = effective_content.as_deref().unwrap_or(content);
                let original_line = line_content
                    .lines()
                    .nth(line_number.saturating_sub(1))
                    .unwrap_or("")
                    .to_string();

                if let Ok(version_spec) = Self::parse_cargo_version(&version_str) {
                    deps.push(Dependency {
                        name: name.clone(),
                        version_spec,
                        source_file: effective_source.to_path_buf(),
                        line_number,
                        original_line,
                    });
                }
            }
        }

        deps
    }

    /// Check if a dependency value is a workspace reference (`.workspace = true`)
    fn is_workspace_reference(value: &Value) -> bool {
        if let Value::Table(table) = value {
            table.get("workspace").and_then(Value::as_bool) == Some(true)
        } else {
            false
        }
    }

    /// Parse a Cargo version spec (bare versions are caret in Cargo semantics)
    fn parse_cargo_version(s: &str) -> Result<VersionSpec> {
        let s = s.trim();

        // If it has an operator, use standard parsing
        if s.starts_with('^') || s.starts_with('~') || s.starts_with('>')
            || s.starts_with('<') || s.starts_with('=') || s.contains('*')
            || s.contains(',')
        {
            return VersionSpec::parse(s).map_err(|e| anyhow::anyhow!("{e}"));
        }

        // Bare version in Cargo means caret (^)
        // e.g., "1.0" means "^1.0" which allows 1.x but not 2.0
        VersionSpec::parse(&format!("^{s}")).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Extract version string from a dependency value, resolving `.workspace = true`
    /// against the loaded workspace dependencies when needed.
    fn extract_version_or_workspace(&self, name: &str, value: &Value) -> Option<String> {
        // First try direct version extraction
        if let Some(version) = self.extract_version(value) {
            return Some(version);
        }

        // Check for .workspace = true (shows up as a table with workspace = true)
        if let Value::Table(table) = value
            && table.get("workspace").and_then(Value::as_bool) == Some(true)
        {
            // Skip path/git deps even if workspace = true
            if table.contains_key("git") || table.contains_key("path") {
                return None;
            }
            return self.workspace_deps.get(name).cloned();
        }

        None
    }

    /// Extract version string from a dependency value (without workspace resolution)
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
    fn parse(&self, path: &Path) -> Result<Vec<Dependency>> {
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
        if let Some(workspace) = parsed.get("workspace").and_then(|v| v.as_table())
            && let Some(deps) = workspace.get("dependencies").and_then(|v| v.as_table()) {
                all_deps.extend(self.parse_deps_table(deps, path, &content));
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

    fn can_parse(&self, path: &Path) -> bool {
        path.file_name()
            .map(|n| n == "Cargo.toml")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

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

    #[test]
    fn test_workspace_dep_resolution() -> Result<()> {
        let tmp = TempDir::new()?;

        // Create root Cargo.toml with [workspace.dependencies]
        let root_toml = tmp.path().join("Cargo.toml");
        fs::write(
            &root_toml,
            r#"
[workspace]
members = ["member"]

[workspace.dependencies]
serde = { version = "1.0.200", features = ["derive"] }
tokio = "1.38"
local-dep = { path = "../local" }
"#,
        )?;

        // Create member Cargo.toml with .workspace = true refs
        let member_dir = tmp.path().join("member");
        fs::create_dir(&member_dir)?;
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"
[package]
name = "member"
version = "0.1.0"

[dependencies]
serde.workspace = true
tokio.workspace = true
local-dep.workspace = true
direct-dep = "2.0"
"#,
        )?;

        let mut parser = CargoTomlParser::new();
        parser.load_workspace_deps(&root_toml)?;

        let deps = parser.parse(&member_toml)?;

        // serde and tokio resolved from workspace, direct-dep is direct,
        // local-dep is a path dep and should be skipped
        assert_eq!(deps.len(), 3, "deps: {:?}", deps.iter().map(|d| &d.name).collect::<Vec<_>>());

        let serde_dep = deps.iter().find(|d| d.name == "serde").expect("serde");
        assert_eq!(serde_dep.version_spec.version_string().expect("version"), "1.0.200");
        // source_file should point to root Cargo.toml for workspace deps
        assert_eq!(serde_dep.source_file, root_toml);

        let tokio_dep = deps.iter().find(|d| d.name == "tokio").expect("tokio");
        assert_eq!(tokio_dep.version_spec.version_string().expect("version"), "1.38");

        let direct = deps.iter().find(|d| d.name == "direct-dep").expect("direct-dep");
        assert_eq!(direct.source_file, member_toml);

        Ok(())
    }

    #[test]
    fn test_workspace_deps_without_load() -> Result<()> {
        // Without calling load_workspace_deps, .workspace = true deps should be silently skipped
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"
[dependencies]
serde.workspace = true
direct = "1.0"
"#
        )?;

        let parser = CargoTomlParser::new();
        let deps = parser.parse(&file.path().to_path_buf())?;

        // Only direct dep should be found
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "direct");

        Ok(())
    }
}
