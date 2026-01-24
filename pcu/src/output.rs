use crate::global::{GlobalCheck, GlobalSource};
use crate::uv_python::UvPythonCheck;
use check_updates_core::UpdateSeverity;
use colored::Colorize;
use std::collections::BTreeMap;

// Re-export TableRenderer from core for convenience
pub use check_updates_core::TableRenderer;

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
            self.render_group_rows(&updates);
        }
    }

    fn render_group_rows(&self, checks: &[&GlobalCheck]) {
        // Calculate widths
        let max_name = checks.iter().map(|c| c.package.name.len()).max().unwrap_or(0);
        let max_installed = checks
            .iter()
            .map(|c| c.package.installed_version.to_string().len())
            .max()
            .unwrap_or(0);
        let max_latest = checks
            .iter()
            .map(|c| c.latest.to_string().len())
            .max()
            .unwrap_or(0);

        // Sort checks by package name
        let mut sorted_checks = checks.to_vec();
        sorted_checks.sort_by(|a, b| a.package.name.to_lowercase().cmp(&b.package.name.to_lowercase()));

        // Print each row (indented)
        for check in sorted_checks {
            let severity_str = match check.update_severity() {
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
                None => "".to_string(),
            };

            println!(
                "  {:<name_w$}  {:>inst_w$} → {:<to_w$}  {}",
                check.package.name,
                check.package.installed_version.to_string(),
                check.latest.to_string(),
                severity_str,
                name_w = max_name,
                inst_w = max_installed,
                to_w = max_latest,
            );
        }
    }
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
        let max_series = updates.iter().map(|c| c.series.len()).max().unwrap_or(0);
        let max_installed = updates
            .iter()
            .map(|c| c.installed_version.to_string().len())
            .max()
            .unwrap_or(0);

        // Print rows sorted by series
        let mut sorted = updates.to_vec();
        sorted.sort_by(|a, b| a.series.cmp(&b.series));

        for check in sorted {
            let severity_str = if self.show_colors {
                if check.is_patch_update() {
                    "patch".green().to_string()
                } else {
                    "minor".yellow().to_string()
                }
            } else if check.is_patch_update() {
                "patch".to_string()
            } else {
                "minor".to_string()
            };

            println!(
                "  {:<series_w$}  {:>inst_w$} → {}  {}",
                check.series,
                check.installed_version.to_string(),
                check.latest_version.to_string(),
                severity_str,
                series_w = max_series,
                inst_w = max_installed,
            );
        }
    }
}
