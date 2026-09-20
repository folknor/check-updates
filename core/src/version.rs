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
#[derive(Debug, Clone)]
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

impl Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Version::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        // Defined in terms of `Ord` so that equality and ordering can never
        // disagree about two pre-release strings that normalize to the same
        // identifier sequence (`1.2.3-rc1` and `1.2.3rc1`).
        self.cmp(other) == Ordering::Equal
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
            original: format!("{major}.{minor}.{patch}"),
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

        // Split the numeric release core from whatever follows it. The release
        // core is the longest leading `\d+(\.\d+)*`; the remainder must be a
        // recognizable pre-release suffix or the whole string is rejected.
        let (base_part, suffix) = split_release(version_part);
        if base_part.is_empty() {
            return Err(VersionError::InvalidVersion(s.to_string()));
        }
        let pre_release =
            parse_prerelease(suffix).ok_or_else(|| VersionError::InvalidVersion(s.to_string()))?;

        // Parse the base version (major.minor.patch). Every segment here is a
        // digit run by construction, so a parse failure means numeric overflow
        // and is an error - never a silent `0`.
        let mut parts = base_part.split('.');

        let mut next_segment = |required: bool| -> Result<u64, VersionError> {
            match parts.next() {
                Some(seg) => seg
                    .parse()
                    .map_err(|_| VersionError::InvalidVersion(s.to_string())),
                None if required => Err(VersionError::InvalidVersion(s.to_string())),
                None => Ok(0),
            }
        };

        let major = next_segment(true)?;
        let minor = next_segment(false)?;
        let patch = next_segment(false)?;

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

/// Split off the leading numeric release core, `\d+(\.\d+)*`.
///
/// Returns `(release, remainder)`. A `.` is only consumed when it is followed
/// by a digit, so `1.0.0.post1` yields `("1.0.0", ".post1")` and `1.x` yields
/// `("1", ".x")`. All scanning is on bytes of the original string - no
/// lowercased copy is produced, so no byte index taken from one string is ever
/// used to slice another.
fn split_release(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut end = 0;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            end = i;
            // Continue only across a dot that introduces another digit run.
            if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                i += 1;
                continue;
            }
            break;
        }
        break;
    }

    (&s[..end], &s[end..])
}

/// Pre-release / post-release markers recognized after the numeric core.
///
/// This is the PEP 440 set plus the spellings seen in the wild. Anything else
/// that is not introduced by a semver `-` is rejected rather than being
/// absorbed as junk.
const PRE_RELEASE_MARKERS: [&str; 12] = [
    "dev", "post", "alpha", "beta", "preview", "pre", "rev", "rc", "a", "b", "c", "r",
];

fn is_identifier_body(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// Classify the remainder left by [`split_release`].
///
/// `None` means "this is not a version at all" - the caller turns it into a
/// parse error. `Some(None)` means there is no pre-release part.
///
/// The returned string is normalized: the introducing separator (`-`, `.` or
/// `_`) is stripped, so `1.2.3-rc1` and `1.2.3rc1` both yield `"rc1"` and the
/// comparison in [`compare_prerelease`] sees one consistent form.
fn parse_prerelease(suffix: &str) -> Option<Option<String>> {
    if suffix.is_empty() {
        return Some(None);
    }

    // Semver: everything after the first `-` is the pre-release, whatever it
    // spells (`1.2.3-1`, `1.2.3-pre`, `1.2.3-alpha.1`).
    if let Some(rest) = suffix.strip_prefix('-') {
        return if is_identifier_body(rest) {
            Some(Some(rest.to_string()))
        } else {
            None
        };
    }

    // PEP 440 and the compact semver-adjacent forms: an optional `.`/`_`
    // separator followed by a known marker word.
    let rest = suffix
        .strip_prefix('.')
        .or_else(|| suffix.strip_prefix('_'))
        .unwrap_or(suffix);

    if !is_identifier_body(rest) {
        return None;
    }

    let word_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    if word_len == 0 {
        return None;
    }
    let word = &rest[..word_len];
    if PRE_RELEASE_MARKERS
        .iter()
        .any(|m| m.eq_ignore_ascii_case(word))
    {
        Some(Some(rest.to_string()))
    } else {
        None
    }
}

/// Rank of a known alphabetic marker, lowest first.
///
/// Semver alone would compare identifiers lexically, which puts `dev` above
/// `beta` and `alpha` - the wrong order for every ecosystem that uses these
/// words. Unknown words fall back to lexical comparison among themselves and
/// sort above all known markers.
fn marker_rank(word: &str) -> Option<u8> {
    let rank = match word.to_ascii_lowercase().as_str() {
        "dev" => 0,
        "alpha" | "a" => 1,
        "beta" | "b" => 2,
        "pre" | "preview" | "c" => 3,
        "rc" => 4,
        "post" | "rev" | "r" => 5,
        _ => return None,
    };
    Some(rank)
}

/// One comparable unit of a pre-release string.
#[derive(PartialEq, Eq)]
enum PreSegment<'a> {
    Numeric(u64),
    Alpha(&'a str),
}

/// Split a pre-release string into comparable segments.
///
/// Separators (`.`, `-`, `_`) delimit segments, and a letter/digit transition
/// inside a segment also splits, so `rc1` tokenizes exactly like `rc.1`.
fn prerelease_segments(s: &str) -> Vec<PreSegment<'_>> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'.' || b == b'-' || b == b'_' {
            i += 1;
        } else if b.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // An overflowing numeric identifier degrades to an alpha segment
            // rather than panicking or silently becoming 0.
            match s[start..i].parse::<u64>() {
                Ok(n) => out.push(PreSegment::Numeric(n)),
                Err(_) => out.push(PreSegment::Alpha(&s[start..i])),
            }
        } else {
            let start = i;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'.' || c == b'-' || c == b'_' || c.is_ascii_digit() {
                    break;
                }
                i += 1;
            }
            out.push(PreSegment::Alpha(&s[start..i]));
        }
    }

    out
}

/// Compare two pre-release strings by identifier, not by raw string.
///
/// Numeric identifiers compare numerically and rank below alphabetic ones
/// (semver rule 11); alphabetic identifiers compare by [`marker_rank`] first so
/// `alpha < beta < rc`. A shorter identifier list is smaller when it is a
/// prefix of the longer one, so `rc < rc.1`.
fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let left = prerelease_segments(a);
    let right = prerelease_segments(b);

    for (l, r) in left.iter().zip(right.iter()) {
        let ord = match (l, r) {
            (PreSegment::Numeric(x), PreSegment::Numeric(y)) => x.cmp(y),
            (PreSegment::Numeric(_), PreSegment::Alpha(_)) => Ordering::Less,
            (PreSegment::Alpha(_), PreSegment::Numeric(_)) => Ordering::Greater,
            (PreSegment::Alpha(x), PreSegment::Alpha(y)) => {
                match (marker_rank(x), marker_rank(y)) {
                    (Some(rx), Some(ry)) => rx.cmp(&ry),
                    // A known marker is always "earlier" than an unknown word.
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()),
                }
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }

    left.len().cmp(&right.len())
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
            (Some(a), Some(b)) => compare_prerelease(a, b),
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

                if let (Some(min_str), Some(max_str)) =
                    (min_part.strip_prefix(">="), max_part.strip_prefix('<'))
                {
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
                // PEP 440: ~=X.Y means >=X.Y, <(X+1).0.0 (lock major only)
                //          ~=X.Y.Z means >=X.Y.Z, <X.(Y+1).0 (lock major+minor)
                let dot_count = v.original.chars().filter(|c| *c == '.').count();
                if dot_count < 2 {
                    // ~=X.Y form: only lock on major
                    version >= v && version.major == v.major
                } else {
                    // ~=X.Y.Z form: lock on major+minor
                    version >= v && version.major == v.major && version.minor == v.minor
                }
            }
            VersionSpec::Wildcard { prefix, .. } => {
                // Must match prefix followed by a dot (or end), so 1.2.* doesn't match 1.20.x
                version.original.starts_with(&format!("{prefix}.")) || version.original == *prefix
            }
            VersionSpec::NotEqual(v) => version != v,
            VersionSpec::Complex(_) => false, // Can't evaluate complex constraints; don't claim in-range
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

    // `max_major()` was removed because it mixed an exclusive bound for
    // `Range`/`LessThan` with an inclusive one for `Caret`/`Minimum` under a
    // single name and had no caller in or outside this crate. Reintroduce it
    // only with the inclusivity of the bound stated in the name.

    /// Get the version string without operators (for Cargo.toml format)
    ///
    /// Returns just `"1.0.0"` instead of `"==1.0.0"`. Variants that have no
    /// bare-version rendering return `None`: `Any`, and `Complex`, whose raw
    /// string still carries its operators. `Wildcard` is the one
    /// deliberate exception - it renders as `"1.2.*"`, since the wildcard is
    /// part of the version, not an operator prefix.
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
            VersionSpec::Wildcard { prefix, .. } => Some(format!("{prefix}.*")),
            VersionSpec::Complex(_) | VersionSpec::Any => None,
        }
    }

    /// Serialize to Cargo.toml requirement syntax.
    /// Cargo conventions: bare version = caret, `=` for exact pin, `~` for tilde, etc.
    pub fn to_cargo_string(&self) -> Option<String> {
        match self {
            VersionSpec::Caret(v) => Some(v.to_string()), // bare = caret in Cargo
            VersionSpec::Tilde(v) => Some(format!("~{v}")),
            VersionSpec::Pinned(v) => Some(format!("={v}")), // Cargo uses single =
            VersionSpec::Minimum(v) => Some(format!(">={v}")),
            VersionSpec::Maximum(v) => Some(format!("<={v}")),
            VersionSpec::GreaterThan(v) => Some(format!(">{v}")),
            VersionSpec::LessThan(v) => Some(format!("<{v}")),
            VersionSpec::Range { min, max } => Some(format!(">={min}, <{max}")),
            VersionSpec::Wildcard { prefix, .. } => Some(format!("{prefix}.*")),
            VersionSpec::NotEqual(v) => Some(format!("!={v}")),
            VersionSpec::Compatible(v) => Some(v.to_string()), // not a Cargo concept, treat as bare
            VersionSpec::Complex(s) => Some(s.clone()),
            VersionSpec::Any => Some("*".to_string()),
        }
    }

    /// Returns true if this spec can be safely rewritten by an updater
    pub fn is_rewritable(&self) -> bool {
        !matches!(self, VersionSpec::Complex(_) | VersionSpec::Any)
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
            VersionSpec::Wildcard { prefix, pattern } => {
                // Preserve the original wildcard precision:
                // "1.*" (1 segment) → "2.*", "1.2.*" (2 segments) → "1.3.*"
                let segments = prefix.split('.').count();
                let new_prefix = match segments {
                    0 | 1 => format!("{}", new_version.major),
                    _ => format!("{}.{}", new_version.major, new_version.minor),
                };
                VersionSpec::Wildcard {
                    prefix: new_prefix,
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
            VersionSpec::Pinned(v) => write!(f, "=={v}"),
            VersionSpec::Minimum(v) => write!(f, ">={v}"),
            VersionSpec::Maximum(v) => write!(f, "<={v}"),
            VersionSpec::GreaterThan(v) => write!(f, ">{v}"),
            VersionSpec::LessThan(v) => write!(f, "<{v}"),
            VersionSpec::Range { min, max } => write!(f, ">={min},<{max}"),
            VersionSpec::Caret(v) => write!(f, "^{v}"),
            VersionSpec::Tilde(v) => write!(f, "~{v}"),
            VersionSpec::Compatible(v) => write!(f, "~={v}"),
            VersionSpec::Wildcard { prefix, .. } => write!(f, "=={prefix}.*"),
            VersionSpec::NotEqual(v) => write!(f, "!={v}"),
            VersionSpec::Complex(s) => write!(f, "{s}"),
        }
    }
}

impl Serialize for VersionSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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

    // The patch number of a named pre-release must survive parsing: an earlier
    // parser dropped it and read `1.2.3-rc1` as `1.2.0`.
    #[test]
    fn named_prerelease_keeps_its_patch_number() {
        for s in [
            "1.2.3-rc1",
            "1.2.3-beta2",
            "1.2.3-alpha.1",
            "1.2.3-dev",
            "1.2.3rc1",
            "1.2.3-a1",
            "1.2.3-pre",
        ] {
            let v = Version::from_str(s).unwrap();
            assert_eq!((v.major, v.minor, v.patch), (1, 2, 3), "parsing {s}");
            assert!(v.is_prerelease(), "{s} should be flagged pre-release");
        }

        // A PEP 440 post-release keeps its patch number too. Whether it is a
        // *pre*-release is a separate, still-open question - it is not, and
        // sorts above the bare release - so only the numbers are pinned here.
        let v = Version::from_str("1.2.3.post1").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));

        assert_ne!(
            Version::from_str("1.2.3-rc1").unwrap(),
            Version::from_str("1.2.0-rc1").unwrap()
        );
    }

    #[test]
    fn prerelease_separator_is_normalized_away() {
        assert_eq!(
            Version::from_str("1.2.3-rc1").unwrap().pre_release,
            Some("rc1".to_string())
        );
        assert_eq!(
            Version::from_str("1.2.3rc1").unwrap().pre_release,
            Some("rc1".to_string())
        );
        assert_eq!(
            Version::from_str("1.2.3-1").unwrap().pre_release,
            Some("1".to_string())
        );
    }

    // Pre-release ordering is by identifier, not by raw string: `beta.10` must
    // sort above `beta.2`, which lexicographic comparison gets backwards.
    #[test]
    fn prerelease_ordering_is_semantic() {
        let lt = |a: &str, b: &str| {
            let va = Version::from_str(a).unwrap();
            let vb = Version::from_str(b).unwrap();
            assert!(va < vb, "expected {a} < {b}");
        };

        lt("1.0.0-beta.2", "1.0.0-beta.10");
        lt("1.0.0-rc.2", "1.0.0-rc.10");
        lt("1.0.0-alpha9", "1.0.0-alpha10");
        lt("1.0.0-dev", "1.0.0-alpha1");
        lt("1.0.0-alpha1", "1.0.0-beta1");
        lt("1.0.0-beta1", "1.0.0-rc1");
        lt("1.0.0-rc1", "1.0.0");
        lt("1.0.0-rc", "1.0.0-rc.1");
        lt("1.0.0-1", "1.0.0-alpha");
    }

    #[test]
    fn max_picks_the_highest_prerelease() {
        let mut versions = ["1.0.0-rc.2", "1.0.0-rc.10", "1.0.0-beta.3"]
            .iter()
            .map(|s| Version::from_str(s).unwrap())
            .collect::<Vec<_>>();
        versions.sort();
        assert_eq!(versions.last().unwrap().original, "1.0.0-rc.10");
    }

    // A malformed numeric segment is a parse error, never a silently
    // substituted zero - `1.x` must not read as `1.0.0`.
    #[test]
    fn malformed_versions_are_rejected() {
        for s in [
            "1.x",
            "1.2.x",
            "1.2.3 - 2.3.4",
            "x",
            "",
            "latest",
            "1.2.3-",
            "^1.2.3",
        ] {
            assert!(
                Version::from_str(s).is_err(),
                "{s:?} should not parse as a version"
            );
        }
    }

    #[test]
    fn wildcard_spec_does_not_degrade_to_a_pin() {
        assert!(matches!(
            VersionSpec::parse("1.x").unwrap(),
            VersionSpec::Complex(_)
        ));
        assert!(matches!(
            VersionSpec::parse("1.2.3 - 2.3.4").unwrap(),
            VersionSpec::Complex(_)
        ));
    }

    #[test]
    fn missing_components_still_default_to_zero() {
        let v = Version::from_str("2").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (2, 0, 0));
        let v = Version::from_str("2.7").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (2, 7, 0));
    }

    // No byte index taken from a lowercased copy is ever used to slice the
    // original: for non-ASCII input the two differ in length and slicing panics.
    #[test]
    fn non_ascii_version_strings_do_not_panic() {
        for s in ["1.2.3-RC1", "1.2.3-ÄLPHA", "İ1.2.3", "1.2.3İ", "1.2.3-ßeta"] {
            let _ = Version::from_str(s);
        }
        assert_eq!(
            Version::from_str("1.2.3-RC1").unwrap().pre_release,
            Some("RC1".to_string())
        );
        assert_eq!(
            Version::from_str("1.2.3-RC1").unwrap(),
            Version::from_str("1.2.3-rc1").unwrap()
        );
    }

    #[test]
    fn local_segment_is_split_off() {
        let v = Version::from_str("1.2.3+cu118").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        assert_eq!(v.local, Some("cu118".to_string()));
        assert!(!v.is_prerelease());
    }

    // `version_string` renders a bare version and never leaks the operator,
    // since callers write the result straight into a manifest.
    #[test]
    fn version_string_has_no_operators() {
        for s in ["==1.2.3", ">=1.2.3", "^1.2.3", "~1.2.3", "!=1.2.3"] {
            let rendered = VersionSpec::parse(s)
                .unwrap()
                .version_string()
                .expect("bare version");
            assert_eq!(rendered, "1.2.3", "for spec {s}");
        }
        assert_eq!(
            VersionSpec::parse(">1.0,<2.0,!=1.5")
                .unwrap()
                .version_string(),
            None
        );
        assert_eq!(VersionSpec::parse("*").unwrap().version_string(), None);
    }

    #[test]
    fn test_satisfies() {
        let spec = VersionSpec::parse(">=1.0.0,<2.0.0").unwrap();
        assert!(spec.satisfies(&Version::from_str("1.5.0").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("2.0.0").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("0.9.0").unwrap()));
    }
}
