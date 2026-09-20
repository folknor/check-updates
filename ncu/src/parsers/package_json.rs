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

        for (key, version_value) in deps {
            if let Some(version_str) = version_value.as_str() {
                // Allow-list, not deny-list: see `classify_specifier`.
                let Some(specifier) = classify_specifier(version_str) else {
                    continue;
                };

                if let Ok(version_spec) = Self::parse_npm_version(specifier.range) {
                    let line_number = Self::find_line_number(content, section, key);
                    let original_line = line_number
                        .and_then(|n| content.lines().nth(n.saturating_sub(1)))
                        .unwrap_or("")
                        .to_string();

                    // `Dependency::name` is contractually the *upstream* name on
                    // the registry, so an `npm:` alias reports the aliased
                    // package and keeps the local table key in `manifest_key` -
                    // the same split ccu uses for Cargo `package = "..."`
                    // renames. The updater writes back under `manifest_key`.
                    let (name, manifest_key) = match specifier.upstream {
                        Some(upstream) if upstream != key.as_str() => {
                            (upstream.to_string(), Some(key.clone()))
                        }
                        _ => (key.clone(), None),
                    };

                    result.push(Dependency {
                        name,
                        version_spec,
                        source_file: source_file.to_path_buf(),
                        line_number,
                        original_line,
                        manifest_key,
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
    /// it is display/diagnostic data, not what the updater edits. `None` when
    /// the key is not found inside the section; never a fabricated `1`.
    fn find_line_number(content: &str, section: &str, package_name: &str) -> Option<usize> {
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
                return Some(i + 1);
            }
            // A closing brace at the start of the trimmed line ends the table.
            if line.trim_start().starts_with('}') {
                break;
            }
        }

        None
    }
}

/// A package.json specifier we are prepared to resolve against the npm registry.
pub struct RegistrySpecifier<'a> {
    /// Upstream package name, when the specifier names one explicitly
    /// (`npm:lodash@^4`). `None` means "whatever the table key says".
    pub upstream: Option<&'a str>,
    /// The semver range part, with any alias prefix removed.
    pub range: &'a str,
}

/// Decide whether a package.json specifier addresses the npm registry, and if
/// so under which name and range.
///
/// This replaced a deny-list (`git`/`file:`/`link:`/`workspace:`/`github:`/
/// `://`). A deny-list cannot work here, for a structural reason:
/// `VersionSpec::parse` never fails - anything it does not recognise becomes
/// `VersionSpec::Complex(text)` - so every specifier the deny-list forgot was
/// shipped to the registry verbatim as a package name. The list forgot pnpm
/// `catalog:`, yarn berry `patch:`/`portal:`/`exec:`, bare GitHub shorthand
/// (`user/repo`), dist-tags (`latest`, `next`), tarball paths and `npm:`
/// aliases; npm's specifier grammar is open-ended, so it would have kept
/// forgetting new ones. The allow-list inverts the failure mode: an unknown
/// specifier is silently skipped rather than turned into a bogus registry
/// query (a 404 in the error list, or worse a real unrelated package).
///
/// Accepted: an optional `npm:<name>@` alias prefix followed by text made only
/// of semver-range characters and containing at least one digit, plus the
/// "any version" spellings `*` and `""`. That admits `^1.2.3`, `~1.2`, `1.x`,
/// `>=1 <2`, `^17 || ^18`, `1.2.3-beta.1`. It rejects anything containing `:`,
/// `/` or `#`, and anything digit-free.
pub fn classify_specifier(version_str: &str) -> Option<RegistrySpecifier<'_>> {
    let s = version_str.trim();

    if let Some(rest) = s.strip_prefix("npm:") {
        // `npm:@scope/pkg@^1.0.0` - the separating `@` is the last one, and a
        // leading `@` belongs to the scope.
        let split = rest
            .char_indices()
            .skip(1)
            .filter(|(_, c)| *c == '@')
            .map(|(i, _)| i)
            .last();
        let (name, range) = match split {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            // `"lodash4": "npm:lodash"` - an alias with no range at all.
            None => (rest, "*"),
        };
        if !is_package_name(name) || !is_registry_range(range) {
            return None;
        }
        return Some(RegistrySpecifier {
            upstream: Some(name),
            range,
        });
    }

    if is_registry_range(s) {
        Some(RegistrySpecifier {
            upstream: None,
            range: s,
        })
    } else {
        None
    }
}

fn is_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    let body = match name.strip_prefix('@') {
        // Scoped: exactly one `/`, both halves non-empty.
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, pkg)) if !scope.is_empty() && !pkg.is_empty() && !pkg.contains('/') => {
                return scope.chars().chain(pkg.chars()).all(is_name_char);
            }
            _ => return false,
        },
        None => name,
    };
    body.chars().all(is_name_char)
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn is_registry_range(range: &str) -> bool {
    let range = range.trim();
    if range.is_empty() || range == "*" || range == "x" || range == "X" {
        return true;
    }
    if !range.chars().any(|c| c.is_ascii_digit()) {
        // Dist-tags (`latest`, `next`) resolve on the registry but carry no
        // range we could compare or rewrite, so they are not ours to report.
        return false;
    }
    range.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '.' | '-' | '+' | '^' | '~' | '<' | '>' | '=' | '*' | '|' | ',' | ' '
            )
    })
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
    fn npm_alias_reports_the_upstream_name_and_keeps_the_local_key() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"{{
  "dependencies": {{
    "lodash4": "npm:lodash@^4.17.0",
    "ui": "npm:@scope/ui@~2.1.0",
    "whole": "npm:left-pad"
  }}
}}"#
        )?;

        let deps = PackageJsonParser::new().parse(file.path())?;
        assert_eq!(deps.len(), 3);

        let lodash = deps.iter().find(|d| d.name == "lodash").unwrap();
        assert_eq!(lodash.manifest_key.as_deref(), Some("lodash4"));
        assert_eq!(lodash.version_spec.version_string().unwrap(), "4.17.0");

        let ui = deps.iter().find(|d| d.name == "@scope/ui").unwrap();
        assert_eq!(ui.manifest_key.as_deref(), Some("ui"));

        let whole = deps.iter().find(|d| d.name == "left-pad").unwrap();
        assert_eq!(whole.manifest_key.as_deref(), Some("whole"));
        assert!(matches!(whole.version_spec, VersionSpec::Any));

        Ok(())
    }

    #[test]
    fn non_registry_protocols_are_not_queried() {
        for spec in [
            "catalog:",
            "catalog:default",
            "patch:left-pad@1.0.0#./patch.diff",
            "portal:../pkg",
            "exec:./gen.js",
            "workspace:*",
            "link:../pkg",
            "file:../pkg",
            "git+ssh://git@github.com/u/r.git",
            "user/repo",
            "user/repo#semver:^1.0.0",
            "https://example.com/pkg.tgz",
            "latest",
            "next",
            "npm:",
            "npm:@scope@1.0.0",
        ] {
            assert!(
                classify_specifier(spec).is_none(),
                "{spec} must not reach the registry"
            );
        }
    }

    #[test]
    fn plain_ranges_are_accepted_without_an_upstream_override() {
        for spec in ["^1.2.3", "~1.2", "1.x", ">=1.0.0 <2.0.0", "^17 || ^18", "*"] {
            let parsed = classify_specifier(spec).unwrap_or_else(|| panic!("{spec}"));
            assert!(parsed.upstream.is_none());
            assert_eq!(parsed.range, spec);
        }
    }

    #[test]
    fn an_alias_whose_key_equals_the_upstream_name_carries_no_manifest_key() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"{{
  "dependencies": {{
    "lodash": "npm:lodash@^4.17.0"
  }}
}}"#
        )?;

        let deps = PackageJsonParser::new().parse(file.path())?;
        assert_eq!(deps[0].name, "lodash");
        assert_eq!(deps[0].manifest_key, None);

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
