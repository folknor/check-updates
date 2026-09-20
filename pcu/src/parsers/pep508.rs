//! One PEP 508 requirement parser, shared by every Python front end.
//!
//! `requirements.txt` lines, the PEP 621 / PDM / uv / PEP 735 dependency
//! arrays in `pyproject.toml`, and the `pip:` section of a conda
//! `environment.yml` all speak the same grammar. Before this module each of
//! those three had its own hand-rolled splitter, and they disagreed with each
//! other on the same input: two of the three scanned a fixed operator list and
//! broke on the first operator *found in the list* rather than the leftmost one
//! *in the string*, so `pkg<3.0,>=2.0` yielded the package name `pkg<3.0,`;
//! one of them truncated at `[` before looking for an operator at all, so
//! `requests[security]>=2.28.0` lost its constraint entirely.
//!
//! The fix is not a better operator scan. It is to find the name by the
//! grammar - PEP 508 says a name is `letterOrDigit (letterOrDigit | - | _ | .)*
//! letterOrDigit` - and treat whatever follows as opaque. Then no operator,
//! present or future, single- or multi-clause, can be mistaken for a name
//! boundary.

use check_updates_core::VersionSpec;

/// A parsed PEP 508 requirement.
///
/// Every field is kept verbatim as it appeared in the source. Nothing here
/// discards information: a caller that wants only the name can ignore the rest,
/// but a caller that needs to rewrite the source text in place (which pcu's
/// updater does) needs the original bytes to anchor on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// Distribution name exactly as written, before normalization.
    pub name: String,
    /// Extras from `name[a,b]`, in source order, trimmed. Empty when absent.
    pub extras: Vec<String>,
    /// The full version specifier set, verbatim and including every clause:
    /// `">=2.0,<3.0"`, `"==1.2.*"`, `"~=7.0"`. Empty when unconstrained.
    pub specifier: String,
    /// Environment marker text after `;`, without the semicolon. `None` when
    /// absent. Two entries for the same distribution with different markers are
    /// genuinely different requirements and callers must keep both.
    pub marker: Option<String>,
    /// PEP 508 direct reference URL (`name @ https://...`, or a bare URL with
    /// no name at all). When this is set the version does not come from the
    /// index and the requirement must not be resolved against PyPI.
    pub url: Option<String>,
}

impl Requirement {
    /// True when the version is pinned by a URL rather than by the index.
    pub fn is_direct_reference(&self) -> bool {
        self.url.is_some()
    }

    /// The version constraint as a `VersionSpec`.
    ///
    /// A specifier the core version model cannot represent is preserved as
    /// `Complex(raw)`, never downgraded to `Any`. The difference matters:
    /// `Any` claims the file stated no constraint, which is a lie about the
    /// input, and `Complex` is correctly non-rewritable so `-u` leaves the
    /// text alone instead of replacing a constraint it did not understand.
    pub fn version_spec(&self) -> VersionSpec {
        if self.specifier.is_empty() {
            return VersionSpec::Any;
        }
        VersionSpec::parse(&self.specifier)
            .unwrap_or_else(|_| VersionSpec::Complex(self.specifier.clone()))
    }

    /// The name in the form the rest of pcu keys on.
    pub fn normalized_name(&self) -> String {
        normalize_name(&self.name)
    }
}

/// Normalize a distribution name for cross-source matching.
///
/// This is PEP 503 normalization: `re.sub(r"[-_.]+", "-", name).lower()`, so
/// any run of `-`, `_` or `.` folds to a single `-` and the result is
/// lowercased. `zope.interface`, `zope_interface` and `Zope--Interface` all
/// key as `zope-interface`, which is what PyPI and every lock-file format
/// compare by.
///
/// This is the one place the rule lives, and it must stay that way: pcu
/// matches manifest-parser output against lock-file output by this key, and
/// `pcu/src/parsers/lockfiles.rs` normalizes every name it records through
/// this function. A second, weaker copy of the rule anywhere would make dotted
/// or run-separated distributions report as uninstalled.
///
/// Conda names are not PEP 503 names and are deliberately not normalized here.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_separator = false;

    for ch in name.trim().chars() {
        if matches!(ch, '-' | '_' | '.') {
            // A run of separators of any length becomes exactly one `-`,
            // wherever it sits: the regex does not special-case position.
            pending_separator = true;
            continue;
        }
        if pending_separator {
            out.push('-');
            pending_separator = false;
        }
        out.extend(ch.to_lowercase());
    }
    if pending_separator {
        out.push('-');
    }

    out
}

/// Parse one PEP 508 requirement.
///
/// Returns `None` when the text is not a requirement at all (empty, a comment,
/// or something with no name where a name must be). Callers decide what to do
/// with a direct reference; this function reports it rather than inventing a
/// package literally named `"pkg @ https://..."`.
pub fn parse(input: &str) -> Option<Requirement> {
    let input = input.trim();
    if input.is_empty() || input.starts_with('#') {
        return None;
    }

    let (requirement, marker) = split_marker(input);
    let requirement = requirement.trim();
    if requirement.is_empty() {
        return None;
    }

    // A name has to be followed by something that can legally follow a name.
    // `https://host/x.whl` does start with the valid name `https`, so the
    // remainder is what distinguishes a requirement from a bare URL.
    let named = take_name(requirement)
        .and_then(|(name, rest)| take_extras(rest).map(|(extras, rest)| (name, extras, rest)))
        .filter(|(_, _, rest)| {
            let rest = rest.trim();
            rest.is_empty()
                || rest.starts_with('@')
                || rest.starts_with('(')
                || starts_with_operator(rest)
        });

    let Some((name, extras, rest)) = named else {
        // No usable name: either a bare URL requirement, where the whole string
        // is the reference, or something that is not a requirement at all.
        if is_bare_url(requirement) {
            return Some(Requirement {
                name: String::new(),
                extras: Vec::new(),
                specifier: String::new(),
                marker,
                url: Some(requirement.to_string()),
            });
        }
        return None;
    };

    let rest = rest.trim();

    // Direct reference: `name [extras] @ url`.
    if let Some(url) = rest.strip_prefix('@') {
        let url = url.trim();
        if url.is_empty() {
            return None;
        }
        return Some(Requirement {
            name: name.to_string(),
            extras,
            specifier: String::new(),
            marker,
            url: Some(url.to_string()),
        });
    }

    // PEP 508 allows the specifier set to be parenthesised: `name (>=1.0)`.
    let rest = match rest.strip_prefix('(') {
        Some(inner) => inner.strip_suffix(')')?.trim(),
        None => rest,
    };

    if !rest.is_empty() && !starts_with_operator(rest) {
        // Trailing text that is neither a specifier nor a marker: this is not a
        // requirement we understand, and guessing a name out of it is how
        // garbage reaches the index.
        return None;
    }

    Some(Requirement {
        name: name.to_string(),
        extras,
        specifier: rest.to_string(),
        marker,
        url: None,
    })
}

/// Split off an environment marker at the first `;` that is at bracket depth
/// zero and outside quotes. Markers themselves contain quoted strings which
/// may contain anything, so the scan has to be state-aware rather than a
/// `find(';')`.
fn split_marker(input: &str) -> (&str, Option<String>) {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;

    for (idx, ch) in input.char_indices() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '[' | '(' => depth += 1,
                ']' | ')' => depth = depth.saturating_sub(1),
                ';' if depth == 0 => {
                    let marker = input[idx + 1..].trim();
                    let marker = if marker.is_empty() {
                        None
                    } else {
                        Some(marker.to_string())
                    };
                    return (&input[..idx], marker);
                }
                _ => {}
            },
        }
    }

    (input, None)
}

/// A requirement that is nothing but a URL: `https://...`, `git+ssh://...`,
/// `file:///...`, or a local `./dist/foo-1.0.whl`.
fn is_bare_url(s: &str) -> bool {
    s.contains("://")
        || s.starts_with("file:")
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('/')
}

/// Take the leading distribution name per the PEP 508 grammar.
///
/// The name starts with an alphanumeric, may contain `-`, `_` and `.`, and must
/// end on an alphanumeric. Everything after is the caller's problem - which is
/// exactly the property that makes operator lists irrelevant here.
fn take_name(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    if bytes.first().is_none_or(|b| !b.is_ascii_alphanumeric()) {
        return None;
    }

    let mut end = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            end = i + 1;
        } else {
            break;
        }
    }

    // The grammar forbids a trailing separator; hand those back to the rest.
    while end > 0 && !bytes[end - 1].is_ascii_alphanumeric() {
        end -= 1;
    }

    if end == 0 {
        return None;
    }
    Some((&s[..end], &s[end..]))
}

/// Take an optional `[extra1,extra2]` group. Absent brackets are not an error;
/// an unclosed bracket is.
fn take_extras(s: &str) -> Option<(Vec<String>, &str)> {
    let trimmed = s.trim_start();
    let Some(inner) = trimmed.strip_prefix('[') else {
        return Some((Vec::new(), s));
    };

    let close = inner.find(']')?;
    let extras = inner[..close]
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .collect();

    Some((extras, &inner[close + 1..]))
}

/// Whether the remaining text opens a version specifier. `^` and `~` are not
/// PEP 508, but Poetry and PDM write them in the same position and the core
/// version model understands them, so accepting them here keeps one parser
/// rather than two.
fn starts_with_operator(s: &str) -> bool {
    s.starts_with("==")
        || s.starts_with(">=")
        || s.starts_with("<=")
        || s.starts_with("!=")
        || s.starts_with("~=")
        || s.starts_with('>')
        || s.starts_with('<')
        || s.starts_with('=')
        || s.starts_with('^')
        || s.starts_with('~')
        || s.starts_with('*')
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn leftmost_operator_wins_not_first_in_a_list() {
        // The whole point of a real grammar walk: an operator list scan returns the name
        // `pkg<3.0,` for both of these.
        let r = parse("pkg<3.0,>=2.0").unwrap();
        assert_eq!(r.name, "pkg");
        assert_eq!(r.specifier, "<3.0,>=2.0");

        let r = parse("pkg>1.0,<=2.0").unwrap();
        assert_eq!(r.name, "pkg");
        assert_eq!(r.specifier, ">1.0,<=2.0");

        let r = parse("django<3.0,>=2.0").unwrap();
        assert_eq!(r.name, "django");
        assert_eq!(r.specifier, "<3.0,>=2.0");
    }

    #[test]
    fn extras_do_not_swallow_the_specifier() {
        let r = parse("requests[security]>=2.28.0").unwrap();
        assert_eq!(r.name, "requests");
        assert_eq!(r.extras, vec!["security".to_string()]);
        assert_eq!(r.specifier, ">=2.28.0");
        assert!(matches!(r.version_spec(), VersionSpec::Minimum(_)));

        let r = parse("celery[redis, msgpack] == 5.2.0").unwrap();
        assert_eq!(r.name, "celery");
        assert_eq!(r.extras, vec!["redis".to_string(), "msgpack".to_string()]);
        assert!(matches!(r.version_spec(), VersionSpec::Pinned(_)));
    }

    #[test]
    fn markers_are_kept_not_discarded() {
        let r = parse("pkg==1.0; python_version < '3.8'").unwrap();
        assert_eq!(r.name, "pkg");
        assert_eq!(r.specifier, "==1.0");
        assert_eq!(r.marker.as_deref(), Some("python_version < '3.8'"));
    }

    #[test]
    fn a_semicolon_inside_a_quoted_marker_does_not_split_twice() {
        let r = parse("pkg==1.0; sys_platform == 'a;b'").unwrap();
        assert_eq!(r.marker.as_deref(), Some("sys_platform == 'a;b'"));
    }

    #[test]
    fn direct_references_are_reported_not_renamed() {
        let r = parse("mypkg @ https://example.com/mypkg-1.0.whl").unwrap();
        assert_eq!(r.name, "mypkg");
        assert!(r.is_direct_reference());

        let r = parse("https://example.com/mypkg-1.0.whl").unwrap();
        assert!(r.name.is_empty());
        assert!(r.is_direct_reference());

        let r = parse("git+https://github.com/o/r.git#egg=r").unwrap();
        assert!(r.is_direct_reference());
    }

    #[test]
    fn parenthesised_specifiers() {
        let r = parse("pkg (>=1.0)").unwrap();
        assert_eq!(r.name, "pkg");
        assert_eq!(r.specifier, ">=1.0");
    }

    #[test]
    fn unmodelled_specifiers_survive_as_complex() {
        let r = parse("pkg===1.0+local").unwrap();
        assert_eq!(r.name, "pkg");
        assert!(matches!(r.version_spec(), VersionSpec::Complex(_)));
        assert!(!r.version_spec().is_rewritable());

        let r = parse("pkg==1!2.0").unwrap();
        assert_eq!(r.name, "pkg");
        // Whatever the core model makes of an epoch, it must not claim `Any`.
        assert!(!matches!(r.version_spec(), VersionSpec::Any));
    }

    #[test]
    fn bare_name_is_unconstrained() {
        let r = parse("flask").unwrap();
        assert_eq!(r.name, "flask");
        assert!(r.specifier.is_empty());
        assert!(matches!(r.version_spec(), VersionSpec::Any));
    }

    #[test]
    fn dotted_and_underscored_names_are_kept_whole() {
        let r = parse("zope.interface>=5.0").unwrap();
        assert_eq!(r.name, "zope.interface");
        let r = parse("typing_extensions>=4.0").unwrap();
        assert_eq!(r.normalized_name(), "typing-extensions");
    }

    #[test]
    fn normalization_is_pep_503() {
        // re.sub(r"[-_.]+", "-", name).lower()
        assert_eq!(normalize_name("zope.interface"), "zope-interface");
        assert_eq!(normalize_name("Zope.Interface"), "zope-interface");
        assert_eq!(normalize_name("zope_interface"), "zope-interface");
        assert_eq!(normalize_name("foo--bar"), "foo-bar");
        assert_eq!(normalize_name("foo._-.bar"), "foo-bar");
        assert_eq!(normalize_name("ruamel.yaml.clib"), "ruamel-yaml-clib");
        assert_eq!(normalize_name("  Requests  "), "requests");
        // Names that need no folding are returned unchanged but lowercased.
        assert_eq!(normalize_name("Flask"), "flask");
        // Idempotent: normalizing a normalized key is a no-op.
        let once = normalize_name("Zope..Interface");
        assert_eq!(normalize_name(&once), once);
    }

    #[test]
    fn a_dotted_requirement_keys_the_same_as_its_lock_entry() {
        // The cross-file invariant: whatever a manifest writes and whatever a
        // lock file writes must land on one key.
        let from_manifest = parse("zope.interface>=5.0").unwrap().normalized_name();
        assert_eq!(from_manifest, normalize_name("zope-interface"));
        assert_eq!(from_manifest, normalize_name("zope_interface"));
    }

    #[test]
    fn garbage_is_rejected_rather_than_named() {
        assert!(parse("").is_none());
        assert!(parse("# comment").is_none());
        assert!(parse("numpy 1.24.0 py39_0").is_none());
        assert!(parse("-e .").is_none());
    }
}
