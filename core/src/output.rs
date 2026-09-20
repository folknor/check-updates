use crate::types::{DependencyCheck, UpdateSeverity};
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
            self.print_row(check, max_name, max_from, max_to);
        }
    }

    fn print_row(
        &self,
        check: &DependencyCheck,
        name_width: usize,
        from_width: usize,
        to_width: usize,
    ) {
        let from = check
            .current_version()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();

        let to = check
            .target
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();

        let severity_str = self.format_severity(check.severity);

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
        println!("{}", row.trim_end());
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
