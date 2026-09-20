# Version model defects (VER)

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.

Findings about `core/src/version.rs`, `core/src/resolver.rs` and the shared
`Version` / `VersionSpec` model. Every CLI inherits these.

## VER-001 - Named prereleases silently lose their patch number

Reported independently by all five hunters (core, ccu, ncu, pcu-parsers,
pcu-runtime), each calling it the highest-impact defect in its scope.

`parse_prerelease` scans the pattern list `["dev","post","alpha","beta","rc","a","b","c","-"]`
and splits at the first pattern found anywhere with `idx > 0`. `"-"` is checked
**last**, so for `1.2.3-rc1` the match is `rc` at index 6 and the base string is
`"1.2.3-"`. `from_str` then does `parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0)`
- `"3-".parse::<u64>()` fails and the patch silently becomes **0**. Same for
`-beta`, `-alpha`, `-dev`, `-post`, `-a/-b/-c`. Only a dash followed by an
unrecognized word (`1.2.3-pre`) parses correctly, so behavior is inconsistent
from one prerelease convention to the next.

Consequences: `1.2.3-rc1 == 1.2.0-rc1` under `PartialEq`; ordering, `max()`,
`in_range`, `target` and `severity` are all computed against a fabricated
version. `Display` echoes `original`, so the number the user sees and the number
the tool compared are different - which is why this is invisible in output.

Downstream (ccu): under `--pre-release`, an update from `1.2.2` to `1.2.3-rc1`
computes `severity = None` while `target` is `Some`, so the row prints,
`will_update()` is false, `-u` refuses to write it, and it lands in the "skipped
outside the selected severity" tally with no explanation. Also hits `ccu -g`
when an installed binary's version in `.crates.toml` is a prerelease.

Fix direction given by the hunters: parse strictly (numeric core, then the first
`-`, then `+`), and stop using `unwrap_or(0)` for a segment that failed to parse
- see VER-005.

## VER-002 - Prerelease ordering is a plain string comparison

Reported by core, ccu, ncu, pcu-parsers.

`impl Ord for Version` does `(Some(a), Some(b)) => a.cmp(b)`, so
`1.0.0-beta.10 < 1.0.0-beta.2`, `rc.2 < rc.10`, `alpha10 < alpha9`. `beta < rc`
works only by accident of the alphabet, and `dev` sorts above `beta`/`alpha`
instead of below. Semver requires dot-separated identifier comparison with
numeric identifiers compared numerically; PEP 440 has its own ordering.

The retained string is also inconsistent about whether it carries the leading
`-` (`1.2.3-rc1` yields `"rc1"` today because of VER-001; `1.2.3rc1` also yields
`"rc1"`; `1.2.3-1` yields `"-1"`), so mixed forms in one version list sort
arbitrarily. Affects "latest" selection under `-p` and the
`matching.sort(); matching.last()` lockfile pick in `ccu/src/main.rs`.

## VER-003 - PEP 440 post-releases are classified as pre-releases

Reported by core, pcu-parsers, pcu-runtime.

`"1.0.0.post1"` matches the `post` pattern, so `pre_release = Some(".post1")`
and `is_prerelease()` is true. `Ord` then ranks `1.0.0.post1 < 1.0.0`. Post
releases are strictly *newer* than the base release under PEP 440.

- `pcu/src/pypi.rs` filters every `.postN` release out of `filtered_versions`
  and out of `latest_stable`, so a package whose newest stable release is a post
  release is reported one release behind, and pcu can never recommend one.
- In pcu global mode an installed `1.4.0.post1` compares `<` `1.4.0`, so
  `has_update` is true: pcu prints `1.4.0.post1 -> 1.4.0` and generates an
  upgrade command for a **downgrade**.

## VER-004 - PEP 440 epoch versions fail to parse and are dropped silently

Reported by pcu-runtime, pcu-parsers.

`Version::from_str("1!2.0.0")` splits on `.` and tries to parse `"1!2"` as the
major - it fails, so the whole parse errors. In `pypi.rs::get_package` that
release is skipped by `if let Ok(version)` with no diagnostic. A package that
has performed an epoch reset has its newest releases invisible, and pcu
confidently reports an older version as latest. `Ord` also has no epoch field,
so even once parsed they would order wrong.

## VER-005 - `unwrap_or(0)` turns a failed parse into valid-looking data

Reported by core, ncu, pcu-runtime (as the root smell behind VER-001).

A numeric segment that fails to parse becomes `0` rather than an error, so junk
parses as a valid pin instead of being rejected:

- `"1.x"` (common npm spec) - minor fails to parse - `Version 1.0.0` -
  `VersionSpec::Pinned(1.0.0)`. A wildcard spec becomes an exact pin, and a
  rewrite then emits `"==1.4.2"` (see UPD-002).
- `"1.2.3 - 2.3.4"` (npm hyphen range) - splits at `-`, base `"1.2.3 "` -
  `Pinned(1.2.0)`.

`from_str` should reject a numeric segment it cannot parse. A missing component
defaulting to 0 is fine; a malformed one is not.

## VER-006 - Byte index from a lowercased copy is used to slice the original

Reported by core, pcu-parsers, pcu-runtime.

`s.to_lowercase().find(pattern)` produces a byte index into the *lowercased*
string; `&s[..idx]` / `&s[idx..]` then slice the **original**. Lowercasing is not
length-preserving in Unicode, so the index can be wrong or land off a char
boundary - a panic path in a library that parses registry-supplied strings.
Also allocates a fresh lowercased string once per pattern, nine times per
version parsed.

## VER-007 - Cargo's single-`=` exact pin degrades to an unrewritable `Complex`

Reported by core and ccu.

`VersionSpec::parse` handles `==`, `>=`, `<=`, `!=`, `>`, `<` (Python style) but
never bare `=`. `ccu/src/parsers/cargo_toml.rs::parse_cargo_version` routes
anything starting with `=` into it; no operator branch matches,
`Version::from_str("=1.2.3")` fails, and it falls through to
`Complex("=1.2.3")`. `=x.y.z` is standard Cargo syntax. Downstream:

- `satisfies()` returns `false` for `Complex`, so `in_range` is always `None`;
- `is_rewritable()` is false, so `target_spec` and `force_spec` are `None` and
  `will_update` is false even under `-uf` - the dep is silently never updated;
- the installed-version picker in `ccu/src/main.rs` filters `Cargo.lock` entries
  by `satisfies`, gets an empty set, and falls back to "highest overall version
  in the lock file", which for a crate present at two majors (e.g. `syn` 1.x and
  2.x) picks the *transitive* version, not the pinned one, and produces a wrong
  severity.

No diagnostic at any point.

## VER-008 - `Complex` is a silent dead end for every spec the parser cannot model

Reported by core, ccu, pcu-parsers (and as the mechanism behind VER-007 and
DSC-007).

Anything unparseable becomes `Complex`, which claims "out of range" from
`satisfies()` and "not rewritable" from `is_rewritable()`, quietly disabling
half the pipeline with no user-visible signal. The hunters split on the remedy:
core and ccu argue an unparseable spec should be a hard error surfaced to the
user; ncu argues the opposite direction for its ecosystem - that `Complex`
(non-rewritable) is the *safe* landing place and the bug is that npm specs get
lossily approximated into rewritable variants instead (DSC-007). Both agree the
current silence is the defect.

## VER-009 - Two contradictory definitions of "in range"

Reported by core.

`VersionSpec::satisfies` says `3.1.0` satisfies `>=2.28.0`.
`DependencyResolver::calculate_in_range` then adds a special case restricting
`Minimum`/`GreaterThan` to a single major. So `DependencyCheck.in_range` is not
"the latest version satisfying the constraint" as `types.rs` documents it; it is
that plus an undocumented semver heuristic applied to exactly two of the
thirteen variants. The JSON field and the predicate the same crate exposes
disagree. Either `satisfies` encodes the major-pinning or the resolver does not.

## VER-010 - `calculate_severity` compares fields independently

Reported by core, ccu (as the downstream of VER-001), ncu.

```rust
if target.major > current.major { Major }
else if target.minor > current.minor { Minor }
else if target.patch > current.patch { Patch }
else { None }
```

- Prerelease to release (`1.2.0-rc1` -> `1.2.0`) has all three fields equal, so
  `target` is `Some` but `severity` is `None`. `will_update` then returns false
  for every non-force mode, `format_severity` prints an empty column, and a
  dependency with a real available update is displayed blank and never written
  by `-u`.
- A target whose minor is lower but patch higher (`1.2.3` -> `1.1.5`) classifies
  as `Patch`. Unreachable today because `calculate_target` only returns targets
  greater than current, but the function is `pub` and named as a general
  classifier.

Should be one lexicographic comparison of the triple plus a prerelease rule.

## VER-011 - `with_version` changes the meaning of `Compatible` / `Tilde` constraints

Reported by core.

`with_version` preserves the variant but not the precision. `~=1.4` (PEP 440:
lock major only) rewritten to `2.0.0` becomes `~=2.0.0`, which locks major
*and* minor - because `satisfies` for `Compatible` decides which rule applies by
counting dots in `v.original`. An update silently narrows the user's declared
range. Same class of issue for `Tilde`. The `Wildcard` arm got this right by
counting prefix segments; the others did not.

## VER-012 - Wildcard check runs before the range check, and matches on raw strings

Reported by core.

`VersionSpec::parse` tests `s.contains('*')` *before* the comma/range check, so
a compound spec containing a wildcard (`>=1.0,<2.*`) parses as a single
`Wildcard` with prefix `">=1.0,<2"`. `satisfies` then does
`version.original.starts_with(">=1.0,<2.")` - always false.

Separately, `Wildcard::satisfies` matches on `version.original` (a raw string)
rather than the parsed numeric fields, so `1.2.*` is sensitive to the exact
textual form the registry returned (`1.2` vs `1.02`).

## VER-013 - `VersionSpec::parse` only models a two-clause range

Reported by pcu-parsers.

Only `>=X,<Y` is recognised. `>1.0,<2.0`, three-clause specs, and PEP 440
multi-clause specifier sets all become `Complex`, whose `satisfies` returns
`false` - i.e. the tool claims the installed version is out of range. See
VER-008.

## VER-014 - `version_string()` violates its own doc comment

Reported by core.

Documented as "Returns just `1.0.0` instead of `==1.0.0`", but the `Complex(s)`
arm returns the raw string verbatim, operators included, and `Wildcard` returns
`"1.2.*"`. Callers in the `ccu` and `ncu` parsers assert on it as if it were a
bare version.

## VER-015 - `PartialEq` / `Ord` ignore the local version segment

Reported by pcu-parsers.

`1.0.0+cu118` and `1.0.0+cpu` compare equal. (Semver excludes build metadata
from precedence by design, so this is correct for ccu/ncu and wrong for PEP 440
local versions, which are ordered - another instance of one struct serving three
incompatible orderings.)

## VER-016 - `max_major()` has mutually incompatible semantics per variant and no callers

Reported by core.

`Range { max }` returns `max.major` (an *exclusive* bound) while `Caret(v)`
returns `v.major` (inclusive) and `LessThan(v)` returns `v.major` (exclusive).
No caller outside `core`. Delete it or define the bound.

## Structural recommendation, as filed by the hunters

Four of the five hunters independently reached the same conclusion, differing
only in which replacement they would reach for. The shared diagnosis: `Version`
hand-rolls a parser that is neither semver nor PEP 440 and gets both wrong, then
papers over failures with `unwrap_or(0)`; and `pre_release: Option<String>`
cannot support correct ordering no matter how `Ord` is written. VER-001 through
VER-006, VER-010 and VER-015 all trace to this.

- core and pcu argue for one `Version` type carrying an explicit `Ecosystem`
  discriminant, with a real semver path (structured prerelease identifiers,
  build metadata excluded from comparison) and a real PEP 440 path (epoch,
  release tuple of arbitrary length, `{a,b,rc}` pre, `.postN`, `.devN`, local),
  dispatching `Ord`, `is_prerelease` and severity accordingly.
- ccu argues for deleting the shared model in this direction instead: give `ccu`
  the `semver` crate's `Version`/`VersionReq` behind a small trait the resolver
  consumes - which handles `=`, multi-requirement lists, prerelease ordering and
  build metadata correctly and for free - and leave the hand-rolled enum to
  `pcu`/`ncu` until they get the same treatment.
- ncu argues that `VersionSpec` cannot represent npm's grammar at all and that
  ncu needs its own range parser regardless of what happens to `Version` (see
  DSC-007, UPD-002).

All four agree `VersionSpec` should become per-ecosystem comparator lists rather
than thirteen ad-hoc variants plus a `Complex` escape hatch, and that
`satisfies` and `calculate_in_range` should share one definition (VER-009).
