use crate::types::{DependencyCheck, UpdateBlocker, UpdateSeverity};
use colored::Colorize;
use std::collections::HashSet;

/// Renders the dependency check results in a table format
pub struct TableRenderer {
    show_colors: bool,
}

impl TableRenderer {
    pub fn new(show_colors: bool) -> Self {
        Self { show_colors }
    }

    /// Render all packages with updates
    pub fn render(&self, checks: &[DependencyCheck], header: &str) {
        let checks_with_updates: Vec<&DependencyCheck> =
            checks.iter().filter(|check| check.has_update()).collect();

        self.render_deduped(&checks_with_updates, header);
    }

    /// Render a list of checks, one row per distinct `(name, target)` pair.
    ///
    /// The deduplication lives here rather than in the callers. All three CLIs
    /// report one `DependencyCheck` per *declaration*, so a crate declared by
    /// five workspace members produces five checks that differ only in which
    /// manifest they came from; the table has no column for the manifest, so
    /// without this they print as five identical rows. Each CLI used to
    /// hand-roll exactly this filter before calling in - the same key, the same
    /// `HashSet`, three copies - and ncu's was added only after the per
    /// declaration change made the duplicates visible. Doing it here is what
    /// the name has always claimed, and it cannot be forgotten by a new caller.
    ///
    /// The key is the name plus the *rendered* target, not the target itself:
    /// two declarations that resolve to the same version through different
    /// specs produce the same row, and one row is what should be printed. A
    /// declaration that resolves somewhere else keeps its own row, because the
    /// difference is visible in the table.
    ///
    /// What deliberately stays in the callers is the *filtering* - `has_update`
    /// and the `--update`/severity policy. That is a question about which
    /// dependencies the run is about, decided from CLI flags this function does
    /// not see, and it is not a display concern. Dedup is idempotent, so a
    /// caller that still filters and dedups is correct too, just redundant.
    pub fn render_deduped(&self, checks: &[&DependencyCheck], header: &str) {
        let mut seen: HashSet<(&str, String)> = HashSet::new();
        let deduped: Vec<&DependencyCheck> = checks
            .iter()
            .copied()
            .filter(|c| {
                let target = c
                    .target
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default();
                seen.insert((c.dependency.name.as_str(), target))
            })
            .collect();
        let checks: &[&DependencyCheck] = &deduped;

        if checks.is_empty() {
            println!("All dependencies are up to date!");
            return;
        }

        // Calculate column widths.
        //
        // These must be counted in `char`s, not bytes: the `{:<w$}` padding
        // below is implemented by `Display for str`, which delegates to
        // `Formatter::pad` and measures the string in `chars().count()`. Using
        // `str::len()` here would measure the same strings in bytes, so the two
        // sides of the alignment would disagree for any non-ASCII input and the
        // column would over-pad by the number of continuation bytes.
        //
        // This is char count, not terminal display width. A correct display
        // width (CJK names are one char but two columns; combining marks are
        // chars of zero width) cannot be expressed through `{:<w$}` at all -
        // `Formatter::pad` has no hook for a custom metric, so it would require
        // dropping the format-spec padding for hand-rolled padding plus a
        // `unicode-width` dependency. That is not worth paying for here:
        // crates.io, npm and PyPI all constrain package names to ASCII, and
        // semver / PEP 440 versions are ASCII too, so no string reaching this
        // renderer can differ between chars and columns.
        let max_name = checks
            .iter()
            .map(|c| c.dependency.name.chars().count())
            .max()
            .unwrap_or(0);

        let max_from = checks
            .iter()
            .map(|c| {
                c.current_version()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default()
                    .chars()
                    .count()
            })
            .max()
            .unwrap_or(0);

        // Widths are measured over exactly what `print_row` will print, empty
        // fallback included, rather than over the `Some` values only. The two
        // agree today - an absent version renders as `""`, which is never the
        // widest - but computing the width from a different expression than the
        // one that produces the cell is how a column silently stops lining up.
        let max_to = checks
            .iter()
            .map(|c| {
                c.target
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default()
                    .chars()
                    .count()
            })
            .max()
            .unwrap_or(0);

        println!("{header}\n");

        for check in checks {
            println!("{}", self.format_row(check, max_name, max_from, max_to));
        }

        let legend = self.blocker_legend(checks);
        if !legend.is_empty() {
            println!();
            for line in legend {
                println!("{line}");
            }
        }
    }

    /// Print one line per distinct reason `-u` cannot act on a row in the table.
    ///
    /// The rows themselves carry only a short marker, because the explanation
    /// is per-reason and not per-row: a package.json full of hyphen ranges
    /// would otherwise repeat the same sentence twenty times and push the
    /// version columns off the terminal. The legend is printed only when a
    /// marker was actually rendered, so an ordinary table is unchanged.
    ///
    /// `resolution-principles` rule 3: these rows stay in the table. The
    /// legend is what stops them reading as "an update `-u` will apply".
    pub fn blocker_legend(&self, checks: &[&DependencyCheck]) -> Vec<String> {
        let mut seen: Vec<UpdateBlocker> = Vec::new();
        for blocker in checks.iter().filter_map(|c| c.update_blocker()) {
            if !seen.contains(&blocker) {
                seen.push(blocker);
            }
        }

        seen.into_iter()
            .map(|blocker| {
                let count = checks
                    .iter()
                    .filter(|c| c.update_blocker() == Some(blocker))
                    .count();
                let marker = self.paint_marker(blocker.marker());
                format!("  {marker}  {count} row(s): {}", blocker.explanation())
            })
            .collect()
    }

    /// Markers are yellow, never red or green: they are neither a severity nor
    /// an error, and colouring them like one invites the reader to compare them
    /// with the severity column they sit next to.
    fn paint_marker(&self, marker: &str) -> String {
        if self.show_colors {
            marker.yellow().to_string()
        } else {
            marker.to_string()
        }
    }

    /// Build one row. Returned rather than printed so the marker placement can
    /// be asserted in a test without capturing stdout.
    fn format_row(
        &self,
        check: &DependencyCheck,
        name_width: usize,
        from_width: usize,
        to_width: usize,
    ) -> String {
        let from = check
            .current_version()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();

        let to = check
            .target
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();

        // The marker rides in the severity column rather than replacing it.
        // Both facts matter and they are independent: the severity says how big
        // the jump is, the marker says `-u` will not make it. Dropping the
        // severity to make room would hide the size of what the user now has to
        // apply by hand.
        let severity_str = match (self.format_severity(check.severity), check.update_blocker()) {
            (severity, None) => severity,
            (severity, Some(blocker)) if severity.is_empty() => self.paint_marker(blocker.marker()),
            (severity, Some(blocker)) => {
                format!("{severity} {}", self.paint_marker(blocker.marker()))
            }
        };

        let available_hint = if check.has_newer_available() {
            format!("  ({} available)", check.latest)
        } else {
            String::new()
        };

        // Built then trimmed rather than printed directly: the last column is
        // the severity, which is empty whenever `update_severity()` returns
        // `None`, so the row would end in the padding of the target column plus
        // the two-space separator - pure trailing blanks that show up in diffs
        // and in anything copy-pasted out of the terminal. Trimming the right
        // edge cannot disturb the columns, which are all to the left of it.
        // When the severity is present and coloured the row ends in the reset
        // escape, not whitespace, so nothing is trimmed there.
        let row = format!(
            "  {:<name_w$}  {:>from_w$} → {:<to_w$}  {}{}",
            check.dependency.name,
            from,
            to,
            severity_str,
            available_hint,
            name_w = name_width,
            from_w = from_width,
            to_w = to_width,
        );
        row.trim_end().to_string()
    }

    /// Format severity with optional colors
    pub fn format_severity(&self, severity: Option<UpdateSeverity>) -> String {
        match severity {
            Some(UpdateSeverity::Major) => {
                if self.show_colors {
                    "MAJOR".red().to_string()
                } else {
                    "MAJOR".to_string()
                }
            }
            Some(UpdateSeverity::Minor) => {
                if self.show_colors {
                    "minor".yellow().to_string()
                } else {
                    "minor".to_string()
                }
            }
            Some(UpdateSeverity::Patch) => {
                if self.show_colors {
                    "patch".green().to_string()
                } else {
                    "patch".to_string()
                }
            }
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TableRenderer;
    use crate::types::{Dependency, DependencyCheck, UpdateSeverity};
    use crate::version::{Version, VersionSpec};
    use std::path::PathBuf;

    fn check(spec: VersionSpec, rewritable: bool) -> DependencyCheck {
        let target = Version::new(1, 9, 0);
        DependencyCheck {
            dependency: Dependency {
                name: "react".to_string(),
                version_spec: spec,
                source_file: PathBuf::from("package.json"),
                line_number: Some(3),
                original_line: String::new(),
                manifest_key: None,
                section: Some("dependencies".to_string()),
            },
            installed: Some(Version::new(1, 2, 3)),
            in_range: None,
            latest: target.clone(),
            target: Some(target.clone()),
            target_spec: rewritable.then(|| VersionSpec::Caret(target.clone())),
            severity: Some(UpdateSeverity::Minor),
            force_spec: rewritable.then_some(VersionSpec::Caret(target)),
            installed_released_at: None,
            target_released_at: None,
            latest_released_at: None,
        }
    }

    /// The failure this exists to prevent: a row `-u` cannot write rendering
    /// exactly like one it will.
    #[test]
    fn blocked_row_is_marked_and_writable_row_is_not() {
        let renderer = TableRenderer::new(false);

        let writable = check(VersionSpec::Caret(Version::new(1, 2, 3)), true);
        let row = renderer.format_row(&writable, 8, 6, 6);
        assert!(
            row.ends_with("minor"),
            "an actionable row is unchanged: {row}"
        );

        let blocked = check(VersionSpec::Complex("1.x".to_string()), false);
        let row = renderer.format_row(&blocked, 8, 6, 6);
        assert!(
            row.contains("1.9.0"),
            "the row keeps its target - shown, not dropped: {row}"
        );
        assert!(
            row.contains("minor") && row.contains("[not updatable: spec]"),
            "severity and blocker are independent facts: {row}"
        );
    }

    /// The explanation is per-reason, printed once, with a count - not repeated
    /// on every row.
    #[test]
    fn legend_lists_each_reason_once_with_a_count() {
        let renderer = TableRenderer::new(false);
        let a = check(VersionSpec::Complex("1.x".to_string()), false);
        let b = check(VersionSpec::Complex("1.2.3 - 2.0.0".to_string()), false);
        let c = check(VersionSpec::Any, false);
        let ok = check(VersionSpec::Caret(Version::new(1, 2, 3)), true);

        let legend = renderer.blocker_legend(&[&a, &b, &c, &ok]);
        assert_eq!(legend.len(), 2, "one line per distinct reason: {legend:?}");
        assert!(legend[0].contains("2 row(s)"), "{legend:?}");
        assert!(legend[1].contains("1 row(s)"), "{legend:?}");

        assert!(
            renderer.blocker_legend(&[&ok]).is_empty(),
            "an ordinary table prints no legend"
        );
    }

    /// Guards the assumption documented in `render_deduped`: `{:<w$}` measures
    /// the padded string in `char`s, so column widths must be counted the same
    /// way. If this ever fails, the width computation needs to change with it.
    #[test]
    fn format_padding_is_measured_in_chars_not_bytes() {
        let name = "kaffé";
        assert_eq!(name.len(), 6, "test input must be multi-byte");
        assert_eq!(name.chars().count(), 5);

        let width = name.chars().count();
        let padded = format!("{name:<width$}|");
        assert_eq!(padded, "kaffé|", "char width must produce no extra padding");

        let byte_padded = format!("{name:<w$}|", w = name.len());
        assert_eq!(byte_padded, "kaffé |", "byte width over-pads by one column");
    }

    /// Pins the two halves of the row-trimming rule the renderers rely on: an
    /// empty last column leaves the row ending in pure blanks, and trimming
    /// them cannot eat anything from a row whose last column is non-empty.
    #[test]
    fn trailing_trim_removes_only_the_empty_last_column() {
        let empty_severity = format!("  {:<8}  {:>6} → {:<6}  {}", "serde", "1.0.2", "1.0.9", "");
        assert!(
            empty_severity.ends_with("  "),
            "an empty severity leaves trailing blanks"
        );
        assert_eq!(empty_severity.trim_end(), "  serde      1.0.2 → 1.0.9");

        let with_severity = format!(
            "  {:<8}  {:>6} → {:<6}  {}",
            "serde", "1.0.2", "1.0.9", "minor"
        );
        assert_eq!(
            with_severity.trim_end(),
            with_severity,
            "a populated severity column must survive untouched"
        );
    }
}
