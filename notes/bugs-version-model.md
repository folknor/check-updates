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

## VER-013 - `VersionSpec::parse` only models a two-clause range

Reported by pcu-parsers.

Only `>=X,<Y` is recognised. `>1.0,<2.0`, three-clause specs, and PEP 440
multi-clause specifier sets all become `Complex`, whose `satisfies` returns
`false` - i.e. the tool claims the installed version is out of range. See
VER-008.

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
