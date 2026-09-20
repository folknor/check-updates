use crate::global::{GlobalCheck, GlobalSource};
use check_updates_core::UpdateSeverity;
use colored::Colorize;

// Re-export TableRenderer from core for convenience
pub use check_updates_core::TableRenderer;

/// Stand-in for a git hash we could not read, and the width the hash column is
/// padded to. Seven characters, matching the short hash shown for packages we
/// could read.
const MISSING_HASH: &str = "???????";

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
        // Measured over exactly what the row printer emits, empty fallback
        // included, rather than over the `Some` values only. `has_update` is a
        // plain bool that nothing ties to `latest_version` being `Some`, so a
        // row with no latest version is not ruled out by the types; it renders
        // as an empty cell either way, but the width must come from the same
        // expression as the cell or the column can stop lining up.
        let max_latest = checks
            .iter()
            .map(|c| {
                c.latest_version
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default()
                    .chars()
                    .count()
            })
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

            // Trimmed on the right: the severity is the last column and is
            // empty when `update_severity()` returns `None`, which would leave
            // the row ending in the latest-version padding plus the separator.
            // Everything that carries meaning is to the left, so trimming
            // cannot disturb the alignment.
            let row = format!(
                "  {:<name_w$}  {:>inst_w$} → {:<to_w$}  {}",
                check.package.name,
                check.package.installed_version.to_string(),
                latest_str,
                severity_str,
                name_w = max_name,
                inst_w = max_installed,
                to_w = max_latest,
            );
            println!("{}", row.trim_end());
        }
    }

    /// Render a group that shows commits behind (used for both git and path sources)
    fn render_commits_group(&self, header: &str, checks: &[&GlobalCheck], show_hash: bool) {
        // A failed check is shown alongside real updates: leaving it out would
        // let "All packages up to date." cover a package we could not check.
        let updates: Vec<&&GlobalCheck> = checks
            .iter()
            .filter(|c| c.has_update || c.has_dirty_changes || c.check_failed)
            .collect();

        println!("{header}");

        if updates.is_empty() {
            println!("  All packages up to date.");
        } else {
            self.render_commits_rows(&updates, show_hash);
        }
    }

    fn render_commits_rows(&self, checks: &[&&GlobalCheck], show_hash: bool) {
        // Counted in `char`s to match `{:<w$}` padding - see
        // `render_registry_rows` above.
        let max_name = checks
            .iter()
            .map(|c| c.package.name.chars().count())
            .max()
            .unwrap_or(0);

        let mut sorted = checks.to_vec();
        sorted.sort_by_key(|a| a.package.name.to_lowercase());

        for check in sorted {
            let mut status_parts: Vec<String> = Vec::new();

            if check.check_failed {
                let unknown = "could not check";
                if self.show_colors {
                    status_parts.push(unknown.dimmed().to_string());
                } else {
                    status_parts.push(unknown.to_string());
                }
            }

            if let Some(n) = check.commits_behind
                && n > 0
                && !check.check_failed
            {
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
                    // Truncate to 7 characters on a char boundary: slicing
                    // `&h[..7]` would panic if byte 7 fell inside a codepoint.
                    // Git hashes are hex, but the value is read out of
                    // `.crates.toml`, which we do not control.
                    .map(|h| h.char_indices().nth(7).map_or(h, |(i, _)| &h[..i]))
                    .unwrap_or(MISSING_HASH);

                // Padded like every other column. The truncation above caps the
                // hash at 7 chars and the placeholder is 7 chars, so this is a
                // no-op for well-formed input - but a hash shorter than 7 chars
                // in `.crates.toml` would otherwise shift the status column of
                // that one row left and leave the group ragged.
                let row = format!(
                    "  {:<name_w$}  {:<hash_w$}  {}",
                    check.package.name,
                    hash_str,
                    status,
                    name_w = max_name,
                    hash_w = MISSING_HASH.chars().count(),
                );
                println!("{}", row.trim_end());
            } else {
                // Trimmed for the same reason as elsewhere: `status` is the
                // last column and can be empty, for instance when a package is
                // flagged as having an update but the commit count came back
                // absent.
                let row = format!(
                    "  {:<name_w$}  {}",
                    check.package.name,
                    status,
                    name_w = max_name,
                );
                println!("{}", row.trim_end());
            }
        }
    }
}
