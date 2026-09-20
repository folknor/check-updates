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

Still true, by a different mechanism than filed: `split_release` now stops at
`1` and the suffix `"!2.0.0"` is rejected as not a recognised pre-release, so
the parse still errors and the release is still dropped silently.

## VER-017 - conda wildcards now resolve correctly and still cannot be written

Narrowed to its last part, and that part is now the only thing standing between
a conda prefix pin and a working update.

Fixed in `core`: `VersionSpec::Wildcard` carries a `base: Version` (the prefix
parsed and zero-filled), so `base_version()` returns `Some` and a wildcard
dependency in a lock-less project gets a `current`, a severity and a rewritable
spec. `with_version` preserves all three precisions, so `1.24.0.*` bumps to
`1.26.3.*` instead of widening to `1.26.*`. `Wildcard::satisfies` compares
parsed numeric fields by declared precision rather than string prefixes.

Residue: `pcu/src/updater.rs::replace_in_conda` has no `==X.*` <-> `=X`
mapping. It tries the literal spec, then a blanket `==` -> `=` substitution. For
a conda line `numpy=1.24` the parser produces a `Wildcard` rendering as
`==1.24.*`, which becomes `=1.24.*` and does not match the text `=1.24` in the
file. So the dependency now computes a correct target, severity and spec - and
is still silently not written. The fix is to strip the `.*` when the source line
used conda's bare `=` prefix form.

One form already works: a line spelled `python=3.9.*` matches after the
`==`->`=` substitution.

## VER-018 - `VersionSpec::parse` cannot fail, which makes `if let Ok` a no-op

Found while fixing ncu's alias handling, and it changes how several other
entries should be read.

`VersionSpec::parse` ends with `Ok(VersionSpec::Complex(s.to_string()))`, so it
never returns `Err` for unrecognised text. Every `if let Ok(spec) = ...` guard
over it is therefore not a filter at all - it admits everything. In ncu this is
why an open-ended set of npm protocol specifiers (`catalog:`, `patch:`,
`portal:`, `exec:`, `user/repo`, dist-tags, tarball paths) all became `Complex`
specs and were sent to the registry as package names.

Two consequences worth recording:

- A deny-list of "things that are not registry specs" is a losing race against
  that fallback. ncu now uses an allow-list instead, and the same argument
  applies anywhere else a parser guards on this function.
- One narrow case *did* return `Err` until recently - a two-clause range whose
  bound failed to parse, e.g. `>=1.0,<2.x`. That now degrades to `Complex` like
  everything else, which is consistent but means the `Err` arm is dead.

This intersects VER-008 and VER-013: the question those entries raise - whether
an unmodellable spec should be a hard error or a silent `Complex` - is currently
answered "always `Complex`, everywhere, with no way for a caller to tell".

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

## VER-013 - `VersionSpec::parse` only models a two-clause range

Reported by pcu-parsers.

Only `>=X,<Y` is recognised. `>1.0,<2.0`, three-clause specs, and PEP 440
multi-clause specifier sets all become `Complex`, whose `satisfies` returns
`false` - i.e. the tool claims the installed version is out of range. See
VER-008.

## VER-015 - `PartialEq` / `Ord` ignore the local version segment

Reported by pcu-parsers.

`1.0.0+cu118` and `1.0.0+cpu` compare equal. (Semver excludes build metadata
from precedence by design, so this is correct for ccu/ncu and wrong for PEP 440
local versions, which are ordered - another instance of one struct serving three
incompatible orderings.)

## Structural recommendation, as filed by the hunters

Four of the five hunters independently reached the same conclusion, differing
only in which replacement they would reach for. The shared diagnosis: `Version`
hand-rolls a parser that is neither semver nor PEP 440 and gets both wrong, then
papers over failures with `unwrap_or(0)`; and `pre_release: Option<String>`
cannot support correct ordering no matter how `Ord` is written.

Partly overtaken by events: the strict release/suffix split, structured
prerelease ordering and the removal of `unwrap_or(0)` have landed in
`core/src/version.rs`, so the parser is no longer the weak point. What remains
is the type: `pre_release: Option<String>` still serves three incompatible
orderings (VER-003, VER-015), there is still no epoch field (VER-004), and
`is_prerelease` still cannot distinguish a pre-release from a post-release.

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
