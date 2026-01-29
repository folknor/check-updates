use anyhow::{Context, Result};
use check_updates_core::{Dependency, VersionSpec};
use std::fs;
use std::path::PathBuf;

pub struct PackageJsonParser;

impl PackageJsonParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse dependencies from a package.json file
    pub fn parse(&self, path: &PathBuf) -> Result<Vec<Dependency>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON in {}", path.display()))?;

        let mut deps = Vec::new();

        // Parse dependencies
        if let Some(dependencies) = parsed.get("dependencies").and_then(|v| v.as_object()) {
            deps.extend(self.parse_deps(dependencies, path, &content));
        }

        // Parse devDependencies
        if let Some(dev_deps) = parsed.get("devDependencies").and_then(|v| v.as_object()) {
            deps.extend(self.parse_deps(dev_deps, path, &content));
        }

        // Parse peerDependencies
        if let Some(peer_deps) = parsed.get("peerDependencies").and_then(|v| v.as_object()) {
            deps.extend(self.parse_deps(peer_deps, path, &content));
        }

        // Parse optionalDependencies
        if let Some(opt_deps) = parsed.get("optionalDependencies").and_then(|v| v.as_object()) {
            deps.extend(self.parse_deps(opt_deps, path, &content));
        }

        Ok(deps)
    }

    fn parse_deps(
        &self,
        deps: &serde_json::Map<String, serde_json::Value>,
        source_file: &PathBuf,
        content: &str,
    ) -> Vec<Dependency> {
        let mut result = Vec::new();

        for (name, version_value) in deps {
            if let Some(version_str) = version_value.as_str() {
                // Skip non-registry deps (git, file, link, workspace)
                if version_str.starts_with("git")
                    || version_str.starts_with("file:")
                    || version_str.starts_with("link:")
                    || version_str.starts_with("workspace:")
                    || version_str.contains("github:")
                    || version_str.contains("://")
                {
                    continue;
                }

                if let Ok(version_spec) = Self::parse_npm_version(version_str) {
                    let line_number = Self::find_line_number(content, name);
                    let original_line = content
                        .lines()
                        .nth(line_number.saturating_sub(1))
                        .unwrap_or("")
                        .to_string();

                    result.push(Dependency {
                        name: name.clone(),
                        version_spec,
                        source_file: source_file.clone(),
                        line_number,
                        original_line,
                    });
                }
            }
        }

        result
    }

    /// Parse npm version spec into VersionSpec
    fn parse_npm_version(s: &str) -> Result<VersionSpec> {
        let s = s.trim();

        // npm uses same caret/tilde semantics
        // ^1.2.3, ~1.2.3, >=1.0.0, 1.2.3, etc.
        VersionSpec::parse(s).map_err(|e| anyhow::anyhow!("{}", e))
    }

    fn find_line_number(content: &str, package_name: &str) -> usize {
        for (i, line) in content.lines().enumerate() {
            if line.contains(&format!("\"{}\"", package_name)) {
                return i + 1;
            }
        }
        1
    }
}

impl Default for PackageJsonParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_dependencies() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"{{
  "name": "test",
  "dependencies": {{
    "express": "^4.18.0",
    "lodash": "~4.17.0"
  }},
  "devDependencies": {{
    "typescript": "^5.0.0"
  }}
}}"#
        )?;

        let parser = PackageJsonParser::new();
        let deps = parser.parse(&file.path().to_path_buf())?;

        assert_eq!(deps.len(), 3);

        let express = deps.iter().find(|d| d.name == "express").unwrap();
        assert_eq!(express.version_spec.version_string().unwrap(), "4.18.0");

        Ok(())
    }

    #[test]
    fn test_skip_git_deps() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"{{
  "dependencies": {{
    "express": "^4.18.0",
    "my-pkg": "git+https://github.com/user/repo.git",
    "local": "file:../local"
  }}
}}"#
        )?;

        let parser = PackageJsonParser::new();
        let deps = parser.parse(&file.path().to_path_buf())?;

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "express");

        Ok(())
    }
}
