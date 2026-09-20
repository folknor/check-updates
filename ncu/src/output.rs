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

        // `GlobalSource` has a single variant, `Npm`, and discovery only ever
        // reads `npm ls -g`, so every check belongs to one group. The grouping
        // map and the `first_group` blank-line bookkeeping that used to live
        // here had nothing to separate - hence the `let _ = first_group;` that
        // was needed to silence the unused-assignment warning.
        //
        // Multi-source globals (pnpm/yarn global installs) would be a real
        // feature, but it is a discovery feature first: the grouping is the
        // trivial half and belongs with the code that produces a second source,
        // not ahead of it. Restore the separator logic then, modelled on
        // `ccu::output::GlobalTableRenderer::render`, which does have three
        // sources and does need it.
        let npm_checks: Vec<&GlobalCheck> = checks
            .iter()
            .filter(|c| c.package.source == GlobalSource::Npm)
            .collect();

        if !npm_checks.is_empty() {
            self.render_group_or_uptodate("npm global:", &npm_checks);
        }
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
        // Widths are counted in `char`s to match the `{:<w$}` padding below,
        // which goes through `Formatter::pad` and measures `chars().count()`.
        // See `check_updates_core::output` for the full reasoning, including
        // why display width (`unicode-width`) is deliberately not used.
        let max_name = checks
            .iter()
            .map(|c| c.package.name.chars().count())
            .max()
            .unwrap_or(0);
        let max_installed = checks
            .iter()
            .map(|c| c.package.installed_version.to_string().chars().count())
            .max()
            .unwrap_or(0);
        let max_latest = checks
            .iter()
            .map(|c| c.latest.to_string().chars().count())
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

            // Trimmed on the right: the severity is the last column and is
            // empty when `update_severity()` returns `None`, which would leave
            // the row ending in the latest-version padding plus the separator -
            // trailing blanks that show up in diffs and copy-pasted output.
            // The columns are all to the left, so trimming cannot disturb them.
            let row = format!(
                "  {:<name_w$}  {:>inst_w$} \u{2192} {:<to_w$}  {}",
                check.package.name,
                check.package.installed_version.to_string(),
                check.latest.to_string(),
                severity_str,
                name_w = max_name,
                inst_w = max_installed,
                to_w = max_latest,
            );
            println!("{}", row.trim_end());
        }
    }
}
