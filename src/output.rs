use crate::resolver::{DependencyCheck, UpdateSeverity};
use colored::Colorize;

/// Column widths for table layout
struct ColumnWidths {
    package: usize,
    defined: usize,
    installed: usize,
    in_range: usize,
    latest: usize,
    update_to: usize,
}

/// Renders the dependency check results as a table
pub struct TableRenderer {
    show_colors: bool,
}

impl TableRenderer {
    pub fn new(show_colors: bool) -> Self {
        Self { show_colors }
    }

    /// Render the results table
    pub fn render(&self, checks: &[DependencyCheck]) {
        // Filter to only show rows with updates
        let checks_with_updates: Vec<&DependencyCheck> = checks
            .iter()
            .filter(|check| check.has_update())
            .collect();

        if checks_with_updates.is_empty() {
            return;
        }

        // Calculate column widths
        let widths = self.calculate_widths(&checks_with_updates);

        // Print header
        self.print_header(&widths);

        // Print each row
        for check in checks_with_updates {
            self.print_row(check, &widths);
        }
    }

    /// Calculate the maximum width needed for each column
    fn calculate_widths(&self, checks: &[&DependencyCheck]) -> ColumnWidths {
        let mut widths = ColumnWidths {
            package: "Package".len(),
            defined: "Defined".len(),
            installed: "Installed".len(),
            in_range: "In Range".len(),
            latest: "Latest".len(),
            update_to: "Update To".len(),
        };

        for check in checks {
            widths.package = widths.package.max(check.dependency.name.len());
            widths.defined = widths.defined.max(check.dependency.version_spec.to_string().len());

            let installed_str = check.installed.as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            widths.installed = widths.installed.max(installed_str.len());

            let in_range_str = check.in_range.as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            widths.in_range = widths.in_range.max(in_range_str.len());

            widths.latest = widths.latest.max(check.latest.to_string().len());

            let update_to_str = check.update_to.as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            widths.update_to = widths.update_to.max(update_to_str.len());
        }

        widths
    }

    /// Print the header
    fn print_header(&self, widths: &ColumnWidths) {
        println!(
            "{:<package_w$}  {:>defined_w$}  {:>installed_w$}  {:>in_range_w$}  {:>latest_w$}  {:>update_to_w$}",
            "Package",
            "Defined",
            "Installed",
            "In Range",
            "Latest",
            "Update To",
            package_w = widths.package,
            defined_w = widths.defined,
            installed_w = widths.installed,
            in_range_w = widths.in_range,
            latest_w = widths.latest,
            update_to_w = widths.update_to,
        );
    }

    /// Print a single row
    fn print_row(&self, check: &DependencyCheck, widths: &ColumnWidths) {
        let installed = check.installed.as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());

        let in_range = check.in_range.as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());

        let update_to = check.update_to.as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());

        // Get severity for coloring the update_to column
        let severity = check.update_severity();
        let colored_update = self.colorize(&update_to, severity);

        println!(
            "{:<package_w$}  {:>defined_w$}  {:>installed_w$}  {:>in_range_w$}  {:>latest_w$}  {:>update_to_w$}",
            check.dependency.name,
            check.dependency.version_spec.to_string(),
            installed,
            in_range,
            check.latest.to_string(),
            colored_update,
            package_w = widths.package,
            defined_w = widths.defined,
            installed_w = widths.installed,
            in_range_w = widths.in_range,
            latest_w = widths.latest,
            update_to_w = widths.update_to,
        );
    }

    /// Colorize text based on update severity
    fn colorize(&self, text: &str, severity: Option<UpdateSeverity>) -> String {
        if !self.show_colors {
            return text.to_string();
        }

        match severity {
            Some(UpdateSeverity::Major) => text.red().to_string(),
            Some(UpdateSeverity::Minor) => text.yellow().to_string(),
            Some(UpdateSeverity::Patch) => text.green().to_string(),
            None => text.to_string(),
        }
    }
}
