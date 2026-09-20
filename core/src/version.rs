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

/// A parsed version.
///
/// The model is PEP 440's, because PEP 440 is the superset: an epoch, a release
/// tuple of arbitrary length, a pre-release, a post-release, a dev-release and a
/// local segment. Every semver version is expressible in it - a semver
/// pre-release is a PEP 440 pre-release, and semver build metadata lands in
/// `local` - so one type still serves all three ecosystems, and the fields that
/// only Python uses simply stay `None` for Cargo and npm input.
///
/// One deliberate deviation remains, recorded here because it is the last place
/// the three orderings genuinely disagree: `local` participates in comparison.
/// PEP 440 orders local versions (`1.0+cu118` and `1.0+cpu` are different
/// releases and must not compare equal), while semver excludes build metadata
/// from precedence. Ordering it is the informing choice under
/// `reference/resolution-principles.md` rule 1 - it can only ever make a
/// version *visible* that equality would have collapsed, never hide one - so it
/// is applied uniformly rather than gated on an ecosystem discriminant that
/// `core` has no way to obtain today.
#[derive(Debug, Clone)]
pub struct Version {
    /// PEP 440 epoch (`1!2.0.0`). Zero for everything that does not spell one.
    pub epoch: u64,
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The full release tuple as written, which may be shorter or longer than
    /// three segments. `major`/`minor`/`patch` are the first three, zero-filled;
    /// comparison uses this, so `1.2.3.4` and `1.2.3.5` are no longer equal.
    pub release: Vec<u64>,
    /// Pre-release identifiers (`rc1`, `alpha.1`, semver's post-`-` body).
    pub pre_release: Option<String>,
    /// PEP 440 post-release number (`1.0.post1`). A post-release is strictly
    /// *newer* than its base release, which is why it cannot live in
    /// `pre_release`.
    pub post: Option<u64>,
    /// PEP 440 dev-release number (`1.0.dev1`), which sorts below everything
    /// else at the same release/pre/post position.
    pub dev: Option<u64>,
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
            epoch: 0,
            major,
            minor,
            patch,
            release: vec![major, minor, patch],
            pre_release: None,
            post: None,
            dev: None,
            local: None,
            original: format!("{major}.{minor}.{patch}"),
        }
    }

    /// Check if this is a pre-release version.
    ///
    /// A dev-release counts; a *post*-release does not. `1.0.0.post1` is a
    /// released version that happens to sort above `1.0.0`, and every caller of
    /// this function uses it to decide whether a release is fit to recommend -
    /// `ccu/src/cratesio.rs`, `pcu/src/pypi.rs` and `ncu/src/npm.rs` all filter
    /// their version lists and their `latest_stable` on it. Answering "yes" for
    /// a post-release is what made pcu report a package one release behind, and
    /// made its global mode emit an upgrade command that was a downgrade.
    pub fn is_prerelease(&self) -> bool {
        self.pre_release.is_some() || self.dev.is_some()
    }

    /// Check if this is a PEP 440 post-release.
    pub fn is_postrelease(&self) -> bool {
        self.post.is_some()
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

        // Strip the PEP 440 epoch (`1!2.0.0`). Before this existed the `!`
        // landed in the release core, the parse failed, and every release
        // published after an epoch reset was dropped - so the tool named an
        // older version as latest with total confidence.
        let (epoch, version_part) =
            split_epoch(version_part).ok_or_else(|| VersionError::InvalidVersion(s.to_string()))?;

        // Split the numeric release core from whatever follows it. The release
        // core is the longest leading `\d+(\.\d+)*`; the remainder must be a
        // recognizable pre/post/dev suffix or the whole string is rejected.
        let (base_part, suffix) = split_release(version_part);
        if base_part.is_empty() {
            return Err(VersionError::InvalidVersion(s.to_string()));
        }
        let parsed_suffix =
            parse_suffix(suffix).ok_or_else(|| VersionError::InvalidVersion(s.to_string()))?;

        // Parse the release tuple. Every segment here is a digit run by
        // construction, so a parse failure means numeric overflow and is an
        // error - never a silent `0`. The tuple is kept whole: truncating at
        // three made `1.2.3.4` compare equal to `1.2.3.5`.
        let mut release = Vec::new();
        for seg in base_part.split('.') {
            release.push(
                seg.parse::<u64>()
                    .map_err(|_| VersionError::InvalidVersion(s.to_string()))?,
            );
        }
        if release.is_empty() {
            return Err(VersionError::InvalidVersion(s.to_string()));
        }

        let at = |i: usize| release.get(i).copied().unwrap_or(0);

        Ok(Version {
            epoch,
            major: at(0),
            minor: at(1),
            patch: at(2),
            release,
            pre_release: parsed_suffix.pre,
            post: parsed_suffix.post,
            dev: parsed_suffix.dev,
            local,
            original: s.to_string(),
        })
    }
}

/// Split a leading `N!` epoch off a version string.
///
/// Returns `None` when a `!` is present but is not preceded by a pure digit run
/// - that is malformed input, not an epoch, and must not be silently absorbed.
fn split_epoch(s: &str) -> Option<(u64, &str)> {
    match s.find('!') {
        None => Some((0, s)),
        Some(idx) => {
            let head = &s[..idx];
            if head.is_empty() || !head.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((head.parse().ok()?, &s[idx + 1..]))
        }
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

/// Spellings PEP 440 normalizes to `.postN`.
const POST_MARKERS: [&str; 3] = ["post", "rev", "r"];

fn is_identifier_body(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// The pre / post / dev parts carried by a version's suffix.
#[derive(Default)]
struct Suffix {
    pre: Option<String>,
    post: Option<u64>,
    dev: Option<u64>,
}

/// One `[separator][letters][digits]` component of a suffix, with the byte
/// offset it starts at so the caller can slice the original text back out.
struct Component<'a> {
    start: usize,
    word: &'a str,
    num: Option<u64>,
}

/// Tokenize a suffix into components. `None` for anything that is not made of
/// separators, ASCII letters and ASCII digits.
fn components(s: &str) -> Option<Vec<Component<'_>>> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let start = i;
        if matches!(bytes[i], b'.' | b'-' | b'_') {
            i += 1;
        }
        let word_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let word = &s[word_start..i];
        let num_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let num = if num_start == i {
            None
        } else {
            // An overflowing numeric identifier is malformed input, not a zero.
            Some(s[num_start..i].parse::<u64>().ok()?)
        };
        if word.is_empty() && num.is_none() {
            return None;
        }
        out.push(Component { start, word, num });
    }

    Some(out)
}

/// Index of the first component whose word is one of `markers`.
fn find_marker(comps: &[Component<'_>], markers: &[&str]) -> Option<usize> {
    comps
        .iter()
        .position(|c| markers.iter().any(|m| m.eq_ignore_ascii_case(c.word)))
}

/// Read the number attached to the marker at `idx`, tolerating the separated
/// spelling (`.post.1`) as well as the joined one (`.post1`). A bare marker
/// means zero, as PEP 440 says.
fn marker_number(comps: &[Component<'_>], idx: usize) -> u64 {
    if let Some(n) = comps[idx].num {
        return n;
    }
    match comps.get(idx + 1) {
        Some(next) if next.word.is_empty() => next.num.unwrap_or(0),
        _ => 0,
    }
}

/// Classify the remainder left by [`split_release`].
///
/// `None` means "this is not a version at all" - the caller turns it into a
/// parse error.
///
/// The pre-release string is normalized: the introducing separator (`-`, `.` or
/// `_`) is stripped, so `1.2.3-rc1` and `1.2.3rc1` both yield `"rc1"` and the
/// comparison in [`compare_prerelease`] sees one consistent form. Post and dev
/// releases are lifted out into their own numeric fields, because they are not
/// pre-releases and cannot be ordered as if they were: `.postN` sorts *above*
/// the base release and `.devN` below it, and a single `Option<String>` ranked
/// by identifier can only express one of those two directions.
fn parse_suffix(suffix: &str) -> Option<Suffix> {
    if suffix.is_empty() {
        return Some(Suffix::default());
    }

    // Semver: everything after the first `-` is the pre-release, whatever it
    // spells (`1.2.3-1`, `1.2.3-pre`, `1.2.3-alpha.1`). The one exception is a
    // body that is exactly a post marker plus digits (`1.0.0-post1`), which no
    // semver project uses as a pre-release tag and which PEP 440 reads as a
    // post-release.
    if let Some(rest) = suffix.strip_prefix('-') {
        if !is_identifier_body(rest) {
            return None;
        }
        if let Some(comps) = components(rest)
            && comps.len() == 1
            && find_marker(&comps, &POST_MARKERS) == Some(0)
        {
            return Some(Suffix {
                post: Some(marker_number(&comps, 0)),
                ..Suffix::default()
            });
        }
        return Some(Suffix {
            pre: Some(rest.to_string()),
            ..Suffix::default()
        });
    }

    // PEP 440 and the compact semver-adjacent forms: an optional `.`/`_`
    // separator followed by known marker words.
    let rest = suffix
        .strip_prefix('.')
        .or_else(|| suffix.strip_prefix('_'))
        .unwrap_or(suffix);

    if !is_identifier_body(rest) {
        return None;
    }

    let comps = components(rest)?;
    let post_idx = find_marker(&comps, &POST_MARKERS);
    let dev_idx = find_marker(&comps, &["dev"]);

    let post = post_idx.map(|i| marker_number(&comps, i));
    let dev = dev_idx.map(|i| marker_number(&comps, i));

    // Everything before the first post/dev marker is the pre-release, taken as
    // the original substring so the field keeps the spelling the registry used.
    let cut = match (post_idx, dev_idx) {
        (Some(p), Some(d)) => p.min(d),
        (Some(p), None) => p,
        (None, Some(d)) => d,
        (None, None) => comps.len(),
    };
    let pre_text = match comps.get(cut) {
        Some(c) => &rest[..c.start],
        None => rest,
    };

    let pre = if pre_text.is_empty() {
        None
    } else {
        // The leading word still has to be a marker we know; an unrecognized
        // word means this is not a version and is rejected rather than absorbed.
        let word_len = pre_text.bytes().take_while(u8::is_ascii_alphabetic).count();
        if word_len == 0 {
            return None;
        }
        let word = &pre_text[..word_len];
        if !PRE_RELEASE_MARKERS
            .iter()
            .any(|m| m.eq_ignore_ascii_case(word))
        {
            return None;
        }
        Some(pre_text.to_string())
    };

    Some(Suffix { pre, post, dev })
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

/// Compare two release tuples of possibly different length, zero-padding the
/// shorter one so `1.2` and `1.2.0` stay equal while `1.2.3.4` and `1.2.3` do
/// not.
fn compare_release(a: &[u64], b: &[u64]) -> Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let ord = a
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&b.get(i).copied().unwrap_or(0));
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Which side of the base release a version sits on.
///
/// The phases are ordered exactly as PEP 440 orders them:
/// `1.0.dev1 < 1.0a1.dev1 < 1.0a1 < 1.0 < 1.0.post1`. A bare dev-release is its
/// own phase because it sorts below *every* pre-release, while a dev-release
/// attached to a pre-release sorts within that pre-release.
fn phase(v: &Version) -> u8 {
    if v.pre_release.is_some() {
        1
    } else if v.post.is_some() {
        3
    } else if v.dev.is_some() {
        0
    } else {
        2
    }
}

/// Compare PEP 440 local version segments (`1.0+cu118` vs `1.0+cpu`).
///
/// Absent sorts below present, numeric segments sort above alphabetic ones and
/// compare numerically. Semver would call these equal; see the type-level note
/// on [`Version`] for why ordering them is the informing choice.
fn compare_local(a: Option<&String>, b: Option<&String>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => {
            let seg = |s: &str| -> Vec<Result<u64, String>> {
                s.split(['.', '-', '_'])
                    .map(|p| match p.parse::<u64>() {
                        Ok(n) => Ok(n),
                        Err(_) => Err(p.to_ascii_lowercase()),
                    })
                    .collect()
            };
            let (lx, ly) = (seg(x), seg(y));
            for (l, r) in lx.iter().zip(ly.iter()) {
                let ord = match (l, r) {
                    (Ok(p), Ok(q)) => p.cmp(q),
                    (Ok(_), Err(_)) => Ordering::Greater,
                    (Err(_), Ok(_)) => Ordering::Less,
                    (Err(p), Err(q)) => p.cmp(q),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            lx.len().cmp(&ly.len())
        }
    }
}

/// Compare two versions on everything *except* the local segment.
///
/// This is the comparison PEP 440 uses for version matching: a specifier that
/// carries no local segment matches a candidate regardless of the candidate's
/// local segment, so `==1.0.0` matches `1.0.0+cu118`. Ordering still takes the
/// local segment into account - see the note on [`Version`] - and that is a
/// different question from whether a specifier matches.
fn cmp_ignoring_local(a: &Version, b: &Version) -> Ordering {
    match a.epoch.cmp(&b.epoch) {
        Ordering::Equal => {}
        ord => return ord,
    }
    match compare_release(&a.release, &b.release) {
        Ordering::Equal => {}
        ord => return ord,
    }

    match phase(a).cmp(&phase(b)) {
        Ordering::Equal => {}
        ord => return ord,
    }

    // Within a phase both sides carry the same kind of marker, so each part
    // is compared in PEP 440's order: pre, then post, then dev.
    let pre = match (&a.pre_release, &b.pre_release) {
        (Some(x), Some(y)) => compare_prerelease(x, y),
        _ => Ordering::Equal,
    };
    if pre != Ordering::Equal {
        return pre;
    }

    // A post-release is newer than the release it patches, so absent sorts
    // below present here.
    match a.post.cmp(&b.post) {
        Ordering::Equal => {}
        ord => return ord,
    }

    // A dev-release is older than the thing it leads up to, so present
    // sorts below absent - the reverse of `post`.
    match (a.dev, b.dev) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Whether `version` matches the version named by a specifier, under PEP 440's
/// local-segment rule: a specifier with no local segment ignores the
/// candidate's, while one that names a local segment requires it exactly.
fn matches_specifier_version(version: &Version, spec: &Version) -> bool {
    if spec.local.is_none() {
        cmp_ignoring_local(version, spec) == Ordering::Equal
    } else {
        version == spec
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match cmp_ignoring_local(self, other) {
            Ordering::Equal => {}
            ord => return ord,
        }
        compare_local(self.local.as_ref(), other.local.as_ref())
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
    /// `==1.2.*` - a prefix match.
    ///
    /// `prefix` is the numeric prefix with no operator and no trailing `.*`
    /// (`"1.2"`); its segment count is the declared precision and is what
    /// decides which fields [`VersionSpec::satisfies`] compares. `pattern` is
    /// the raw text the spec was parsed from, kept for diagnostics only - no
    /// behaviour reads it. `base` is `prefix` parsed as a version, zero-filled
    /// (`1.2` -> `1.2.0`), so a wildcard has a base version like every other
    /// bounded variant; without it a wildcard dependency in a lock-less project
    /// had no `current` at all and could never be reported as updatable.
    Wildcard {
        prefix: String,
        pattern: String,
        base: Version,
    },
    /// !=1.2.3
    NotEqual(Version),
    /// Complex constraint we store as raw string
    Complex(String),
    /// Any version (no constraint or *)
    Any,
}

/// Parse a `*`-bearing specifier into a [`VersionSpec::Wildcard`].
///
/// Returns `None` when the text left of the `*` is not a numeric prefix - a
/// `~=1.2.*` or a `>=1.2.*` is a constraint this type does not model, and the
/// caller keeps it as `Complex` rather than inventing a prefix out of the
/// operator characters.
fn parse_wildcard(s: &str) -> Option<VersionSpec> {
    let body = s
        .strip_prefix("==")
        .or_else(|| s.strip_prefix('='))
        .unwrap_or(s)
        .trim();
    let prefix = body.trim_end_matches('*').trim_end_matches('.');
    if prefix.is_empty() {
        return None;
    }
    let base = Version::from_str(prefix).ok()?;
    Some(VersionSpec::Wildcard {
        prefix: prefix.to_string(),
        pattern: s.to_string(),
        base,
    })
}

/// How many release segments the user actually wrote, 1 to 3.
///
/// `~=1.4` and `~=1.4.0` are different constraints in PEP 440, and `~1.2` and
/// `~1.2.3` are different constraints in Cargo, so the declared precision is
/// part of the spec's meaning and has to survive a rewrite. It is read from the
/// release core of `original` rather than by counting dots in the whole string,
/// which miscounts `1.2.post1` and `1.2+local`.
fn declared_precision(v: &Version) -> usize {
    let trimmed = v.original.trim();
    let body = split_epoch(trimmed).map_or(trimmed, |(_, rest)| rest);
    let (core, _) = split_release(body);
    if core.is_empty() {
        3
    } else {
        core.split('.').count().clamp(1, 3)
    }
}

/// Re-render `v` at `precision` release segments.
///
/// Truncating drops any pre-release and local segment, since `1.2.3-rc1`
/// truncated to two segments is `1.2` and nothing else is meaningful. A
/// precision of 3 or more pads to a full triple - `1.26` rendered at three
/// segments is `1.26.0` - unless the version carries a pre-release or local
/// segment, in which case its own text is kept verbatim.
fn with_precision(v: &Version, precision: usize) -> Version {
    // An epoch is part of the version's identity, so a re-rendered version keeps
    // it: dropping the `1!` would write a spec naming a completely different
    // release line.
    let ep = if v.epoch == 0 {
        String::new()
    } else {
        format!("{}!", v.epoch)
    };
    match precision {
        1 => Version {
            epoch: v.epoch,
            major: v.major,
            minor: 0,
            patch: 0,
            release: vec![v.major],
            pre_release: None,
            post: None,
            dev: None,
            local: None,
            original: format!("{ep}{}", v.major),
        },
        2 => Version {
            epoch: v.epoch,
            major: v.major,
            minor: v.minor,
            patch: 0,
            release: vec![v.major, v.minor],
            pre_release: None,
            post: None,
            dev: None,
            local: None,
            original: format!("{ep}{}.{}", v.major, v.minor),
        },
        _ if v.pre_release.is_some()
            || v.local.is_some()
            || v.post.is_some()
            || v.dev.is_some()
            || v.release.len() > 3 =>
        {
            v.clone()
        }
        _ => Version {
            epoch: v.epoch,
            original: format!("{ep}{}.{}.{}", v.major, v.minor, v.patch),
            ..Version::new(v.major, v.minor, v.patch)
        },
    }
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

        // Handle range (>=X,<Y). This runs *before* the wildcard check: a
        // compound spec that happens to contain a `*` (`>=1.0,<2.*`) is a range
        // first and must not be swallowed whole as a single wildcard with the
        // nonsense prefix `">=1.0,<2"`. A clause that will not parse leaves the
        // whole thing `Complex` rather than raising - a multi-clause specifier
        // set is a constraint we cannot model, not malformed input.
        if s.contains(',') {
            let parts: Vec<&str> = s.split(',').collect();
            if parts.len() == 2 {
                let min_part = parts[0].trim();
                let max_part = parts[1].trim();

                if let (Some(min_str), Some(max_str)) =
                    (min_part.strip_prefix(">="), max_part.strip_prefix('<'))
                    && let (Ok(min), Ok(max)) =
                        (Version::from_str(min_str), Version::from_str(max_str))
                {
                    return Ok(VersionSpec::Range { min, max });
                }
            }
            // Complex constraint
            return Ok(VersionSpec::Complex(s.to_string()));
        }

        // Handle wildcard
        if s.contains('*') {
            return Ok(parse_wildcard(s).unwrap_or_else(|| VersionSpec::Complex(s.to_string())));
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
        // Cargo and npm both spell an exact pin with a single `=` (`=1.2.3`).
        // This branch has to come last among the operators so that `==`, `>=`,
        // `<=`, `!=` and `~=` are all claimed by their own arms first. Without
        // it the spec fell through to `Complex`, which is not rewritable, so an
        // exactly pinned Cargo dependency was silently never updated - not even
        // under `-uf` - and never matched a lock entry either.
        if let Some(version_str) = s.strip_prefix('=') {
            let version = Version::from_str(version_str)?;
            return Ok(VersionSpec::Pinned(version));
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
            // PEP 440: a specifier that names no local segment matches any
            // local version, so `==1.0.0` matches `1.0.0+cu118`. Semver says
            // the same about build metadata, so this holds for all three
            // ecosystems. `!=` is defined as the inverse of `==` and follows.
            VersionSpec::Pinned(v) => matches_specifier_version(version, v),
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
                // Cargo: ~1 means >=1.0.0, <2.0.0 (lock major only)
                //        ~1.2 and ~1.2.3 mean <1.3.0 (lock major+minor)
                if version < v {
                    return false;
                }
                match declared_precision(v) {
                    1 => version.major == v.major,
                    _ => version.major == v.major && version.minor == v.minor,
                }
            }
            VersionSpec::Compatible(v) => {
                // PEP 440: ~=X.Y means >=X.Y, <(X+1).0.0 (lock major only)
                //          ~=X.Y.Z means >=X.Y.Z, <X.(Y+1).0 (lock major+minor)
                if version < v {
                    return false;
                }
                match declared_precision(v) {
                    1 | 2 => version.major == v.major,
                    _ => version.major == v.major && version.minor == v.minor,
                }
            }
            VersionSpec::Wildcard { prefix, base, .. } => {
                // Compare parsed numeric fields, never the raw `original` text:
                // a prefix match on strings makes `1.2.*` depend on the exact
                // spelling the registry returned (`1.2` vs `1.02` vs `1.2.0`).
                // The declared precision decides how many fields are compared,
                // so `1.2.*` matches 1.2.x and never 1.20.x.
                match prefix.split('.').count() {
                    0 | 1 => version.major == base.major,
                    2 => version.major == base.major && version.minor == base.minor,
                    _ => {
                        version.major == base.major
                            && version.minor == base.minor
                            && version.patch == base.patch
                    }
                }
            }
            VersionSpec::NotEqual(v) => !matches_specifier_version(version, v),
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
            // A wildcard's base is the zero-filled prefix. It used to return
            // `None`, which left `DependencyResolver::resolve` with no `current`
            // for any wildcard dependency lacking a lock entry: target became
            // latest with no spec and no severity, and `will_update` was false
            // in every mode, silently. Conda felt it hardest (pcu has no source
            // of installed conda versions at all) but a lock-less `==1.24.*`
            // had the same fate.
            VersionSpec::Wildcard { base, .. } => Some(base),
            VersionSpec::Complex(_) | VersionSpec::Any => None,
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
            // `Tilde` and `Compatible` mean different things at different
            // precisions, so the rewrite has to be rendered at the precision the
            // user declared. Writing the full triple turned `~=1.4` (lock major)
            // into `~=2.0.0` (lock major and minor), silently narrowing the
            // declared range.
            VersionSpec::Tilde(old) => {
                VersionSpec::Tilde(with_precision(new_version, declared_precision(old)))
            }
            VersionSpec::Compatible(old) => {
                VersionSpec::Compatible(with_precision(new_version, declared_precision(old)))
            }
            VersionSpec::Wildcard {
                prefix, pattern, ..
            } => {
                // Preserve the original wildcard precision exactly:
                // "1.*" -> "2.*", "1.2.*" -> "1.3.*", "1.24.0.*" -> "1.26.0.*".
                // Capping at two segments widened the user's declared precision.
                let segments = prefix.split('.').count().clamp(1, 3);
                let base = with_precision(new_version, segments);
                VersionSpec::Wildcard {
                    prefix: base.original.clone(),
                    pattern: pattern.clone(),
                    base,
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

    // Cargo's exact pin is a single `=`. It used to fall through to `Complex`,
    // which is not rewritable, so the dependency was never updated.
    #[test]
    fn single_equals_is_an_exact_pin() {
        let spec = VersionSpec::parse("=1.2.3").unwrap();
        assert_eq!(
            spec,
            VersionSpec::Pinned(Version::from_str("1.2.3").unwrap())
        );
        assert!(spec.satisfies(&Version::from_str("1.2.3").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("1.2.4").unwrap()));
        assert!(spec.is_rewritable());
        // The two-character operators keep their own meaning.
        assert!(matches!(
            VersionSpec::parse(">=1.2.3").unwrap(),
            VersionSpec::Minimum(_)
        ));
        assert!(matches!(
            VersionSpec::parse("<=1.2.3").unwrap(),
            VersionSpec::Maximum(_)
        ));
        assert!(matches!(
            VersionSpec::parse("!=1.2.3").unwrap(),
            VersionSpec::NotEqual(_)
        ));
        assert!(matches!(
            VersionSpec::parse("~=1.2.3").unwrap(),
            VersionSpec::Compatible(_)
        ));
        assert!(matches!(
            VersionSpec::parse("=1.2.*").unwrap(),
            VersionSpec::Wildcard { .. }
        ));
    }

    // PEP 440: a specifier with no local segment matches any local version.
    // Ordering still separates them - only matching ignores the segment.
    #[test]
    fn pin_without_a_local_segment_matches_a_local_version() {
        let spec = VersionSpec::parse("==1.0.0").unwrap();
        assert!(spec.satisfies(&Version::from_str("1.0.0+cu118").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("1.0.1+cu118").unwrap()));

        // A specifier that names a local segment requires that exact one.
        let exact = VersionSpec::parse("==1.0.0+cu118").unwrap();
        assert!(exact.satisfies(&Version::from_str("1.0.0+cu118").unwrap()));
        assert!(!exact.satisfies(&Version::from_str("1.0.0+cpu").unwrap()));
        assert!(!exact.satisfies(&Version::from_str("1.0.0").unwrap()));

        // `!=` is the inverse of `==` and follows the same rule.
        let excluded = VersionSpec::parse("!=1.0.0").unwrap();
        assert!(!excluded.satisfies(&Version::from_str("1.0.0+cu118").unwrap()));

        // Ordering is unchanged: the local segment still participates.
        assert!(Version::from_str("1.0.0").unwrap() < Version::from_str("1.0.0+cu118").unwrap());
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

        // A PEP 440 post-release keeps its patch number too, and is not a
        // pre-release: see `post_releases_sort_above_their_base`.
        let v = Version::from_str("1.2.3.post1").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        assert!(!v.is_prerelease());

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

    // A wildcard has a base version, so a wildcard dependency with no lock
    // entry still has a `current` to compare against.
    #[test]
    fn wildcard_has_a_base_version() {
        let spec = VersionSpec::parse("==1.24.*").unwrap();
        let base = spec.base_version().expect("wildcard base");
        assert_eq!((base.major, base.minor, base.patch), (1, 24, 0));

        let spec = VersionSpec::parse("3.9.*").unwrap();
        let base = spec.base_version().expect("wildcard base");
        assert_eq!((base.major, base.minor, base.patch), (3, 9, 0));
    }

    // Prefix matching is on parsed fields, not on registry text.
    #[test]
    fn wildcard_matches_numeric_fields() {
        let spec = VersionSpec::parse("==1.2.*").unwrap();
        assert!(spec.satisfies(&Version::from_str("1.2").unwrap()));
        assert!(spec.satisfies(&Version::from_str("1.2.9").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("1.20.0").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("1.3.0").unwrap()));

        let spec = VersionSpec::parse("==1.*").unwrap();
        assert!(spec.satisfies(&Version::from_str("1.9.9").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("2.0.0").unwrap()));
    }

    // A rewrite must not widen or narrow the precision the user declared.
    #[test]
    fn rewrites_preserve_declared_precision() {
        let bump = |spec: &str, to: &str| {
            VersionSpec::parse(spec)
                .unwrap()
                .with_version(&Version::from_str(to).unwrap())
                .to_string()
        };

        assert_eq!(bump("==1.24.0.*", "1.26.3"), "==1.26.3.*");
        assert_eq!(bump("==1.24.*", "1.26.3"), "==1.26.*");
        assert_eq!(bump("==1.*", "2.6.3"), "==2.*");
        assert_eq!(bump("~=1.4", "2.0.0"), "~=2.0");
        assert_eq!(bump("~=1.4.2", "2.0.0"), "~=2.0.0");
        assert_eq!(bump("~1.2", "2.3.4"), "~2.3");
        assert_eq!(bump("~1.2.3", "2.3.4"), "~2.3.4");
    }

    // `~=X.Y` locks the major only; `~=X.Y.Z` locks major and minor. The rule
    // is chosen by declared precision, and must survive a rewrite.
    #[test]
    fn compatible_precision_survives_a_rewrite() {
        let spec = VersionSpec::parse("~=1.4").unwrap();
        assert!(spec.satisfies(&Version::from_str("1.9.0").unwrap()));

        let rewritten = spec.with_version(&Version::from_str("2.0.0").unwrap());
        assert!(
            rewritten.satisfies(&Version::from_str("2.9.0").unwrap()),
            "rewriting ~=1.4 must not start locking the minor"
        );

        let narrow = VersionSpec::parse("~=1.4.2").unwrap();
        assert!(!narrow.satisfies(&Version::from_str("1.9.0").unwrap()));
    }

    // A compound spec that happens to contain a `*` is a range first.
    #[test]
    fn a_wildcard_inside_a_compound_spec_is_not_one_wildcard() {
        let spec = VersionSpec::parse(">=1.0,<2.*").unwrap();
        assert!(
            matches!(spec, VersionSpec::Complex(_)),
            "got {spec:?}, expected Complex"
        );
        assert!(matches!(
            VersionSpec::parse(">=1.0,<2.0").unwrap(),
            VersionSpec::Range { .. }
        ));
    }

    // A post-release is strictly newer than the release it patches. Ranking it
    // as a pre-release made pcu report a package one release behind, and made
    // its global mode print `1.4.0.post1 -> 1.4.0` with an upgrade command that
    // was a downgrade.
    #[test]
    fn post_releases_sort_above_their_base() {
        let lt = |a: &str, b: &str| {
            let (va, vb) = (Version::from_str(a).unwrap(), Version::from_str(b).unwrap());
            assert!(va < vb, "expected {a} < {b}");
        };

        lt("1.4.0", "1.4.0.post1");
        lt("1.4.0.post1", "1.4.0.post2");
        lt("1.4.0.post2", "1.4.1");
        lt("1.4.0rc1", "1.4.0.post1");
        lt("1.0.0-post1", "1.0.0.post2");

        for s in ["1.4.0.post1", "1.4.0post1", "1.4.0.post", "1.4.0-post1"] {
            let v = Version::from_str(s).unwrap();
            assert!(!v.is_prerelease(), "{s} is not a pre-release");
            assert!(v.is_postrelease(), "{s} is a post-release");
        }

        assert_eq!(Version::from_str("1.4.0.post1").unwrap().post, Some(1));
        assert_eq!(Version::from_str("1.4.0.post").unwrap().post, Some(0));
        assert_eq!(Version::from_str("1.4.0.post.3").unwrap().post, Some(3));
    }

    // Dev releases sit below everything at the same release, including
    // pre-releases, and a dev release attached to a pre or post sorts within it.
    #[test]
    fn dev_releases_sort_below_everything_at_the_same_release() {
        let lt = |a: &str, b: &str| {
            let (va, vb) = (Version::from_str(a).unwrap(), Version::from_str(b).unwrap());
            assert!(va < vb, "expected {a} < {b}");
        };

        lt("1.0.dev1", "1.0a1");
        lt("1.0a1.dev1", "1.0a1");
        lt("1.0.dev1", "1.0.dev2");
        lt("1.0a1", "1.0");
        lt("1.0", "1.0.post1.dev1");
        lt("1.0.post1.dev1", "1.0.post1");

        assert!(Version::from_str("1.0.dev1").unwrap().is_prerelease());
    }

    // An epoch reset is how a project starts its version numbering over. Before
    // it parsed, every release published after the reset was dropped and the
    // tool named an older version as latest.
    #[test]
    fn epoch_parses_and_dominates_the_release() {
        let v = Version::from_str("1!2.0.0").unwrap();
        assert_eq!(v.epoch, 1);
        assert_eq!((v.major, v.minor, v.patch), (2, 0, 0));
        assert_eq!(v.to_string(), "1!2.0.0");

        let low = Version::from_str("99.0.0").unwrap();
        assert!(low < v, "an epoch outranks any release tuple");
        assert!(Version::from_str("1!0.1").unwrap() > low);

        let mut versions = ["2023.1", "1!1.0", "2024.2"]
            .iter()
            .map(|s| Version::from_str(s).unwrap())
            .collect::<Vec<_>>();
        versions.sort();
        assert_eq!(versions.last().unwrap().original, "1!1.0");

        // A `!` that is not an epoch is malformed, not something to absorb.
        assert!(Version::from_str("a!1.0").is_err());
        assert!(Version::from_str("!1.0").is_err());
    }

    // The release tuple is not three fields. Truncating at the patch made
    // `1.2.3.4` and `1.2.3.5` compare equal, so a four-segment release could
    // never be reported as an update over its predecessor.
    #[test]
    fn release_tuple_keeps_every_segment() {
        let v = Version::from_str("1.2.3.4").unwrap();
        assert_eq!(v.release, vec![1, 2, 3, 4]);
        assert!(Version::from_str("1.2.3.4").unwrap() < Version::from_str("1.2.3.5").unwrap());
        assert!(Version::from_str("1.2.3").unwrap() < Version::from_str("1.2.3.1").unwrap());
        assert_eq!(
            Version::from_str("1.2").unwrap(),
            Version::from_str("1.2.0").unwrap()
        );
    }

    // PEP 440 orders local versions, so two builds of the same release are two
    // different releases. Semver excludes build metadata from precedence; the
    // deviation is deliberate and documented on `Version`.
    #[test]
    fn local_segments_are_ordered_not_ignored() {
        let cpu = Version::from_str("1.0.0+cpu").unwrap();
        let cu = Version::from_str("1.0.0+cu118").unwrap();
        assert_ne!(cpu, cu);
        assert!(cpu < cu);
        assert!(Version::from_str("1.0.0").unwrap() < cpu);
        assert!(Version::from_str("1.0.0+1").unwrap() > Version::from_str("1.0.0+abc").unwrap());
        assert!(
            Version::from_str("1.0.0+build.2").unwrap()
                > Version::from_str("1.0.0+build.1").unwrap()
        );
    }

    #[test]
    fn test_satisfies() {
        let spec = VersionSpec::parse(">=1.0.0,<2.0.0").unwrap();
        assert!(spec.satisfies(&Version::from_str("1.5.0").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("2.0.0").unwrap()));
        assert!(!spec.satisfies(&Version::from_str("0.9.0").unwrap()));
    }
}
