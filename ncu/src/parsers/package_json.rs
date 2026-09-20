use anyhow::{Context, Result};
use check_updates_core::{Dependency, VersionSpec};
use std::fs;
use std::path::Path;

pub struct PackageJsonParser;

impl PackageJsonParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse dependencies from a package.json file
    pub fn parse(&self, path: &Path) -> Result<Vec<Dependency>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON in {}", path.display()))?;

        let mut deps = Vec::new();

        // Each section is parsed under its own name so that the resulting
        // `Dependency` records which table it came from; the updater needs that
        // to avoid writing a `dependencies` bump into `peerDependencies`.
        for section in SECTIONS {
            if let Some(table) = parsed.get(section).and_then(|v| v.as_object()) {
                deps.extend(self.parse_deps(table, section, path, &content));
            }
        }

        Ok(deps)
    }

    fn parse_deps(
        &self,
        deps: &serde_json::Map<String, serde_json::Value>,
        section: &str,
        source_file: &Path,
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
                    let line_number = Self::find_line_number(content, section, name);
                    let original_line = content
                        .lines()
                        .nth(line_number.saturating_sub(1))
                        .unwrap_or("")
                        .to_string();

                    result.push(Dependency {
                        name: name.clone(),
                        version_spec,
                        source_file: source_file.to_path_buf(),
                        line_number,
                        original_line,
                        manifest_key: None,
                        section: Some(section.to_string()),
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
        VersionSpec::parse(s).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Best-effort line number of `package_name` inside `section`.
    ///
    /// The previous version scanned the whole file for the first `"name"`
    /// occurrence, so a package listed in both `dependencies` and
    /// `peerDependencies` reported the same line twice. Scanning starts at the
    /// section header instead. This is still textual and still approximate -
    /// it is display/diagnostic data, not what the updater edits.
    fn find_line_number(content: &str, section: &str, package_name: &str) -> usize {
        let section_header = format!("\"{section}\"");
        let key = format!("\"{package_name}\"");
        let mut in_section = false;

        for (i, line) in content.lines().enumerate() {
            if !in_section {
                if line.contains(&section_header) {
                    in_section = true;
                }
                continue;
            }
            if line.contains(&key) {
                return i + 1;
            }
            // A closing brace at the start of the trimmed line ends the table.
            if line.trim_start().starts_with('}') {
                break;
            }
        }

        1
    }
}

/// package.json dependency tables ncu reads and writes, in file-conventional order.
pub const SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

impl Default for PackageJsonParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        let deps = parser.parse(file.path())?;

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
        let deps = parser.parse(file.path())?;

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "express");

        Ok(())
    }

    #[test]
    fn same_package_in_two_sections_keeps_its_own_section_and_line() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"{{
  "dependencies": {{
    "react": "^18.0.0"
  }},
  "peerDependencies": {{
    "react": "^17.0.0"
  }}
}}"#
        )?;

        let parser = PackageJsonParser::new();
        let deps = parser.parse(file.path())?;

        let runtime = deps
            .iter()
            .find(|d| d.section.as_deref() == Some("dependencies"))
            .unwrap();
        let peer = deps
            .iter()
            .find(|d| d.section.as_deref() == Some("peerDependencies"))
            .unwrap();

        assert_eq!(runtime.name, "react");
        assert_eq!(peer.name, "react");
        assert_ne!(
            runtime.line_number, peer.line_number,
            "each section entry must point at its own line"
        );

        Ok(())
    }
}
