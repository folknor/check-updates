use crate::global::{GlobalCheck, GlobalSource};
use check_updates_core::UpdateSeverity;
use colored::Colorize;

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

        // Group by source (currently only npm, but extensible)
        let mut by_source: std::collections::HashMap<&GlobalSource, Vec<&GlobalCheck>> =
            std::collections::HashMap::new();

        for check in checks {
            by_source
                .entry(&check.package.source)
                .or_default()
                .push(check);
        }

        let mut first_group = true;

        if let Some(npm_checks) = by_source.get(&GlobalSource::Npm) {
            if !first_group {
                println!();
            }
            first_group = false;
            self.render_group_or_uptodate("npm global:", npm_checks);
        }

        // Suppress unused variable warning — first_group will be used when more sources are added
        let _ = first_group;
    }

    fn render_group_or_uptodate(&self, header: &str, checks: &[&GlobalCheck]) {
        let updates: Vec<&GlobalCheck> = checks.iter().filter(|c| c.has_update).copied().collect();

        println!("{header}");

        if updates.is_empty() {
            println!("  All packages up to date.");
        } else {
            self.render_group_rows(&updates);
        }
    }

    fn render_group_rows(&self, checks: &[&GlobalCheck]) {
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

        let mut sorted_checks = checks.to_vec();
        sorted_checks.sort_by_key(|a| a.package.name.to_lowercase());

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
                None => String::new(),
            };

            println!(
                "  {:<name_w$}  {:>inst_w$} \u{2192} {:<to_w$}  {}",
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
