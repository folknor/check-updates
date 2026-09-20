use crate::uv_python::{UvPythonDiscovery, uv_python_list};
use check_updates_core::Version;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

/// Where the reported "current" Python came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonSource {
    /// An active virtualenv (`VIRTUAL_ENV`) or a project-local `.venv`
    Venv(PathBuf),
    /// Whatever `python3`/`python` resolves to on PATH
    Path(String),
}

/// Information about the Python environment
#[derive(Debug, Clone)]
pub struct PythonInfo {
    /// Current Python version
    pub current: Version,
    /// Which interpreter `current` was read from
    pub source: PythonSource,
    /// Latest available patch *within the current major.minor series*.
    ///
    /// This is deliberately not the newest Python in existence: it is the
    /// version you get by upgrading in place. Callers must not render its
    /// absence, or equality with `current`, as "(latest)" without qualification
    /// - see `latest_overall`.
    pub latest: Option<Version>,
    /// Newest version uv knows about across *all* series. When this is greater
    /// than `latest`, the interpreter is up to date within its series but a
    /// newer series exists.
    pub latest_overall: Option<Version>,
    /// Why the latest-version lookup could not run, if it could not. When this
    /// is `Some`, `latest` being `None` means "unknown", not "up to date".
    pub latest_unknown_reason: Option<String>,
}

impl PythonInfo {
    /// Check if an update is available
    pub fn has_update(&self) -> bool {
        if let Some(ref latest) = self.latest {
            latest > &self.current
        } else {
            false
        }
    }

    /// Is the interpreter current within its series, with a newer series out?
    pub fn newer_series_available(&self) -> bool {
        match (&self.latest, &self.latest_overall) {
            (Some(latest), Some(overall)) => overall > latest,
            _ => false,
        }
    }
}

/// The interpreter of the active virtualenv, if there is one.
fn venv_interpreter() -> Option<PathBuf> {
    let roots = std::env::var("VIRTUAL_ENV")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .chain(std::iter::once(PathBuf::from(".venv")));

    for root in roots {
        for rel in ["bin/python", "Scripts/python.exe"] {
            let candidate = root.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Read `--version` from one interpreter.
///
/// Python 2 prints `--version` to *stderr*, not stdout, so both streams have to
/// be inspected: reading stdout only made the `python` fallback - which exists
/// precisely to catch Python 2 - unable to ever succeed.
fn version_of(program: &std::ffi::OsStr) -> Option<Version> {
    let output = Command::new(program).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    for stream in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(stream);
        if let Some(version_str) = text.trim().strip_prefix("Python ")
            && let Ok(version) = Version::from_str(version_str.split_whitespace().next()?)
        {
            return Some(version);
        }
    }
    None
}

/// Detect the current Python version
///
/// Prefers the project's virtualenv over PATH: the header this feeds sits above
/// a project's dependency table, so reporting a global `python3` there would
/// describe a different environment than the one the table is about.
pub fn detect_python_version() -> Option<(Version, PythonSource)> {
    if let Some(path) = venv_interpreter()
        && let Some(version) = version_of(path.as_os_str())
    {
        return Some((version, PythonSource::Venv(path)));
    }

    for cmd in ["python3", "python"] {
        if let Some(version) = version_of(std::ffi::OsStr::new(cmd)) {
            return Some((version, PythonSource::Path(cmd.to_string())));
        }
    }

    None
}

/// Latest versions uv knows about: (latest in `current`'s series, latest overall).
///
/// Uses uv's own list of available Python versions as the source of truth,
/// since endoflife.date may report versions that uv hasn't built yet. The
/// listing is fetched once per process and shared with `uv_python.rs`.
pub fn fetch_latest_python_versions(
    current: &Version,
) -> Result<(Option<Version>, Option<Version>), String> {
    let stdout = uv_python_list().map_err(|e| e.to_string())?;
    let discovery = UvPythonDiscovery::new();
    let all = discovery
        .parse_uv_python_list(stdout)
        .map_err(|e| e.to_string())?;

    let current_series = format!("{}.{}", current.major, current.minor);
    let mut in_series: Option<Version> = None;
    let mut overall: Option<Version> = None;
    for info in &all {
        let series = format!("{}.{}", info.version.major, info.version.minor);
        if series == current_series && in_series.as_ref().is_none_or(|b| info.version > *b) {
            in_series = Some(info.version.clone());
        }
        if overall.as_ref().is_none_or(|b| info.version > *b) {
            overall = Some(info.version.clone());
        }
    }

    Ok((in_series, overall))
}

/// Get Python info (current version and optionally latest available)
pub fn get_python_info(check_latest: bool) -> Option<PythonInfo> {
    let (current, source) = detect_python_version()?;

    let (latest, latest_overall, latest_unknown_reason) = if check_latest {
        match fetch_latest_python_versions(&current) {
            Ok((latest, overall)) => (latest, overall, None),
            Err(reason) => (None, None, Some(reason)),
        }
    } else {
        (None, None, None)
    };

    Some(PythonInfo {
        current,
        source,
        latest,
        latest_overall,
        latest_unknown_reason,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_python_version() {
        // This test depends on Python being installed
        let version = detect_python_version();
        // We just check it returns something reasonable
        if let Some((v, _)) = version {
            assert!(v.major >= 2);
        }
    }

    fn info(current: &str, latest: Option<&str>, overall: Option<&str>) -> PythonInfo {
        PythonInfo {
            current: Version::from_str(current).unwrap(),
            source: PythonSource::Path("python3".to_string()),
            latest: latest.map(|v| Version::from_str(v).unwrap()),
            latest_overall: overall.map(|v| Version::from_str(v).unwrap()),
            latest_unknown_reason: None,
        }
    }

    #[test]
    fn test_python_info_has_update() {
        assert!(info("3.11.0", Some("3.13.1"), None).has_update());
        assert!(!info("3.13.1", Some("3.13.1"), None).has_update());
        assert!(!info("3.11.0", None, None).has_update());
    }

    #[test]
    fn newer_series_is_distinguished_from_out_of_date() {
        let up_to_date_in_series = info("3.11.14", Some("3.11.14"), Some("3.14.1"));
        assert!(!up_to_date_in_series.has_update());
        assert!(up_to_date_in_series.newer_series_available());

        let newest = info("3.14.1", Some("3.14.1"), Some("3.14.1"));
        assert!(!newest.newer_series_available());
    }

    #[test]
    fn unknown_latest_carries_a_reason() {
        let mut i = info("3.11.14", None, None);
        i.latest_unknown_reason = Some("`uv` is not installed or not on PATH".to_string());
        assert!(!i.has_update());
        assert!(i.latest_unknown_reason.is_some());
    }
}
