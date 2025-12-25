use crate::global::{GlobalCheck, GlobalSource};
use crate::resolver::{DependencyCheck, UpdateSeverity};
use crate::uv_python::UvPythonCheck;
use colored::Colorize;
use std::collections::BTreeMap;

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

/// Column widths for global table layout (3 columns)
struct GlobalColumnWidths {
    package: usize,
    installed: usize,
    latest: usize,
}

/// Renders global package check results grouped by source
pub struct GlobalTableRenderer {
    show_colors: bool,
}

impl GlobalTableRenderer {
    pub fn new(show_colors: bool) -> Self {
        Self { show_colors }
    }

    /// Render the global results table grouped by source
    pub fn render(&self, checks: &[GlobalCheck]) {
        if checks.is_empty() {
            return;
        }

        // Group ALL checks by source (not just those with updates)
        let mut uv_checks: Vec<&GlobalCheck> = Vec::new();
        let mut pipx_checks: Vec<&GlobalCheck> = Vec::new();
        let mut pip_by_python: BTreeMap<String, Vec<&GlobalCheck>> = BTreeMap::new();

        for check in checks {
            match &check.package.source {
                GlobalSource::Uv => uv_checks.push(check),
                GlobalSource::Pipx => pipx_checks.push(check),
                GlobalSource::PipUser => {
                    let py_version = check
                        .package
                        .python_version
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    pip_by_python
                        .entry(py_version)
                        .or_insert_with(Vec::new)
                        .push(check);
                }
            }
        }

        let mut first_group = true;

        // Render uv tools
        if !uv_checks.is_empty() {
            if !first_group {
                println!();
            }
            first_group = false;
            self.render_group_or_uptodate("uv tools:", &uv_checks);
        }

        // Render pipx
        if !pipx_checks.is_empty() {
            if !first_group {
                println!();
            }
            first_group = false;
            self.render_group_or_uptodate("pipx:", &pipx_checks);
        }

        // Render pip --user grouped by Python version
        for (py_version, pip_checks) in &pip_by_python {
            if !first_group {
                println!();
            }
            first_group = false;
            let header = format!("pip --user (Python {}):", py_version);
            self.render_group_or_uptodate(&header, pip_checks);
        }
    }

    /// Render a group, showing "All packages up to date." if no updates
    fn render_group_or_uptodate(&self, header: &str, checks: &[&GlobalCheck]) {
        // Filter to only those with updates
        let updates: Vec<&GlobalCheck> = checks.iter().filter(|c| c.has_update).copied().collect();

        println!("{}", header);

        if updates.is_empty() {
            println!("  All packages up to date.");
        } else {
            let widths = self.calculate_widths(&updates);
            self.render_group_rows(&updates, &widths);
        }
    }

    fn render_group_rows(&self, checks: &[&GlobalCheck], widths: &GlobalColumnWidths) {
        // Print column headers (indented)
        println!(
            "  {:<pkg_w$}  {:>inst_w$}  {:>latest_w$}",
            "Package",
            "Installed",
            "Latest",
            pkg_w = widths.package,
            inst_w = widths.installed,
            latest_w = widths.latest,
        );

        // Sort checks by package name
        let mut sorted_checks = checks.to_vec();
        sorted_checks.sort_by(|a, b| a.package.name.to_lowercase().cmp(&b.package.name.to_lowercase()));

        // Print each row (indented)
        for check in sorted_checks {
            self.print_row(check, widths);
        }
    }

    fn calculate_widths(&self, checks: &[&GlobalCheck]) -> GlobalColumnWidths {
        let mut widths = GlobalColumnWidths {
            package: "Package".len(),
            installed: "Installed".len(),
            latest: "Latest".len(),
        };

        for check in checks {
            widths.package = widths.package.max(check.package.name.len());
            widths.installed = widths
                .installed
                .max(check.package.installed_version.to_string().len());
            widths.latest = widths.latest.max(check.latest.to_string().len());
        }

        widths
    }

    fn print_row(&self, check: &GlobalCheck, widths: &GlobalColumnWidths) {
        let latest_str = check.latest.to_string();
        let colored_latest = self.colorize(&latest_str, check.update_severity());

        println!(
            "  {:<pkg_w$}  {:>inst_w$}  {:>latest_w$}",
            check.package.name,
            check.package.installed_version.to_string(),
            colored_latest,
            pkg_w = widths.package,
            inst_w = widths.installed,
            latest_w = widths.latest,
        );
    }

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

/// Column widths for uv Python version table
struct UvPythonColumnWidths {
    series: usize,
    installed: usize,
    latest: usize,
}

/// Renders uv-managed Python version checks
pub struct UvPythonTableRenderer {
    show_colors: bool,
}

impl UvPythonTableRenderer {
    pub fn new(show_colors: bool) -> Self {
        Self { show_colors }
    }

    pub fn render(&self, checks: &[UvPythonCheck]) {
        if checks.is_empty() {
            return;
        }

        // Filter to only versions with updates
        let updates: Vec<&UvPythonCheck> = checks.iter().filter(|c| c.has_update).collect();

        println!("uv-managed Python installations:");

        if updates.is_empty() {
            println!("  All Python versions up to date.");
            return;
        }

        // Calculate column widths
        let widths = self.calculate_widths(&updates);

        // Print header (indented like global renderer)
        println!(
            "  {:<series_w$}  {:>installed_w$}  {:>latest_w$}",
            "Series",
            "Installed",
            "Latest",
            series_w = widths.series,
            installed_w = widths.installed,
            latest_w = widths.latest,
        );

        // Print rows sorted by series
        let mut sorted = updates.to_vec();
        sorted.sort_by(|a, b| a.series.cmp(&b.series));

        for check in sorted {
            self.print_row(check, &widths);
        }
    }

    fn calculate_widths(&self, checks: &[&UvPythonCheck]) -> UvPythonColumnWidths {
        let mut widths = UvPythonColumnWidths {
            series: "Series".len(),
            installed: "Installed".len(),
            latest: "Latest".len(),
        };

        for check in checks {
            widths.series = widths.series.max(check.series.len());
            widths.installed = widths
                .installed
                .max(check.installed_version.to_string().len());
            widths.latest = widths.latest.max(check.latest_version.to_string().len());
        }

        widths
    }

    fn print_row(&self, check: &UvPythonCheck, widths: &UvPythonColumnWidths) {
        let latest_str = check.latest_version.to_string();

        // Color the latest version based on update type
        let colored_latest = if self.show_colors {
            if check.is_patch_update() {
                latest_str.green().to_string()
            } else {
                // Minor/major update (rare for Python, but handle it)
                latest_str.yellow().to_string()
            }
        } else {
            latest_str
        };

        println!(
            "  {:<series_w$}  {:>installed_w$}  {:>latest_w$}",
            check.series,
            check.installed_version.to_string(),
            colored_latest,
            series_w = widths.series,
            installed_w = widths.installed,
            latest_w = widths.latest,
        );
    }
}
