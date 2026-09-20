use super::{Dependency, DependencyParser};
use anyhow::{Context, Result};
use check_updates_core::VersionSpec;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use toml::Value;

/// Parser for Cargo.toml files
pub struct CargoTomlParser {
    /// Workspace dependency metadata resolved from root Cargo.toml [workspace.dependencies]
    workspace_deps: HashMap<String, WorkspaceDependency>,
    /// Path to the root Cargo.toml (for correct source_file attribution on workspace deps)
    workspace_root: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
struct WorkspaceDependency {
    package_name: String,
    version: String,
    manifest_key: Option<String>,
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
            for (key, value) in deps {
                if let Some(version) = self.extract_version(value) {
                    let package_name =
                        Self::extract_package_rename(value).unwrap_or_else(|| key.clone());
                    let manifest_key = if package_name == *key {
                        None
                    } else {
                        Some(key.clone())
                    };
                    self.workspace_deps.insert(
                        key.clone(),
                        WorkspaceDependency {
                            package_name,
                            version,
                            manifest_key,
                        },
                    );
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
        section: &str,
        section_path: &[&str],
    ) -> Vec<Dependency> {
        let mut deps = Vec::new();

        // The root manifest is where workspace-inherited deps are declared, so
        // their line numbers are looked up in its text. Read it once for the
        // whole table rather than once per inherited dependency.
        let root_content = self
            .workspace_root
            .as_deref()
            .filter(|root_path| *root_path != source_file)
            .and_then(|root_path| fs::read_to_string(root_path).ok());

        for (key, value) in table {
            let is_workspace_ref = Self::is_workspace_reference(value);
            // Resolve a `package = "..."` rename if present.
            // `name` is the upstream crate name we'll query on crates.io;
            // `key` stays as the local table key for the updater to find.
            let (name, manifest_key) =
                if is_workspace_ref && let Some(workspace_dep) = self.workspace_deps.get(key) {
                    (
                        workspace_dep.package_name.clone(),
                        workspace_dep.manifest_key.clone(),
                    )
                } else {
                    match Self::extract_package_rename(value) {
                        Some(upstream) => (upstream, Some(key.clone())),
                        None => (key.clone(), None),
                    }
                };

            if let Some(version_str) = self.extract_version_or_workspace(key, value) {
                // For workspace references resolved from root, point source_file
                // to the root Cargo.toml where the version is actually defined
                let effective_source = if is_workspace_ref {
                    self.workspace_root.as_deref().unwrap_or(source_file)
                } else {
                    source_file
                };

                // For workspace refs, find the line in the root content instead,
                // under [workspace.dependencies] where the version really lives.
                let (line_content, line_number) = match (is_workspace_ref, root_content.as_deref())
                {
                    (true, Some(root)) => (
                        root,
                        Self::find_line_number(root, &["workspace", "dependencies"], key),
                    ),
                    _ => (content, Self::find_line_number(content, section_path, key)),
                };
                let original_line = line_number
                    .and_then(|n| line_content.lines().nth(n.saturating_sub(1)))
                    .unwrap_or("")
                    .to_string();

                if let Ok(version_spec) = Self::parse_cargo_version(&version_str) {
                    deps.push(Dependency {
                        name,
                        version_spec,
                        source_file: effective_source.to_path_buf(),
                        line_number,
                        original_line,
                        manifest_key,
                        section: Some(section.to_string()),
                    });
                }
            }
        }

        deps
    }

    /// If this dependency uses `package = "..."` to rename the upstream crate
    /// (e.g. `tokio1_crate = { package = "tokio", version = "1" }`), return
    /// the upstream name.
    fn extract_package_rename(value: &Value) -> Option<String> {
        if let Value::Table(table) = value {
            table
                .get("package")
                .and_then(Value::as_str)
                .map(String::from)
        } else {
            None
        }
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
        if s.starts_with('^')
            || s.starts_with('~')
            || s.starts_with('>')
            || s.starts_with('<')
            || s.starts_with('=')
            || s.contains('*')
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
            return self.workspace_deps.get(name).map(|dep| dep.version.clone());
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
                table
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            }
            _ => None,
        }
    }

    /// Find the line number a dependency is declared on.
    ///
    /// The lookup goes through `toml_edit`, which records a byte span for every
    /// key it parses, so the answer is the span of *this* key inside *this*
    /// section - not the first line of the file that happens to mention the
    /// name. A text scan cannot tell `tokio = []` under `[features]` apart from
    /// the real declaration; the span can.
    ///
    /// `None` when the section or key is not found, or when the document does
    /// not reparse. ccu's updater edits through `toml_edit`, not by line, so
    /// this is display and JSON data only - but it must still be honest: a
    /// wrong line is worse than no line.
    fn find_line_number(content: &str, section_path: &[&str], key: &str) -> Option<usize> {
        let doc = toml_edit::Document::parse(content).ok()?;

        let mut item = doc.as_item();
        for segment in section_path {
            item = item.as_table_like()?.get(segment)?;
        }

        let (found_key, value) = item.as_table_like()?.get_key_value(key)?;
        let span = found_key.span().or_else(|| value.span())?;
        let prefix = content.get(..span.start)?;
        Some(prefix.matches('\n').count() + 1)
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
            all_deps.extend(self.parse_deps_table(
                deps,
                path,
                &content,
                "dependencies",
                &["dependencies"],
            ));
        }

        // Parse [dev-dependencies]
        if let Some(deps) = parsed.get("dev-dependencies").and_then(|v| v.as_table()) {
            all_deps.extend(self.parse_deps_table(
                deps,
                path,
                &content,
                "dev-dependencies",
                &["dev-dependencies"],
            ));
        }

        // Parse [build-dependencies]
        if let Some(deps) = parsed.get("build-dependencies").and_then(|v| v.as_table()) {
            all_deps.extend(self.parse_deps_table(
                deps,
                path,
                &content,
                "build-dependencies",
                &["build-dependencies"],
            ));
        }

        // Parse [workspace.dependencies]
        if let Some(workspace) = parsed.get("workspace").and_then(|v| v.as_table())
            && let Some(deps) = workspace.get("dependencies").and_then(|v| v.as_table())
        {
            all_deps.extend(self.parse_deps_table(
                deps,
                path,
                &content,
                "workspace.dependencies",
                &["workspace", "dependencies"],
            ));
        }

        // Parse [target.'cfg(...)'.{dependencies,dev-dependencies,build-dependencies}].
        // All three are legal under a target and the updater resolves all three,
        // so reading only two would leave platform-specific build deps unchecked.
        if let Some(target) = parsed.get("target").and_then(|v| v.as_table()) {
            for (target_name, target_value) in target {
                if let Some(target_table) = target_value.as_table() {
                    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
                        if let Some(deps) = target_table.get(kind).and_then(|v| v.as_table()) {
                            all_deps.extend(self.parse_deps_table(
                                deps,
                                path,
                                &content,
                                &format!("target.{target_name}.{kind}"),
                                &["target", target_name, kind],
                            ));
                        }
                    }
                }
            }
        }

        Ok(all_deps)
    }

    fn can_parse(&self, path: &Path) -> bool {
        path.file_name().map(|n| n == "Cargo.toml").unwrap_or(false)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        let deps = parser.parse(file.path())?;

        assert_eq!(deps.len(), 2);

        let serde_dep = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde_dep.version_spec.version_string().unwrap(), "1.0");

        let tokio_dep = deps.iter().find(|d| d.name == "tokio").unwrap();
        assert_eq!(tokio_dep.version_spec.version_string().unwrap(), "1.0");

        Ok(())
    }

    #[test]
    fn line_number_ignores_same_named_key_in_another_section() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"
[package]
name = "test"
version = "0.1.0"

[features]
default = []
tokio = []

[dependencies]
tokio = {{ version = "1.0", features = ["full"] }}
"#
        )?;

        let parser = CargoTomlParser::new();
        let deps = parser.parse(file.path())?;

        let tokio_dep = deps.iter().find(|d| d.name == "tokio").unwrap();
        // The declaration is the [dependencies] entry, not `tokio = []`
        // under [features] eight lines earlier.
        assert_eq!(tokio_dep.line_number, Some(11));
        assert!(tokio_dep.original_line.contains("version"));

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
        let deps = parser.parse(file.path())?;

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
        let deps = parser.parse(file.path())?;

        assert_eq!(deps.len(), 3);
        assert!(deps.iter().any(|d| d.name == "serde"));
        assert!(deps.iter().any(|d| d.name == "tempfile"));
        assert!(deps.iter().any(|d| d.name == "cc"));

        Ok(())
    }

    /// All three dependency kinds under a `[target.'cfg(...)']` are read, and
    /// each records the fully qualified section the updater resolves against.
    #[test]
    fn test_parse_target_sections_including_build_dependencies() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"
[target.'cfg(unix)'.dependencies]
libc = "0.2"

[target.'cfg(unix)'.dev-dependencies]
nix = "0.27"

[target.'cfg(windows)'.build-dependencies]
cc = "1.0"
"#
        )?;

        let parser = CargoTomlParser::new();
        let deps = parser.parse(file.path())?;

        assert_eq!(deps.len(), 3, "{deps:?}");

        let libc = deps.iter().find(|d| d.name == "libc").unwrap();
        assert_eq!(
            libc.section.as_deref(),
            Some("target.cfg(unix).dependencies")
        );

        let nix = deps.iter().find(|d| d.name == "nix").unwrap();
        assert_eq!(
            nix.section.as_deref(),
            Some("target.cfg(unix).dev-dependencies")
        );

        let cc = deps.iter().find(|d| d.name == "cc").unwrap();
        assert_eq!(
            cc.section.as_deref(),
            Some("target.cfg(windows).build-dependencies")
        );

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
        assert_eq!(
            deps.len(),
            3,
            "deps: {:?}",
            deps.iter().map(|d| &d.name).collect::<Vec<_>>()
        );

        let serde_dep = deps.iter().find(|d| d.name == "serde").expect("serde");
        assert_eq!(
            serde_dep.version_spec.version_string().expect("version"),
            "1.0.200"
        );
        // source_file should point to root Cargo.toml for workspace deps
        assert_eq!(serde_dep.source_file, root_toml);

        let tokio_dep = deps.iter().find(|d| d.name == "tokio").expect("tokio");
        assert_eq!(
            tokio_dep.version_spec.version_string().expect("version"),
            "1.38"
        );

        let direct = deps
            .iter()
            .find(|d| d.name == "direct-dep")
            .expect("direct-dep");
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
        let deps = parser.parse(file.path())?;

        // Only direct dep should be found
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "direct");

        Ok(())
    }

    #[test]
    fn test_workspace_renamed_dep_resolution() -> Result<()> {
        let tmp = TempDir::new()?;

        let root_toml = tmp.path().join("Cargo.toml");
        fs::write(
            &root_toml,
            r#"
[workspace]
members = ["member"]

[workspace.dependencies]
tokio1_crate = { package = "tokio", version = "1.38" }
"#,
        )?;

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
tokio1_crate.workspace = true
"#,
        )?;

        let mut parser = CargoTomlParser::new();
        parser.load_workspace_deps(&root_toml)?;

        let deps = parser.parse(&member_toml)?;

        assert_eq!(deps.len(), 1);
        let dep = &deps[0];
        assert_eq!(dep.name, "tokio");
        assert_eq!(dep.manifest_key.as_deref(), Some("tokio1_crate"));
        assert_eq!(dep.source_file, root_toml);
        assert_eq!(dep.version_spec.version_string().expect("version"), "1.38");

        Ok(())
    }

    #[test]
    fn test_renamed_dep_with_package_key() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
serde = "1.0"
tokio1_crate = {{ package = "tokio", version = "1.0" }}
"#
        )?;

        let parser = CargoTomlParser::new();
        let deps = parser.parse(file.path())?;

        assert_eq!(deps.len(), 2);

        let serde_dep = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde_dep.name, "serde");
        assert_eq!(serde_dep.manifest_key, None);

        // Renamed: name is the upstream "tokio", manifest_key preserves the
        // local "tokio1_crate" so the updater can find the table entry.
        let tokio_dep = deps.iter().find(|d| d.name == "tokio").unwrap();
        assert_eq!(tokio_dep.name, "tokio");
        assert_eq!(tokio_dep.manifest_key.as_deref(), Some("tokio1_crate"));

        Ok(())
    }
}
