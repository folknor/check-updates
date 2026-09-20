use anyhow::{Context, Result};
use check_updates_core::{DependencyCheck, UpdateSeverity, Version, VersionSpec, write_atomically};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::parsers::package_json::SECTIONS;

/// Render a `VersionSpec` as npm range syntax, or `None` when npm has no
/// equivalent and we must refuse to write.
///
/// `VersionSpec`'s `Display` impl is PEP 440 flavoured: it renders `Pinned` as
/// `==4.18.2`, `Compatible` as `~=1.2.3` and `Range` with a comma separator.
/// npm accepts none of those, and a bare `"express": "4.18.2"` - the single
/// most common form in a package.json - parses to `Pinned`, so using `Display`
/// here corrupted exact pins on every run. This is the npm-side counterpart to
/// ccu's `to_cargo_string()`.
fn to_npm_string(spec: &VersionSpec) -> Option<String> {
    fn v(version: &Version) -> String {
        version.to_string()
    }

    let rendered = match spec {
        // npm's exact match is the bare version, with no operator.
        VersionSpec::Pinned(ver) => v(ver),
        VersionSpec::Caret(ver) => format!("^{}", v(ver)),
        VersionSpec::Tilde(ver) => format!("~{}", v(ver)),
        VersionSpec::Minimum(ver) => format!(">={}", v(ver)),
        VersionSpec::Maximum(ver) => format!("<={}", v(ver)),
        VersionSpec::GreaterThan(ver) => format!(">{}", v(ver)),
        VersionSpec::LessThan(ver) => format!("<{}", v(ver)),
        // npm intersects with a space, not a comma.
        VersionSpec::Range { min, max } => format!(">={} <{}", v(min), v(max)),
        // `1.2.*` is valid npm. Render from `prefix`, never from `pattern`:
        // `with_version` advances the prefix but carries the *old* pattern
        // along verbatim, so emitting the pattern would write the spec we
        // were asked to replace. `==1.2.*` is a Python spelling that never
        // reaches ncu, and the bare form is what npm accepts anyway.
        VersionSpec::Wildcard { prefix, .. } => {
            let prefix = prefix.trim();
            if prefix.is_empty() || !prefix.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            format!("{prefix}.*")
        }
        // npm has no `!=` operator, no `~=`, and we will not guess at the
        // meaning of a spec our parser itself gave up on.
        VersionSpec::Compatible(_)
        | VersionSpec::NotEqual(_)
        | VersionSpec::Complex(_)
        | VersionSpec::Any => return None,
    };

    // A JSON string value we splice in unescaped must not contain characters
    // that would need escaping. No legal npm range does.
    if rendered.contains(['"', '\\']) {
        return None;
    }

    Some(rendered)
}

/// Updates package.json with new versions
pub struct FileUpdater;

impl FileUpdater {
    pub fn new() -> Self {
        Self
    }

    /// Apply updates to package.json based on severity filter
    pub fn apply_updates(
        &self,
        checks: &[DependencyCheck],
        include_minor: bool,
        force: bool,
    ) -> Result<UpdateResult> {
        let mut modified_files = HashSet::new();

        // Group checks by file, filtering by severity
        let mut file_updates: std::collections::HashMap<PathBuf, Vec<(&DependencyCheck, String)>> =
            std::collections::HashMap::new();

        for check in checks {
            let version_spec = if force {
                check.force_spec.as_ref()
            } else {
                match check.severity {
                    Some(UpdateSeverity::Patch) => check.target_spec.as_ref(),
                    Some(UpdateSeverity::Minor) if include_minor => check.target_spec.as_ref(),
                    _ => None,
                }
            };

            if let Some(spec) = version_spec
                && spec.is_rewritable()
                && let Some(new_version) = to_npm_string(spec)
            {
                file_updates
                    .entry(check.dependency.source_file.clone())
                    .or_default()
                    .push((check, new_version));
            }
        }

        for (file_path, updates) in file_updates {
            let changed = self
                .update_file(&file_path, &updates)
                .with_context(|| format!("Failed to update file: {}", file_path.display()))?;
            if changed {
                modified_files.insert(file_path);
            }
        }

        Ok(UpdateResult { modified_files })
    }

    /// Rewrite the given dependency specs in place. Returns whether the file
    /// content actually changed.
    ///
    /// This edits the original text rather than round-tripping through
    /// `serde_json::Value`. `serde_json::Map` is a `BTreeMap` unless the
    /// `preserve_order` feature is enabled (it is not, in this workspace), so
    /// the old parse -> mutate -> `to_string_pretty` path sorted every key at
    /// every nesting level, reflowed indentation to serde's two-space style and
    /// dropped the author's layout - on a file the user only asked us to change
    /// one version string in. Editing the source text is what ccu gets from
    /// `toml_edit`; there is no `json_edit`, so we do the splice ourselves.
    fn update_file(
        &self,
        file_path: &PathBuf,
        updates: &[(&DependencyCheck, String)],
    ) -> Result<bool> {
        let original = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        // Validate up front so a malformed manifest is an error rather than a
        // silent no-op, then never serialize the parsed value back out.
        serde_json::from_str::<serde_json::Value>(&original)
            .with_context(|| format!("Failed to parse JSON: {}", file_path.display()))?;

        let mut content = original.clone();

        for (check, new_version) in updates {
            // The table key, which for an `npm:` alias is the local name
            // (`"lodash4": "npm:lodash@^4.17.0"`) and not `dependency.name`,
            // which by contract holds the upstream registry name.
            let name = check
                .dependency
                .manifest_key
                .as_deref()
                .unwrap_or(&check.dependency.name);
            // Only the section the dependency was parsed from. A package can
            // sit in `dependencies` at `^1.0.0` and in `peerDependencies` at
            // `^1 || ^2`; the peer range is deliberately wide and is not ours
            // to narrow to a resolved pin.
            match check.dependency.section.as_deref() {
                Some(section) => {
                    content = replace_spec(&content, section, name, new_version).unwrap_or(content);
                }
                None => {
                    // Unknown provenance (a `Dependency` built by something
                    // other than the package.json parser). Fall back to the
                    // historical all-sections behaviour rather than silently
                    // doing nothing.
                    for section in SECTIONS {
                        content =
                            replace_spec(&content, section, name, new_version).unwrap_or(content);
                    }
                }
            }
        }

        if content == original {
            return Ok(false);
        }

        // The splice above edits the original text so key order and
        // indentation survive; only the final store goes through the
        // crash-safe path.
        write_atomically(file_path, content.as_bytes())
            .with_context(|| format!("Failed to update file: {}", file_path.display()))?;

        Ok(true)
    }
}

/// Replace the string value of `section.name` in a JSON document, returning the
/// new document, or `None` if that key was not found.
///
/// Everything outside the spliced value - key order, indentation, blank lines,
/// trailing newline - is preserved byte for byte.
fn replace_spec(content: &str, section: &str, name: &str, new_version: &str) -> Option<String> {
    let (mut start, end) = find_value_span(content, section, name)?;

    // An `npm:` alias value is `npm:<package>@<range>`; only the range is ours
    // to rewrite. Overwriting the whole value would drop the alias and point
    // the entry at a package that does not exist under the local key.
    let value = &content[start..end];
    if let Some(rest) = value.strip_prefix("npm:") {
        let at = rest
            .char_indices()
            .skip(1)
            .filter(|(_, c)| *c == '@')
            .map(|(i, _)| i)
            .last()?;
        start += "npm:".len() + at + 1;
    }

    if &content[start..end] == new_version {
        return None;
    }
    let mut out = String::with_capacity(content.len() + new_version.len());
    out.push_str(&content[..start]);
    out.push_str(new_version);
    out.push_str(&content[end..]);
    Some(out)
}

/// Byte span of the *contents* of the string value at `<root>.<section>.<name>`,
/// exclusive of the surrounding quotes.
///
/// A minimal JSON scanner: it tracks brace depth and the most recent key at
/// each depth, so it matches only a package key that is directly inside the
/// named top-level section - not a same-named key nested somewhere else, and
/// not an occurrence inside another string.
fn find_value_span(content: &str, section: &str, name: &str) -> Option<(usize, usize)> {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut depth = 0usize;
    let mut keys: Vec<&str> = vec![""; 4];
    let mut last_string: Option<(usize, usize)> = None;
    let mut expect_value = false;

    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                if keys.len() <= depth {
                    keys.resize(depth + 1, "");
                }
                last_string = None;
                expect_value = false;
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                last_string = None;
                expect_value = false;
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() {
                    match bytes[j] {
                        b'\\' => j += 2,
                        b'"' => break,
                        _ => j += 1,
                    }
                }
                let end = j.min(bytes.len());

                if expect_value {
                    if depth == 2 && keys[1] == section && keys[2] == name {
                        return Some((start, end));
                    }
                    expect_value = false;
                }
                last_string = Some((start, end));
                i = end.saturating_add(1);
            }
            b':' => {
                if let Some((s, e)) = last_string
                    && depth < keys.len()
                {
                    keys[depth] = &content[s..e];
                }
                last_string = None;
                expect_value = true;
                i += 1;
            }
            b',' => {
                expect_value = false;
                i += 1;
            }
            _ => {
                if !bytes[i].is_ascii_whitespace() {
                    expect_value = false;
                    last_string = None;
                }
                i += 1;
            }
        }
    }

    None
}

impl Default for FileUpdater {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct UpdateResult {
    pub modified_files: HashSet<PathBuf>,
}

impl UpdateResult {
    pub fn print_summary(&self) {
        if !self.modified_files.is_empty() {
            println!();
            println!("Run `npm install` to install updated packages");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use check_updates_core::{Dependency, Version, VersionSpec};
    use std::io::Write;
    use std::str::FromStr;
    use tempfile::NamedTempFile;

    fn create_check(
        name: &str,
        spec_str: &str,
        path: PathBuf,
        target_version: &str,
        severity: UpdateSeverity,
    ) -> DependencyCheck {
        create_check_in(
            "dependencies",
            name,
            spec_str,
            path,
            target_version,
            severity,
        )
    }

    fn create_check_in(
        section: &str,
        name: &str,
        spec_str: &str,
        path: PathBuf,
        target_version: &str,
        severity: UpdateSeverity,
    ) -> DependencyCheck {
        let target = Version::from_str(target_version).unwrap();
        DependencyCheck {
            dependency: Dependency {
                name: name.to_string(),
                version_spec: VersionSpec::parse(spec_str).unwrap(),
                source_file: path,
                line_number: Some(2),
                original_line: format!("\"{name}\": \"{spec_str}\""),
                manifest_key: None,
                section: Some(section.to_string()),
            },
            installed: Some(
                Version::from_str(spec_str.trim_start_matches('^').trim_start_matches('~'))
                    .unwrap(),
            ),
            in_range: Some(target.clone()),
            latest: target.clone(),
            target: Some(target.clone()),
            target_spec: Some(VersionSpec::parse(&format!("^{target_version}")).unwrap()),
            severity: Some(severity),
            force_spec: Some(VersionSpec::parse(&format!("^{target_version}")).unwrap()),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        }
    }

    #[test]
    fn test_update_patch_only() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(
            file,
            r#"{{
  "dependencies": {{
    "express": "^4.18.0",
    "lodash": "^4.17.0"
  }}
}}"#
        )?;
        file.flush()?;

        let temp_path = file.path().to_path_buf();

        let checks = vec![
            create_check(
                "express",
                "^4.18.0",
                temp_path.clone(),
                "4.18.2",
                UpdateSeverity::Patch,
            ),
            create_check(
                "lodash",
                "^4.17.0",
                temp_path.clone(),
                "4.18.0",
                UpdateSeverity::Minor,
            ),
        ];

        let updater = FileUpdater::new();
        updater.apply_updates(&checks, false, false)?;

        let content = fs::read_to_string(&temp_path)?;
        assert!(
            content.contains("4.18.2"),
            "express should be updated: {content}"
        );
        assert!(
            !content.contains("4.18.0") || content.contains("^4.18.0"),
            "lodash should NOT be updated"
        );

        Ok(())
    }

    #[test]
    fn pinned_spec_is_written_as_a_bare_npm_version() {
        let spec = VersionSpec::parse("4.18.2").unwrap();
        assert!(matches!(spec, VersionSpec::Pinned(_)), "{spec:?}");
        assert_eq!(to_npm_string(&spec).as_deref(), Some("4.18.2"));
    }

    #[test]
    fn ranges_use_npm_space_intersection_and_python_forms_are_refused() {
        let range = VersionSpec::parse(">=1.0.0,<2.0.0").unwrap();
        assert_eq!(to_npm_string(&range).as_deref(), Some(">=1.0.0 <2.0.0"));

        assert_eq!(to_npm_string(&VersionSpec::parse("~=1.2.3").unwrap()), None);
        assert_eq!(to_npm_string(&VersionSpec::Any), None);
        assert_eq!(
            to_npm_string(&VersionSpec::Complex("^1 || ^2".to_string())),
            None
        );

        assert_eq!(
            to_npm_string(&VersionSpec::parse("^1.2.3").unwrap()).as_deref(),
            Some("^1.2.3")
        );
        assert_eq!(
            to_npm_string(&VersionSpec::parse("~1.2.3").unwrap()).as_deref(),
            Some("~1.2.3")
        );
    }

    #[test]
    fn wildcard_is_rendered_from_the_advanced_prefix() {
        let spec = VersionSpec::parse("1.2.*").unwrap();
        assert_eq!(to_npm_string(&spec).as_deref(), Some("1.2.*"));

        // `with_version` keeps the old `pattern`; the rendering must not.
        let bumped = spec.with_version(&Version::from_str("1.3.0").unwrap());
        assert_eq!(to_npm_string(&bumped).as_deref(), Some("1.3.*"));
    }

    #[test]
    fn rewrite_preserves_key_order_and_layout() -> Result<()> {
        let source = "{\n\t\"name\": \"app\",\n\t\"version\": \"1.0.0\",\n\t\"scripts\": {\n\t\t\"build\": \"tsc\"\n\t},\n\t\"dependencies\": {\n\t\t\"express\": \"^4.18.0\",\n\t\t\"aaa\": \"^1.0.0\"\n\t}\n}\n";
        let mut file = NamedTempFile::new()?;
        write!(file, "{source}")?;
        file.flush()?;
        let path = file.path().to_path_buf();

        let checks = vec![create_check(
            "express",
            "^4.18.0",
            path.clone(),
            "4.18.2",
            UpdateSeverity::Patch,
        )];

        FileUpdater::new().apply_updates(&checks, false, false)?;

        let content = fs::read_to_string(&path)?;
        assert_eq!(
            content,
            source.replace("^4.18.0", "^4.18.2"),
            "only the version string may change"
        );
        assert!(
            content.find("\"name\"") < content.find("\"version\""),
            "key order must survive"
        );
        assert!(content.contains('\t'), "indentation must survive");

        Ok(())
    }

    #[test]
    fn peer_dependency_range_is_not_clobbered_by_a_dependencies_bump() -> Result<()> {
        let source = "{\n  \"dependencies\": {\n    \"react\": \"^18.0.0\"\n  },\n  \"peerDependencies\": {\n    \"react\": \"^17 || ^18\"\n  }\n}\n";
        let mut file = NamedTempFile::new()?;
        write!(file, "{source}")?;
        file.flush()?;
        let path = file.path().to_path_buf();

        let checks = vec![create_check_in(
            "dependencies",
            "react",
            "^18.0.0",
            path.clone(),
            "18.0.1",
            UpdateSeverity::Patch,
        )];

        FileUpdater::new().apply_updates(&checks, false, false)?;

        let content = fs::read_to_string(&path)?;
        assert!(content.contains("\"react\": \"^18.0.1\""), "{content}");
        assert!(
            content.contains("\"react\": \"^17 || ^18\""),
            "peer range must be untouched: {content}"
        );

        Ok(())
    }

    #[test]
    fn an_npm_alias_is_rewritten_under_its_local_key_and_keeps_the_alias() -> Result<()> {
        let source = "{\n  \"dependencies\": {\n    \"lodash4\": \"npm:lodash@^4.17.0\"\n  }\n}\n";
        let mut file = NamedTempFile::new()?;
        write!(file, "{source}")?;
        file.flush()?;
        let path = file.path().to_path_buf();

        let mut check = create_check(
            "lodash",
            "^4.17.0",
            path.clone(),
            "4.17.21",
            UpdateSeverity::Patch,
        );
        check.dependency.manifest_key = Some("lodash4".to_string());

        let result = FileUpdater::new().apply_updates(&[check], false, false)?;
        assert_eq!(result.modified_files.len(), 1);

        let content = fs::read_to_string(&path)?;
        assert_eq!(
            content, "{\n  \"dependencies\": {\n    \"lodash4\": \"npm:lodash@^4.17.21\"\n  }\n}\n",
            "the alias prefix must survive: {content}"
        );

        Ok(())
    }

    #[test]
    fn an_atomic_write_leaves_no_temporary_file_behind() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("package.json");
        fs::write(
            &path,
            "{\n  \"dependencies\": {\n    \"express\": \"^4.18.0\"\n  }\n}\n",
        )?;

        let checks = vec![create_check(
            "express",
            "^4.18.0",
            path.clone(),
            "4.18.2",
            UpdateSeverity::Patch,
        )];
        FileUpdater::new().apply_updates(&checks, false, false)?;

        assert!(fs::read_to_string(&path)?.contains("^4.18.2"));
        let leftovers: Vec<_> = fs::read_dir(dir.path())?
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n != "package.json")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        Ok(())
    }

    #[test]
    fn a_no_op_update_does_not_report_the_file_as_modified() -> Result<()> {
        let source = "{\n  \"dependencies\": {\n    \"express\": \"^4.18.2\"\n  }\n}\n";
        let mut file = NamedTempFile::new()?;
        write!(file, "{source}")?;
        file.flush()?;
        let path = file.path().to_path_buf();

        let checks = vec![create_check(
            "express",
            "^4.18.2",
            path.clone(),
            "4.18.2",
            UpdateSeverity::Patch,
        )];

        let result = FileUpdater::new().apply_updates(&checks, false, false)?;
        assert!(result.modified_files.is_empty());
        assert_eq!(fs::read_to_string(&path)?, source);

        Ok(())
    }
}
