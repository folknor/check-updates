use crate::version::Version;
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
        if let Ok(output) = Command::new(cmd).args(args.as_slice()).output() {
            if output.status.success() {
                let version_output = String::from_utf8_lossy(&output.stdout);
                // Output is like "Python 3.11.5"
                if let Some(version_str) = version_output
                    .trim()
                    .strip_prefix("Python ")
                {
                    if let Ok(version) = Version::from_str(version_str) {
                        return Some(version);
                    }
                }
            }
        }
    }

    None
}

/// Fetch the latest Python version from endoflife.date
pub async fn fetch_latest_python_version() -> Option<Version> {
    // Use the endoflife.date API - it's well-maintained and returns clean data
    let url = "https://endoflife.date/api/python.json";

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let response = client.get(url).send().await.ok()?;

    if !response.status().is_success() {
        return None;
    }

    let json: serde_json::Value = response.json().await.ok()?;

    // The API returns an array of release cycles sorted by newest first
    // Each cycle has a "latest" field with the latest version for that cycle
    let cycles = json.as_array()?;

    // Get the latest version from the first cycle (newest Python release line)
    // Filter to only consider Python 3.x cycles
    for cycle in cycles {
        let cycle_name = cycle.get("cycle")?.as_str()?;
        if cycle_name.starts_with("3.") {
            let latest_str = cycle.get("latest")?.as_str()?;
            if let Ok(version) = Version::from_str(latest_str) {
                return Some(version);
            }
        }
    }

    None
}

/// Get Python info (current version and optionally latest available)
pub async fn get_python_info(check_latest: bool) -> Option<PythonInfo> {
    let current = detect_python_version()?;

    let latest = if check_latest {
        fetch_latest_python_version().await
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
