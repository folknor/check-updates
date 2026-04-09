use check_updates_core::Version;
use std::collections::HashMap;
use std::process::Command;
use std::str::FromStr;

/// Information about the Python environment
#[derive(Debug, Clone)]
pub struct PythonInfo {
    /// Current Python version
    pub current: Version,
    /// Latest available Python version (from python.org)
    pub latest: Option<Version>,
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
}

/// Detect the current Python version
pub fn detect_python_version() -> Option<Version> {
    // Try python3 first, then python
    let commands = [
        ("python3", ["--version"]),
        ("python", ["--version"]),
    ];

    for (cmd, args) in &commands {
        if let Ok(output) = Command::new(cmd).args(args.as_slice()).output()
            && output.status.success() {
                let version_output = String::from_utf8_lossy(&output.stdout);
                // Output is like "Python 3.11.5"
                if let Some(version_str) = version_output
                    .trim()
                    .strip_prefix("Python ")
                    && let Ok(version) = Version::from_str(version_str) {
                        return Some(version);
                    }
            }
    }

    None
}

/// Fetch the latest Python version for a given series from uv python list
///
/// Uses uv's own list of available Python versions as the source of truth,
/// since endoflife.date may report versions that uv hasn't built yet.
pub fn fetch_latest_python_version(current: &Version) -> Option<Version> {
    let output = Command::new("uv").args(["python", "list"]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let current_series = format!("{}.{}", current.major, current.minor);

    // Find the highest version in the same series from uv's available list
    let mut best: Option<Version> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let name = parts[0];
        // Parse: "cpython-3.14.3-linux-x86_64-gnu"
        let name_parts: Vec<&str> = name.split('-').collect();
        if name_parts.len() < 2 || name_parts[0] != "cpython" {
            continue;
        }
        if name.contains("+freethreaded") {
            continue;
        }
        let version_str = name_parts[1];
        if let Ok(version) = Version::from_str(version_str) {
            let series = format!("{}.{}", version.major, version.minor);
            if series == current_series {
                if best.as_ref().is_none_or(|b| version > *b) {
                    best = Some(version);
                }
            }
        }
    }

    best
}

/// Build a map of series -> latest version from uv python list output
pub fn fetch_all_latest_python_versions() -> HashMap<String, Version> {
    let mut versions: HashMap<String, Version> = HashMap::new();

    let output = match Command::new("uv").args(["python", "list"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return versions,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let name = parts[0];
        let name_parts: Vec<&str> = name.split('-').collect();
        if name_parts.len() < 2 || name_parts[0] != "cpython" {
            continue;
        }
        if name.contains("+freethreaded") {
            continue;
        }
        let version_str = name_parts[1];
        if let Ok(version) = Version::from_str(version_str) {
            let series = format!("{}.{}", version.major, version.minor);
            let entry = versions.entry(series).or_insert_with(|| version.clone());
            if version > *entry {
                *entry = version;
            }
        }
    }

    versions
}

/// Get Python info (current version and optionally latest available)
pub fn get_python_info(check_latest: bool) -> Option<PythonInfo> {
    let current = detect_python_version()?;

    let latest = if check_latest {
        fetch_latest_python_version(&current)
    } else {
        None
    };

    Some(PythonInfo { current, latest })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_python_version() {
        // This test depends on Python being installed
        let version = detect_python_version();
        // We just check it returns something reasonable
        if let Some(v) = version {
            assert!(v.major >= 2);
        }
    }

    #[test]
    fn test_python_info_has_update() {
        let info = PythonInfo {
            current: Version::from_str("3.11.0").unwrap(),
            latest: Some(Version::from_str("3.13.1").unwrap()),
        };
        assert!(info.has_update());

        let info = PythonInfo {
            current: Version::from_str("3.13.1").unwrap(),
            latest: Some(Version::from_str("3.13.1").unwrap()),
        };
        assert!(!info.has_update());

        let info = PythonInfo {
            current: Version::from_str("3.11.0").unwrap(),
            latest: None,
        };
        assert!(!info.has_update());
    }
}
