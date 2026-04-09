use crate::global::{GlobalCheck, GlobalSource};
use check_updates_core::UpdateSeverity;
use colored::Colorize;

// Re-export TableRenderer from core for convenience
pub use check_updates_core::TableRenderer;

/// Renders global cargo crate check results
pub struct GlobalTableRenderer {
    show_colors: bool,
}

impl GlobalTableRenderer {
    pub fn new(show_colors: bool) -> Self {
        Self { show_colors }
    }

    /// Render the global results grouped by source type
    pub fn render(&self, checks: &[GlobalCheck]) {
        let registry_checks: Vec<&GlobalCheck> = checks
            .iter()
            .filter(|c| c.package.source == GlobalSource::Registry)
            .collect();
        let git_checks: Vec<&GlobalCheck> = checks
            .iter()
            .filter(|c| c.package.source == GlobalSource::Git)
            .collect();
        let path_checks: Vec<&GlobalCheck> = checks
            .iter()
            .filter(|c| c.package.source == GlobalSource::Path)
            .collect();

        let mut first_group = true;

        if !registry_checks.is_empty() {
            first_group = false;
            self.render_registry_group("crates.io:", &registry_checks);
        }

        if !git_checks.is_empty() {
            if !first_group {
                println!();
            }
            first_group = false;
            self.render_commits_group("git:", &git_checks, true);
        }

        if !path_checks.is_empty() {
            if !first_group {
                println!();
            }
            self.render_commits_group("local:", &path_checks, false);
        }
    }

    fn render_registry_group(&self, header: &str, checks: &[&GlobalCheck]) {
        let updates: Vec<&&GlobalCheck> = checks.iter().filter(|c| c.has_update).collect();

        println!("{header}");

        if updates.is_empty() {
            println!("  All packages up to date.");
        } else {
            self.render_registry_rows(&updates);
        }
    }

    fn render_registry_rows(&self, checks: &[&&GlobalCheck]) {
        let max_name = checks.iter().map(|c| c.package.name.len()).max().unwrap_or(0);
        let max_installed = checks
            .iter()
            .map(|c| c.package.installed_version.to_string().len())
            .max()
            .unwrap_or(0);
        let max_latest = checks
            .iter()
            .filter_map(|c| c.latest_version.as_ref())
            .map(|v| v.to_string().len())
            .max()
            .unwrap_or(0);

        let mut sorted = checks.to_vec();
        sorted.sort_by_key(|a| a.package.name.to_lowercase());

        for check in sorted {
            let latest_str = check
                .latest_version
                .as_ref()
                .map(std::string::ToString::to_string)
                .unwrap_or_default();

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
                "  {:<name_w$}  {:>inst_w$} → {:<to_w$}  {}",
                check.package.name,
                check.package.installed_version.to_string(),
                latest_str,
                severity_str,
                name_w = max_name,
                inst_w = max_installed,
                to_w = max_latest,
            );
        }
    }

    /// Render a group that shows commits behind (used for both git and path sources)
    fn render_commits_group(&self, header: &str, checks: &[&GlobalCheck], show_hash: bool) {
        let updates: Vec<&&GlobalCheck> = checks
            .iter()
            .filter(|c| c.has_update || c.has_dirty_changes)
            .collect();

        println!("{header}");

        if updates.is_empty() {
            println!("  All packages up to date.");
        } else {
            self.render_commits_rows(&updates, show_hash);
        }
    }

    fn render_commits_rows(&self, checks: &[&&GlobalCheck], show_hash: bool) {
        let max_name = checks.iter().map(|c| c.package.name.len()).max().unwrap_or(0);

        let mut sorted = checks.to_vec();
        sorted.sort_by_key(|a| a.package.name.to_lowercase());

        for check in sorted {
            let mut status_parts: Vec<String> = Vec::new();

            if let Some(n) = check.commits_behind {
                if n > 0 {
                    let behind_str = if n == 1 {
                        "1 commit behind".to_string()
                    } else {
                        format!("{n} commits behind")
                    };
                    if self.show_colors {
                        status_parts.push(behind_str.yellow().to_string());
                    } else {
                        status_parts.push(behind_str);
                    }
                }
            }

            if check.has_dirty_changes {
                let dirty = "dirty";
                if self.show_colors {
                    status_parts.push(dirty.red().to_string());
                } else {
                    status_parts.push(dirty.to_string());
                }
            }

            let status = status_parts.join(", ");

            if show_hash {
                let hash_str = check
                    .package
                    .git_hash
                    .as_deref()
                    .map(|h| &h[..7.min(h.len())])
                    .unwrap_or("???????");

                println!(
                    "  {:<name_w$}  {}  {}",
                    check.package.name,
                    hash_str,
                    status,
                    name_w = max_name,
                );
            } else {
                println!(
                    "  {:<name_w$}  {}",
                    check.package.name,
                    status,
                    name_w = max_name,
                );
            }
        }
    }
}
