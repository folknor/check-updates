use crate::types::{DependencyCheck, UpdateSeverity};
use colored::Colorize;

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

    /// Render a deduplicated list of checks
    pub fn render_deduped(&self, checks: &[&DependencyCheck], header: &str) {
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
            .filter_map(|c| c.current_version())
            .map(|v| v.to_string().chars().count())
            .max()
            .unwrap_or(0);

        let max_to = checks
            .iter()
            .filter_map(|c| c.target.as_ref())
            .map(|v| v.to_string().chars().count())
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

        println!(
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
}
