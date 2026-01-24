use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum VersionError {
    #[error("Invalid version string: {0}")]
    InvalidVersion(String),
    #[error("Invalid version specifier: {0}")]
    InvalidSpecifier(String),
}

/// A parsed semantic version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre_release: Option<String>,
    /// Local version segment (Python) or build metadata (Cargo)
    pub local: Option<String>,
    /// Original string representation
    pub original: String,
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.major == other.major
            && self.minor == other.minor
            && self.patch == other.patch
            && self.pre_release == other.pre_release
    }
}

impl Eq for Version {}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre_release: None,
            local: None,
            original: format!("{}.{}.{}", major, minor, patch),
        }
    }

    /// Check if this is a pre-release version
    pub fn is_prerelease(&self) -> bool {
        self.pre_release.is_some()
    }

    /// Check if this version is in the same major series as another
    pub fn same_major(&self, other: &Version) -> bool {
        self.major == other.major
    }

    /// Check if this version is in the same minor series as another
    pub fn same_minor(&self, other: &Version) -> bool {
        self.major == other.major && self.minor == other.minor
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        // Handle local version separator (+)
        let (version_part, local) = if let Some(idx) = s.find('+') {
            (&s[..idx], Some(s[idx + 1..].to_string()))
        } else {
            (s, None)
        };

        // Handle pre-release separators (-, a, b, rc, alpha, beta, dev, post)
        let (base_part, pre_release) = parse_prerelease(version_part);

        // Parse the base version (major.minor.patch)
        let parts: Vec<&str> = base_part.split('.').collect();

        let major = parts
            .first()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| VersionError::InvalidVersion(s.to_string()))?;

        let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

        let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

        Ok(Version {
            major,
            minor,
            patch,
            pre_release,
            local,
            original: s.to_string(),
        })
    }
}

fn parse_prerelease(s: &str) -> (&str, Option<String>) {
    // Common pre-release patterns
    let patterns = [
        "dev", "post", "alpha", "beta", "rc", "a", "b", "c", "-",
    ];

    for pattern in patterns {
        if let Some(idx) = s.to_lowercase().find(pattern) {
            if idx > 0 {
                return (&s[..idx], Some(s[idx..].to_string()));
            }
        }
    }

    (s, None)
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.major.cmp(&other.major) {
            Ordering::Equal => {}
            ord => return ord,
        }
        match self.minor.cmp(&other.minor) {
            Ordering::Equal => {}
            ord => return ord,
        }
        match self.patch.cmp(&other.patch) {
            Ordering::Equal => {}
            ord => return ord,
        }

        // Pre-release versions are less than release versions
        match (&self.pre_release, &other.pre_release) {
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
            (None, None) => Ordering::Equal,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.original)
    }
}

/// Version specification (constraint)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionSpec {
    /// ==1.2.3
    Pinned(Version),
    /// >=1.2.3
    Minimum(Version),
    /// <=1.2.3
    Maximum(Version),
    /// >1.2.3
    GreaterThan(Version),
    /// <1.2.3
    LessThan(Version),
    /// >=1.2.3,<2.0.0
    Range { min: Version, max: Version },
    /// ^1.2.3 (caret - same major)
    Caret(Version),
    /// ~1.2.3 (tilde - same minor)
    Tilde(Version),
    /// ~=1.2.3 (compatible release - Python)
    Compatible(Version),
    /// ==1.2.*
    Wildcard { prefix: String, pattern: String },
    /// !=1.2.3
    NotEqual(Version),
    /// Complex constraint we store as raw string
    Complex(String),
    /// Any version (no constraint or *)
    Any,
}

impl VersionSpec {
    /// Parse a version specifier string
    pub fn parse(s: &str) -> Result<Self, VersionError> {
        let s = s.trim();

        if s.is_empty() || s == "*" {
            return Ok(VersionSpec::Any);
        }

        // Handle caret notation (poetry/pdm style)
        if let Some(version_str) = s.strip_prefix('^') {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::Caret(version));
        }

        // Handle tilde notation
        if let Some(version_str) = s.strip_prefix("~=") {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::Compatible(version));
        }
        if let Some(version_str) = s.strip_prefix('~') {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::Tilde(version));
        }

        // Handle wildcard
        if s.contains('*') {
            if let Some(prefix) = s.strip_prefix("==") {
                return Ok(VersionSpec::Wildcard {
                    prefix: prefix.replace(".*", "").replace("*", ""),
                    pattern: s.to_string(),
                });
            }
            return Ok(VersionSpec::Wildcard {
                prefix: s.replace(".*", "").replace("*", ""),
                pattern: s.to_string(),
            });
        }

        // Handle range (>=X,<Y)
        if s.contains(',') {
            let parts: Vec<&str> = s.split(',').collect();
            if parts.len() == 2 {
                let min_part = parts[0].trim();
                let max_part = parts[1].trim();

                if let (Some(min_str), Some(max_str)) = (
                    min_part.strip_prefix(">="),
                    max_part.strip_prefix('<'),
                ) {
                    let min = Version::from_str(min_str)?;
                    let max = Version::from_str(max_str)?;
                    return Ok(VersionSpec::Range { min, max });
                }
            }
            // Complex constraint
            return Ok(VersionSpec::Complex(s.to_string()));
        }

        // Handle simple operators
        if let Some(version_str) = s.strip_prefix("==") {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::Pinned(version));
        }
        if let Some(version_str) = s.strip_prefix(">=") {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::Minimum(version));
        }
        if let Some(version_str) = s.strip_prefix("<=") {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::Maximum(version));
        }
        if let Some(version_str) = s.strip_prefix("!=") {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::NotEqual(version));
        }
        if let Some(version_str) = s.strip_prefix('>') {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::GreaterThan(version));
        }
        if let Some(version_str) = s.strip_prefix('<') {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::LessThan(version));
        }

        // No operator - treat as pinned or complex
        if let Ok(version) = Version::from_str(s) {
            return Ok(VersionSpec::Pinned(version));
        }

        Ok(VersionSpec::Complex(s.to_string()))
    }

    /// Check if a version satisfies this constraint
    pub fn satisfies(&self, version: &Version) -> bool {
        match self {
            VersionSpec::Any => true,
            VersionSpec::Pinned(v) => version == v,
            VersionSpec::Minimum(v) => version >= v,
            VersionSpec::Maximum(v) => version <= v,
            VersionSpec::GreaterThan(v) => version > v,
            VersionSpec::LessThan(v) => version < v,
            VersionSpec::Range { min, max } => version >= min && version < max,
            VersionSpec::Caret(v) => {
                // Caret: ^1.2.3 means >=1.2.3 <2.0.0
                // But for 0.x: ^0.1.2 means >=0.1.2 <0.2.0
                // And for 0.0.x: ^0.0.3 means =0.0.3
                if version < v {
                    return false;
                }
                if v.major == 0 {
                    if v.minor == 0 {
                        // ^0.0.z means =0.0.z
                        version.major == 0 && version.minor == 0 && version.patch == v.patch
                    } else {
                        // ^0.y.z means >=0.y.z <0.(y+1).0
                        version.major == 0 && version.minor == v.minor
                    }
                } else {
                    // ^x.y.z means >=x.y.z <(x+1).0.0
                    version.major == v.major
                }
            }
            VersionSpec::Tilde(v) => {
                version >= v && version.major == v.major && version.minor == v.minor
            }
            VersionSpec::Compatible(v) => {
                version >= v && version.major == v.major && version.minor == v.minor
            }
            VersionSpec::Wildcard { prefix, .. } => {
                version.original.starts_with(prefix)
            }
            VersionSpec::NotEqual(v) => version != v,
            VersionSpec::Complex(_) => true, // Can't evaluate complex constraints
        }
    }

    /// Get the base version from the spec (for comparison)
    pub fn base_version(&self) -> Option<&Version> {
        match self {
            VersionSpec::Pinned(v)
            | VersionSpec::Minimum(v)
            | VersionSpec::Maximum(v)
            | VersionSpec::GreaterThan(v)
            | VersionSpec::LessThan(v)
            | VersionSpec::Caret(v)
            | VersionSpec::Tilde(v)
            | VersionSpec::Compatible(v)
            | VersionSpec::NotEqual(v) => Some(v),
            VersionSpec::Range { min, .. } => Some(min),
            VersionSpec::Wildcard { .. } | VersionSpec::Complex(_) | VersionSpec::Any => None,
        }
    }

    /// Get the maximum allowed major version (for "in range" calculation)
    pub fn max_major(&self) -> Option<u64> {
        match self {
            VersionSpec::Range { max, .. } => Some(max.major),
            VersionSpec::Caret(v) => Some(v.major),
            VersionSpec::LessThan(v) | VersionSpec::Maximum(v) => Some(v.major),
            // For unbounded specs, we assume same major (semver)
            VersionSpec::Minimum(v)
            | VersionSpec::GreaterThan(v)
            | VersionSpec::Pinned(v)
            | VersionSpec::Compatible(v)
            | VersionSpec::Tilde(v) => Some(v.major),
            VersionSpec::NotEqual(v) => Some(v.major),
            VersionSpec::Wildcard { prefix, .. } => {
                prefix.split('.').next().and_then(|s| s.parse().ok())
            }
            VersionSpec::Complex(_) | VersionSpec::Any => None,
        }
    }

    /// Get the version string without operators (for Cargo.toml format)
    /// Returns just "1.0.0" instead of "==1.0.0"
    pub fn version_string(&self) -> Option<String> {
        match self {
            VersionSpec::Pinned(v)
            | VersionSpec::Minimum(v)
            | VersionSpec::Maximum(v)
            | VersionSpec::GreaterThan(v)
            | VersionSpec::LessThan(v)
            | VersionSpec::Caret(v)
            | VersionSpec::Tilde(v)
            | VersionSpec::Compatible(v)
            | VersionSpec::NotEqual(v) => Some(v.to_string()),
            VersionSpec::Range { min, .. } => Some(min.to_string()),
            VersionSpec::Wildcard { prefix, .. } => Some(format!("{}.*", prefix)),
            VersionSpec::Complex(s) => Some(s.clone()),
            VersionSpec::Any => None,
        }
    }

    /// Create a new version spec with updated version but same constraint type
    pub fn with_version(&self, new_version: &Version) -> VersionSpec {
        match self {
            VersionSpec::Pinned(_) => VersionSpec::Pinned(new_version.clone()),
            VersionSpec::Minimum(_) => VersionSpec::Minimum(new_version.clone()),
            VersionSpec::Maximum(_) => VersionSpec::Maximum(new_version.clone()),
            VersionSpec::GreaterThan(_) => VersionSpec::GreaterThan(new_version.clone()),
            VersionSpec::LessThan(_) => VersionSpec::LessThan(new_version.clone()),
            VersionSpec::Range { max, .. } => {
                // If new min would exceed max, update max to next major
                if new_version >= max {
                    VersionSpec::Range {
                        min: new_version.clone(),
                        max: Version::new(new_version.major + 1, 0, 0),
                    }
                } else {
                    VersionSpec::Range {
                        min: new_version.clone(),
                        max: max.clone(),
                    }
                }
            }
            VersionSpec::Caret(_) => VersionSpec::Caret(new_version.clone()),
            VersionSpec::Tilde(_) => VersionSpec::Tilde(new_version.clone()),
            VersionSpec::Compatible(_) => VersionSpec::Compatible(new_version.clone()),
            VersionSpec::Wildcard { pattern, .. } => {
                // Update wildcard to new major.minor.*
                VersionSpec::Wildcard {
                    prefix: format!("{}.{}", new_version.major, new_version.minor),
                    pattern: pattern.clone(),
                }
            }
            VersionSpec::NotEqual(_) => VersionSpec::NotEqual(new_version.clone()),
            VersionSpec::Complex(s) => VersionSpec::Complex(s.clone()),
            VersionSpec::Any => VersionSpec::Any,
        }
    }
}

impl fmt::Display for VersionSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionSpec::Any => write!(f, "*"),
            VersionSpec::Pinned(v) => write!(f, "=={}", v),
            VersionSpec::Minimum(v) => write!(f, ">={}", v),
            VersionSpec::Maximum(v) => write!(f, "<={}", v),
            VersionSpec::GreaterThan(v) => write!(f, ">{}", v),
            VersionSpec::LessThan(v) => write!(f, "<{}", v),
            VersionSpec::Range { min, max } => write!(f, ">={},<{}", min, max),
            VersionSpec::Caret(v) => write!(f, "^{}", v),
            VersionSpec::Tilde(v) => write!(f, "~{}", v),
            VersionSpec::Compatible(v) => write!(f, "~={}", v),
            VersionSpec::Wildcard { prefix, .. } => write!(f, "=={}.*", prefix),
            VersionSpec::NotEqual(v) => write!(f, "!={}", v),
            VersionSpec::Complex(s) => write!(f, "{}", s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        let v = Version::from_str("1.2.3").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);

        let v = Version::from_str("2.0").unwrap();
        assert_eq!(v.major, 2);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn test_version_comparison() {
        let v1 = Version::from_str("1.2.3").unwrap();
        let v2 = Version::from_str("1.2.4").unwrap();
        let v3 = Version::from_str("2.0.0").unwrap();

        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v1 < v3);
    }

    #[test]
    fn test_parse_version_spec() {
        assert!(matches!(
            VersionSpec::parse("==1.2.3").unwrap(),
            VersionSpec::Pinned(_)
        ));
        assert!(matches!(
            VersionSpec::parse(">=1.2.3").unwrap(),
            VersionSpec::Minimum(_)
        ));
        assert!(matches!(
            VersionSpec::parse("^1.2.3").unwrap(),
            VersionSpec::Caret(_)
        ));
        assert!(matches!(
            VersionSpec::parse(">=1.0.0,<2.0.0").unwrap(),
            VersionSpec::Range { .. }
        ));
    }

    #[test]
    fn test_satisfies() {
        let spec = VersionSpec::parse(">=1.0.0,<2.0.0").unwrap();
        assert!(spec.satisfies(&Version::from_str("1.5.0").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("2.0.0").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("0.9.0").unwrap()));
    }
}
