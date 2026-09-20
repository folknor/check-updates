use crate::global::UpgradeCommand;
use anyhow::Result;
use check_updates_core::Version;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::OnceLock;

/// Why `uv python list` did not produce a usable listing.
///
/// Callers that previously saw an empty vector for every one of these cases now
/// get a distinguishable error, so a broken `uv` no longer looks like a machine
/// with no Pythons. `NotInstalled` is the benign case: rendering it as an error
/// to the user would be noise on a machine that simply does not use uv.
#[derive(Debug, thiserror::Error)]
pub enum UvPythonError {
    #[error("`uv` is not installed or not on PATH")]
    NotInstalled,
    #[error("`uv python list` failed ({status}): {stderr}")]
    CommandFailed { status: String, stderr: String },
    #[error(
        "`uv python list` returned no download-available rows, so the latest available Python per series is unknown (is `--only-installed`, `UV_PYTHON_DOWNLOADS=never` or `--offline` in effect?)"
    )]
    NoDownloadBaseline,
}

/// Cached outcome of a single `uv python list` invocation.
#[derive(Debug, Clone)]
enum UvListOutcome {
    Ok(String),
    NotInstalled,
    Failed { status: String, stderr: String },
}

static UV_PYTHON_LIST: OnceLock<UvListOutcome> = OnceLock::new();

/// Run `uv python list` once per process and hand out the cached outcome.
///
/// The cache stores the *failure* as well as the success, so caching does not
/// turn "uv is broken" into "uv said nothing": every caller sees the same
/// explicit error rather than an empty listing.
fn uv_python_list_outcome() -> &'static UvListOutcome {
    UV_PYTHON_LIST.get_or_init(
        || match Command::new("uv").args(["python", "list"]).output() {
            Ok(o) if o.status.success() => {
                UvListOutcome::Ok(String::from_utf8_lossy(&o.stdout).into_owned())
            }
            Ok(o) => UvListOutcome::Failed {
                status: o
                    .status
                    .code()
                    .map_or_else(|| "signal".to_string(), |c| c.to_string()),
                stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => UvListOutcome::NotInstalled,
            Err(e) => UvListOutcome::Failed {
                status: "spawn failed".to_string(),
                stderr: e.to_string(),
            },
        },
    )
}

/// The raw stdout of `uv python list`, or why it is not available.
pub fn uv_python_list() -> std::result::Result<&'static str, UvPythonError> {
    match uv_python_list_outcome() {
        UvListOutcome::Ok(s) => Ok(s.as_str()),
        UvListOutcome::NotInstalled => Err(UvPythonError::NotInstalled),
        UvListOutcome::Failed { status, stderr } => Err(UvPythonError::CommandFailed {
            status: status.clone(),
            stderr: stderr.clone(),
        }),
    }
}

/// Information about an installed uv-managed Python version
#[derive(Debug, Clone, Serialize)]
pub struct UvPythonInfo {
    /// Full implementation name (e.g., "cpython-3.11.5-linux-x86_64-gnu")
    pub full_name: String,
    /// Python version (e.g., "3.11.5")
    pub version: Version,
    /// Installation path (if installed, otherwise None)
    pub path: Option<PathBuf>,
    /// Whether this is installed or just available for download
    pub is_installed: bool,
    /// Whether this interpreter lives in uv's own Python install directory.
    /// A `/usr/bin/python3.12` row is listed by `uv python list` but is a system
    /// interpreter: `uv python install` would add a separate uv copy rather than
    /// upgrade it, so the distinction has to survive into the output.
    pub is_uv_managed: bool,
    /// Python implementation type (cpython, pypy, graalpy, etc.)
    pub implementation: String,
}

/// Does this column look like an interpreter path rather than a status marker?
fn is_path_like(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('~') || s.contains(std::path::MAIN_SEPARATOR)
}

/// Root of uv's managed Python installations, if it can be determined.
fn uv_python_install_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("UV_PYTHON_INSTALL_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    dirs::data_dir().map(|d| d.join("uv").join("python"))
}

/// Is this interpreter path inside uv's managed Python directory?
fn path_is_uv_managed(path: &Path) -> bool {
    if let Some(root) = uv_python_install_dir()
        && path.starts_with(&root)
    {
        return true;
    }
    // Fallback for non-default layouts: an adjacent `uv/python` pair anywhere in
    // the path. Cheaper than shelling out to `uv python dir`.
    let comps: Vec<_> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    comps.windows(2).any(|w| w[0] == "uv" && w[1] == "python")
}

/// Result of checking a Python series for updates
#[derive(Debug, Clone, Serialize)]
pub struct UvPythonCheck {
    /// The major.minor series (e.g., "3.11")
    pub series: String,
    /// Currently installed version in this series
    pub installed_version: Version,
    /// Latest available patch in this series from endoflife.date
    pub latest_version: Version,
    /// Whether an update is available
    pub has_update: bool,
    /// Full uv python info for the installed version
    pub python_info: UvPythonInfo,
}

impl UvPythonCheck {
    /// Get update severity for coloring (patch or minor)
    pub fn is_patch_update(&self) -> bool {
        self.has_update
            && self.latest_version.major == self.installed_version.major
            && self.latest_version.minor == self.installed_version.minor
    }
}

/// Discovery and checking for uv-managed Python installations
pub struct UvPythonDiscovery {}

impl Default for UvPythonDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl UvPythonDiscovery {
    pub fn new() -> Self {
        Self {}
    }

    /// Parse `uv python list` output to find installed Python versions
    pub(crate) fn parse_uv_python_list(&self, output: &str) -> Result<Vec<UvPythonInfo>> {
        let mut versions = Vec::new();

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            let full_name = parts[0];

            // Installed rows carry an interpreter path; download rows carry the
            // "<download available>" marker instead.
            let is_installed = !line.contains("<download available>")
                && parts.get(1).is_some_and(|p| is_path_like(p));

            // Parse: "cpython-3.11.5-linux-x86_64-gnu"
            let name_parts: Vec<&str> = full_name.split('-').collect();
            if name_parts.len() < 2 {
                continue;
            }

            let implementation = name_parts[0]; // "cpython", "pypy", etc.
            let version_str = name_parts[1]; // "3.11.5"

            // Skip freethreaded variants for simplicity
            if full_name.contains("+freethreaded") {
                continue;
            }

            // Skip non-cpython for now (can extend later)
            if implementation != "cpython" {
                continue;
            }

            if let Ok(version) = Version::from_str(version_str) {
                // Only treat the second column as a path when it looks like one.
                // Taking `parts[1]` unconditionally stored whatever uv happened
                // to print there (a marker, a `->` arrow) in a field that is
                // serialised into the JSON envelope.
                let path = if is_installed {
                    parts
                        .get(1)
                        .filter(|p| is_path_like(p))
                        .map(|p| PathBuf::from(*p))
                } else {
                    None
                };
                let is_uv_managed = path.as_deref().is_some_and(path_is_uv_managed);

                versions.push(UvPythonInfo {
                    full_name: full_name.to_string(),
                    version,
                    path,
                    is_installed,
                    is_uv_managed,
                    implementation: implementation.to_string(),
                });
            }
        }

        Ok(versions)
    }

    /// Build latest available versions per series from uv python list output
    fn latest_versions_from_uv_list(
        &self,
        all_versions: &[UvPythonInfo],
    ) -> HashMap<String, Version> {
        let mut latest: HashMap<String, Version> = HashMap::new();
        for info in all_versions {
            let series = format!("{}.{}", info.version.major, info.version.minor);
            let entry = latest.entry(series).or_insert_with(|| info.version.clone());
            if info.version > *entry {
                *entry = info.version.clone();
            }
        }
        latest
    }

    /// Build the per-series checks from an already parsed listing.
    ///
    /// Split out from [`Self::discover_and_check`] so the whole decision path is
    /// testable from a fixture without running `uv`.
    fn checks_from_versions(
        &self,
        all_versions: Vec<UvPythonInfo>,
    ) -> std::result::Result<Vec<UvPythonCheck>, UvPythonError> {
        // The "latest available" baseline is only meaningful if the listing
        // actually contains not-yet-installed builds. uv includes those by
        // default, but `--only-installed`, `UV_PYTHON_DOWNLOADS=never` and
        // `--offline` all suppress them - and then latest always equals
        // installed and every Python silently reports as up to date. Refuse to
        // report rather than emit that false negative.
        if !all_versions.iter().any(|v| !v.is_installed) {
            return Err(UvPythonError::NoDownloadBaseline);
        }

        let latest_versions = self.latest_versions_from_uv_list(&all_versions);

        // Pick the *newest* installed build per series, not the first row uv
        // happened to print. Where two builds share a version, prefer the
        // uv-managed one so the reported path and name describe uv's copy.
        let mut best_installed: HashMap<String, UvPythonInfo> = HashMap::new();
        for python in all_versions.into_iter().filter(|v| v.is_installed) {
            let series = format!("{}.{}", python.version.major, python.version.minor);
            match best_installed.get(&series) {
                Some(existing)
                    if existing.version > python.version
                        || (existing.version == python.version && existing.is_uv_managed) => {}
                _ => {
                    best_installed.insert(series, python);
                }
            }
        }

        let mut checks: Vec<UvPythonCheck> = best_installed
            .into_iter()
            .filter_map(|(series, python)| {
                let latest = latest_versions.get(&series)?;
                Some(UvPythonCheck {
                    series: series.clone(),
                    installed_version: python.version.clone(),
                    latest_version: latest.clone(),
                    has_update: latest > &python.version,
                    python_info: python,
                })
            })
            .collect();

        checks.sort_by(|a, b| a.installed_version.cmp(&b.installed_version));
        Ok(checks)
    }

    /// Discover installed uv Python versions and check for updates
    ///
    /// Errors are [`UvPythonError`] values: a missing or broken `uv` is no
    /// longer reported as "no Pythons found".
    pub async fn discover_and_check(&self) -> Result<Vec<UvPythonCheck>> {
        let stdout = uv_python_list()?;
        let all_versions = self.parse_uv_python_list(stdout)?;
        Ok(self.checks_from_versions(all_versions)?)
    }
}

/// Generate upgrade commands for outdated uv Python versions
pub fn generate_uv_python_upgrade_commands(checks: &[UvPythonCheck]) -> Vec<UpgradeCommand> {
    let mut commands = Vec::new();

    let outdated: Vec<_> = checks.iter().filter(|c| c.has_update).collect();

    if outdated.is_empty() {
        return commands;
    }

    // Generate: uv python install 3.11.14
    for check in outdated {
        if !check.python_info.is_uv_managed {
            // The outdated interpreter in this series is a system Python that uv
            // merely found on PATH. `uv python install` will not upgrade it - it
            // installs a separate uv-managed copy - so say so instead of letting
            // the command imply an in-place upgrade.
            commands.push(UpgradeCommand::Comment(format!(
                "Python {} in series {} is a system interpreter ({}); the command below installs a separate uv-managed copy",
                check.installed_version,
                check.series,
                check
                    .python_info
                    .path
                    .as_ref()
                    .map_or_else(|| "path unknown".to_string(), |p| p.display().to_string()),
            )));
        }
        commands.push(UpgradeCommand::Command(format!(
            "uv python install {}",
            check.latest_version
        )));
    }

    commands
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uv_python_list() {
        let discovery = UvPythonDiscovery::new();
        let output = r#"cpython-3.11.5-linux-x86_64-gnu     /home/user/.local/share/uv/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11
cpython-3.12.2-linux-x86_64-gnu     /usr/bin/python3.12
cpython-3.13.0-linux-x86_64-gnu     <download available>
"#;
        let versions = discovery.parse_uv_python_list(output).unwrap();

        // Should have 3 versions total (2 installed, 1 download available)
        assert_eq!(versions.len(), 3);

        assert_eq!(versions[0].version.to_string(), "3.11.5");
        assert_eq!(versions[0].implementation, "cpython");
        assert!(versions[0].is_installed);
        assert!(versions[0].path.is_some());

        assert_eq!(versions[1].version.to_string(), "3.12.2");
        assert!(versions[1].is_installed);
        assert!(versions[1].path.is_some());

        assert_eq!(versions[2].version.to_string(), "3.13.0");
        assert!(!versions[2].is_installed);
        assert!(versions[2].path.is_none());
    }

    #[test]
    fn test_parse_uv_python_list_skip_freethreaded() {
        let discovery = UvPythonDiscovery::new();
        let output = r#"cpython-3.13.0+freethreaded-linux-x86_64-gnu     /path/to/python
cpython-3.12.2-linux-x86_64-gnu     /usr/bin/python3.12
"#;
        let versions = discovery.parse_uv_python_list(output).unwrap();

        // Should only have 1 version (freethreaded skipped)
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "3.12.2");
    }

    #[test]
    fn test_parse_uv_python_list_skip_non_cpython() {
        let discovery = UvPythonDiscovery::new();
        let output = r#"pypy-3.10.14-linux-x86_64-gnu     /path/to/pypy
cpython-3.12.2-linux-x86_64-gnu     /usr/bin/python3.12
"#;
        let versions = discovery.parse_uv_python_list(output).unwrap();

        // Should only have cpython version
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].implementation, "cpython");
        assert_eq!(versions[0].version.to_string(), "3.12.2");
    }

    /// A listing with two installed builds in one series, ascending, plus a
    /// download row for the baseline.
    const ASCENDING_LISTING: &str = "cpython-3.11.5-linux-x86_64-gnu    /home/u/.local/share/uv/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11\ncpython-3.11.14-linux-x86_64-gnu    /home/u/.local/share/uv/python/cpython-3.11.14-linux-x86_64-gnu/bin/python3.11\ncpython-3.11.14-linux-x86_64-gnu    <download available>\n";

    #[test]
    fn reports_newest_installed_in_series_not_first_row() {
        let discovery = UvPythonDiscovery::new();
        let versions = discovery.parse_uv_python_list(ASCENDING_LISTING).unwrap();
        let checks = discovery.checks_from_versions(versions).unwrap();

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].installed_version.to_string(), "3.11.14");
        assert!(!checks[0].has_update);
    }

    #[test]
    fn system_interpreter_is_not_reported_as_uv_managed() {
        let discovery = UvPythonDiscovery::new();
        let output = "cpython-3.12.2-linux-x86_64-gnu    /usr/bin/python3.12\ncpython-3.12.12-linux-x86_64-gnu    <download available>\n";
        let versions = discovery.parse_uv_python_list(output).unwrap();
        let checks = discovery.checks_from_versions(versions).unwrap();

        assert_eq!(checks.len(), 1);
        assert!(!checks[0].python_info.is_uv_managed);

        let commands = generate_uv_python_upgrade_commands(&checks);
        assert_eq!(commands.len(), 2);
        assert!(matches!(commands[0], UpgradeCommand::Comment(_)));
        assert!(matches!(commands[1], UpgradeCommand::Command(_)));
    }

    #[test]
    fn uv_managed_path_is_recognised() {
        let discovery = UvPythonDiscovery::new();
        let versions = discovery.parse_uv_python_list(ASCENDING_LISTING).unwrap();
        assert!(
            versions
                .iter()
                .filter(|v| v.is_installed)
                .all(|v| v.is_uv_managed)
        );
    }

    #[test]
    fn listing_without_download_rows_is_an_error_not_up_to_date() {
        let discovery = UvPythonDiscovery::new();
        let output = "cpython-3.12.2-linux-x86_64-gnu    /usr/bin/python3.12\n";
        let versions = discovery.parse_uv_python_list(output).unwrap();
        let err = discovery.checks_from_versions(versions).unwrap_err();
        assert!(matches!(err, UvPythonError::NoDownloadBaseline));
    }

    #[test]
    fn non_path_second_column_is_not_stored_as_a_path() {
        let discovery = UvPythonDiscovery::new();
        let output = "cpython-3.13.0-linux-x86_64-gnu    <download available>\ncpython-3.12.2-linux-x86_64-gnu    some-marker\n";
        let versions = discovery.parse_uv_python_list(output).unwrap();
        let marker_row = versions
            .iter()
            .find(|v| v.version.to_string() == "3.12.2")
            .unwrap();
        assert!(marker_row.path.is_none());
        assert!(!marker_row.is_installed);
    }

    #[test]
    fn test_generate_upgrade_commands() {
        let checks = vec![
            UvPythonCheck {
                series: "3.11".to_string(),
                installed_version: Version::from_str("3.11.5").unwrap(),
                latest_version: Version::from_str("3.11.14").unwrap(),
                has_update: true,
                python_info: UvPythonInfo {
                    full_name: "cpython-3.11.5-linux-x86_64-gnu".to_string(),
                    version: Version::from_str("3.11.5").unwrap(),
                    path: None,
                    is_installed: true,
                    is_uv_managed: true,
                    implementation: "cpython".to_string(),
                },
            },
            UvPythonCheck {
                series: "3.12".to_string(),
                installed_version: Version::from_str("3.12.2").unwrap(),
                latest_version: Version::from_str("3.12.12").unwrap(),
                has_update: true,
                python_info: UvPythonInfo {
                    full_name: "cpython-3.12.2-linux-x86_64-gnu".to_string(),
                    version: Version::from_str("3.12.2").unwrap(),
                    path: None,
                    is_installed: true,
                    is_uv_managed: true,
                    implementation: "cpython".to_string(),
                },
            },
        ];

        let commands = generate_uv_python_upgrade_commands(&checks);
        assert_eq!(commands.len(), 2);

        match &commands[0] {
            UpgradeCommand::Command(cmd) => {
                assert_eq!(cmd, "uv python install 3.11.14");
            }
            _ => panic!("Expected Command"),
        }

        match &commands[1] {
            UpgradeCommand::Command(cmd) => {
                assert_eq!(cmd, "uv python install 3.12.12");
            }
            _ => panic!("Expected Command"),
        }
    }

    #[test]
    fn test_is_patch_update() {
        let check = UvPythonCheck {
            series: "3.11".to_string(),
            installed_version: Version::from_str("3.11.5").unwrap(),
            latest_version: Version::from_str("3.11.14").unwrap(),
            has_update: true,
            python_info: UvPythonInfo {
                full_name: "cpython-3.11.5-linux-x86_64-gnu".to_string(),
                version: Version::from_str("3.11.5").unwrap(),
                path: None,
                is_installed: true,
                is_uv_managed: true,
                implementation: "cpython".to_string(),
            },
        };

        assert!(check.is_patch_update());

        // No update
        let check_no_update = UvPythonCheck {
            series: "3.11".to_string(),
            installed_version: Version::from_str("3.11.14").unwrap(),
            latest_version: Version::from_str("3.11.14").unwrap(),
            has_update: false,
            python_info: UvPythonInfo {
                full_name: "cpython-3.11.14-linux-x86_64-gnu".to_string(),
                version: Version::from_str("3.11.14").unwrap(),
                path: None,
                is_installed: true,
                is_uv_managed: true,
                implementation: "cpython".to_string(),
            },
        };

        assert!(!check_no_update.is_patch_update());
    }
}
