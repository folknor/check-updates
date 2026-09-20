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

## VER-019 - two PEP 440 residuals from the version-model restructure

Both verified during review, both judged not worth an ecosystem discriminant
today, both recorded so they are not rediscovered as fresh defects.

- `1.0-1` is an implicit post-release under PEP 440 (`1.0.post1`) but parses
  through the semver branch as the pre-release `"1"`. PyPI normalises versions
  in its JSON responses, so this only reaches us from a hand-written pin.
- `==1.0.0` no longer satisfies `1.0.0+cu118`. PEP 440 says a specifier with no
  local segment matches any local version. Worth revisiting if `in_range` for a
  pinned torch-style dependency looks wrong.

Context for whoever picks these up: `local` now participates in ordering for all
three ecosystems, which is a deliberate deviation from semver precedence. It is
unobservable on crates.io and npm - both registries reject a publish that
differs from an existing release only in build metadata - and it is required for
PyPI, where `2.1.0+cu118` and `2.1.0+cpu` are distinct releases. Ordering a
local segment can only make a version visible that equality would have
collapsed, never hide one, so principle 1 settles it without needing to know
which tool is asking.

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

## VER-008 - the blocked-row signal exists but the update tables filter it out

Reported by core, ccu, pcu-parsers (and as the mechanism behind VER-007 and
DSC-007). The hunters' split is now settled by evidence rather than argument:
`Complex` stays the safe landing place (a rewrite would have to discard the part
that was not understood), and the defect was only ever the silence.

The signal landed. `core::UpdateBlocker` distinguishes `UnmodellableSpec`,
`UnconstrainedSpec` and `NoWritableTarget`; `DependencyCheck::update_blocker()`
is defined as "has an update but is not writable under the most permissive
flags", so a major withheld by plain `-u` is *not* flagged and stays in the
existing "run `-uf`" count. The table carries a yellow marker inside the
severity column and prints one legend line per distinct reason with a row count.
`--json` gains `updatable` and `blocked_reason`.

Residue, and it defeats the whole thing until fixed - in all three `main.rs`:

- The `--update` display list filters on `!will_update(args.minor, args.force)`,
  which drops blocked rows from the table **entirely**. That is the rule-3
  violation the marker was built to fix, one layer above where the marker can
  reach. The filter needs `|| c.update_blocker().is_some()`.
- The `skipped` count conflates "excluded by your filter" with "cannot be
  written at all" and tells the user to `Run -uf to force upgrade all`, which is
  false for a blocked row.
- `UpdateBlocker` is only reachable as `check_updates_core::types::UpdateBlocker`
  and should join the `pub use types::{...}` line in `core/src/lib.rs`.

## VER-010 - the independent-field severity comparison survives in four more places

Reported by core, ccu, ncu. Fixed in `core/src/resolver.rs`: ordering now decides
whether there is an update at all and the fields only choose the name, so `None`
means "not an update" and nothing else.

The same hand-rolled comparison was copied into global mode in all three tools
and into pcu's Python reporting, none of which call the fixed function:

- `ccu/src/global.rs::update_severity`
- `pcu/src/global.rs::update_severity`
- `ncu/src/global.rs::update_severity`
- `pcu/src/uv_python.rs::is_patch_update`

All four still return `None` for a move that does not change the triple - a
prerelease-to-release, a gained post-release, a fourth release segment, a local
segment - and all four still misclassify a lower-minor/higher-patch target. They
should call `DependencyResolver::calculate_severity`, which now holds the one
correct definition.

This is where the pcu global-mode *downgrade* symptom actually lived. The
`1.4.0.post1 -> 1.4.0` command is already gone, because `has_update` there is
computed from the corrected `Ord`, but the severity half is untouched.

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
