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
    pub fn render(&self, checks: &[DependencyCheck]) {
        let checks_with_updates: Vec<&DependencyCheck> = checks
            .iter()
            .filter(|check| check.has_update())
            .collect();

        self.render_deduped(&checks_with_updates);
    }

    /// Render a deduplicated list of checks
    pub fn render_deduped(&self, checks: &[&DependencyCheck]) {
        if checks.is_empty() {
            println!("All dependencies are up to date!");
            return;
        }

        // Calculate column widths
        let max_name = checks
            .iter()
            .map(|c| c.dependency.name.len())
            .max()
            .unwrap_or(0);

        let max_from = checks
            .iter()
            .filter_map(|c| c.current_version())
            .map(|v| v.to_string().len())
            .max()
            .unwrap_or(0);

        let max_to = checks
            .iter()
            .filter_map(|c| c.target.as_ref())
            .map(|v| v.to_string().len())
            .max()
            .unwrap_or(0);

        println!("Outdated dependencies:\n");

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
            .map(|v| v.to_string())
            .unwrap_or_default();

        let to = check
            .target
            .as_ref()
            .map(|v| v.to_string())
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
