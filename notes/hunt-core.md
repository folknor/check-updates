Read-only review of the `core` crate (`/home/folk/Programs/check-updates/core/src/{lib,types,version,resolver,output}.rs`) plus its callers in `ccu/`, `pcu/`, `ncu/`. No files edited, no build/test run.

## Findings, worst first

### 1. Every pre-release version silently loses its patch number (`core/src/version.rs`, `parse_prerelease` + `Version::from_str`)
`parse_prerelease` scans for the patterns `["dev","post","alpha","beta","rc","a","b","c","-"]` in that order and splits at the first hit with `idx > 0`. `"-"` is *last*, so for `1.2.3-rc1` the match is `rc` at index 6, leaving the base string `"1.2.3-"`. `from_str` then does `parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0)` — `"3-".parse::<u64>()` fails and the patch silently becomes **0**. Same for `-beta`, `-alpha`, `-dev`, `-post`, `-rc`, `-a/-b/-c`.

Consequences: `1.2.3-rc1 == 1.2.0-rc1` under `PartialEq`, ordering is wrong, `in_range`/`target`/`severity` are computed against a fabricated version, and the displayed "from" column still prints the correct `original` string, so the number the user sees and the number the tool compared are different. This is the single highest-impact defect in the crate; it is invisible precisely because `Display` echoes `original`.

The same `unwrap_or(0)` swallow makes junk parse as a valid pin rather than erroring:
- `"1.x"` (common npm spec) → minor fails to parse → `Version 1.0.0` → `VersionSpec::Pinned(1.0.0)`. A wildcard spec becomes an exact pin.
- `"1.2.3 - 2.3.4"` (npm hyphen range) → splits at `-`, base `"1.2.3 "` → `Pinned(1.2.0)`.
Neither is reported; `from_str` should reject a numeric segment it cannot parse instead of defaulting to 0.

### 2. `parse_prerelease` indexes the original string with offsets from a lowercased copy
`s.to_lowercase().find(pattern)` produces a byte index into the lowercased string, then `&s[..idx]` / `&s[idx..]` slice the *original*. Lowercasing is not length-preserving in Unicode (e.g. `İ` is 2 bytes upper, 3 bytes lower in UTF-8 forms), so the index can be wrong or land off a char boundary — a panic path in a library that parses registry-supplied strings. Also `.to_lowercase()` allocates once per pattern, nine times per version parsed.

### 3. PEP 440 post-releases are classified as pre-releases and sorted below the release
`"1.0.0.post1"` matches pattern `post` → `pre_release = Some(".post1")`, so `is_prerelease()` is true. `pcu/src/pypi.rs` filters those versions out entirely unless prereleases are requested, and `Ord` ranks `1.0.0.post1 < 1.0.0`. Post-releases are strictly *newer* than the release under PEP 440. pcu therefore cannot ever recommend a post-release, which is a real publishing pattern on PyPI.

### 4. Cargo exact pins (`=1.2.3`) degrade to `Complex` and become permanently un-updatable
`VersionSpec::parse` handles `==`, `>=`, `<=`, `!=`, `>`, `<` but never bare `=`. `ccu/src/parsers/cargo_toml.rs::parse_cargo_version` routes anything starting with `=` into `VersionSpec::parse`, which falls through to `Version::from_str("=1.2.3")` (major parse fails) and returns `Complex("=1.2.3")`. Downstream:
- `satisfies()` returns `false` for `Complex`, so `in_range` is always `None`;
- `is_rewritable()` is false, so `target_spec` and `force_spec` are `None` and `will_update` is false even with `--force`;
- in `ccu/src/main.rs` the installed-version picker filters `Cargo.lock` entries by `satisfies`, gets an empty set, and falls back to "highest overall version in the lock file" — which for a crate present at multiple majors picks the *transitive* version, not the pinned one. The resulting row compares the wrong installed version against latest.

Net effect: `=x.y.z` pins in a Cargo.toml are reported wrongly and never rewritten, with no diagnostic. `Complex` is a silent dead end generally — worth making unparseable specs a hard error surfaced to the user rather than a value that quietly disables half the pipeline.

### 5. Two contradictory definitions of "in range"
`VersionSpec::satisfies` says `3.1.0` satisfies `>=2.28.0`. `DependencyResolver::calculate_in_range` then adds a special case that restricts `Minimum`/`GreaterThan` to a single major. So `DependencyCheck.in_range` is not "the latest version satisfying `dependency.spec`" as `types.rs` documents it ("Latest version within the constraint"); it is that, plus an undocumented semver heuristic applied to exactly two of the thirteen spec variants. Either `satisfies` should encode the major-pinning or the resolver should not — the current split means the JSON field and the predicate the same crate exposes disagree.

### 6. `calculate_severity` compares fields independently, so it can report the wrong severity or none at all
```rust
if target.major > current.major { Major }
else if target.minor > current.minor { Minor }
else if target.patch > current.patch { Patch }
else { None }
```
- Pre-release → release (`1.2.0-rc1` → `1.2.0`) has all three fields equal, so `target` is `Some` but `severity` is `None`. `will_update` then returns false for every non-force mode, and `TableRenderer::format_severity` prints an empty column. A dependency with a real available update is displayed with a blank severity and is never written by `-u`.
- Any target whose minor is lower but patch higher (`1.2.3` → `1.1.5`) classifies as `Patch`. The resolver only emits increasing targets today, so this is latent rather than live, but the function is `pub` and named as a general classifier.
Severity should be derived from a single lexicographic comparison of the triple plus a pre-release rule, not three independent `>` tests.

### 7. `Ord` on pre-release identifiers is a plain string compare
`(Some(a), Some(b)) => a.cmp(b)` — so `rc.10 < rc.9`, `beta < rc` accidentally works but `alpha10 < alpha9`, and the retained string sometimes carries the leading `-` and sometimes not (`1.2.3-rc1` yields `"rc1"` today because of bug 1; `1.2.3rc1` also yields `"rc1"`; `1.2.3-1` yields `"-1"`). Mixed forms in one version list sort arbitrarily. Semver requires dot-separated identifier comparison with numeric identifiers compared numerically.

### 8. `Compatible`/`Tilde` rewrites change the constraint's meaning
`with_version` preserves the variant but not the precision. `~=1.4` (PEP 440: lock major only) rewritten to `2.0.0` becomes `~=2.0.0`, which locks major *and* minor — because `satisfies` for `Compatible` decides which rule applies by counting dots in `v.original`. So an update silently narrows the user's declared range. Same class of issue for `Tilde`. The `Wildcard` arm got this right by counting prefix segments; the others did not.

### 9. `Version::from_str` ordering: `+` is stripped before pre-release, but `Complex`/wildcard ordering in `VersionSpec::parse` is wrong
The `s.contains('*')` check runs *before* the comma/range check, so any compound spec containing a wildcard (`>=1.0,<2.*`) is parsed as a single `Wildcard` with prefix `">=1.0,<2"`. `satisfies` then does `version.original.starts_with(">=1.0,<2.")` — always false.

Also `Wildcard::satisfies` matches on `version.original` (a raw string) rather than on the parsed numeric fields, so `1.2.*` does not match a registry version published as `1.2` vs `1.02`, and is sensitive to the exact textual form the registry returned.

### 10. `version_string()` violates its own doc comment
Documented as "Returns just `1.0.0` instead of `==1.0.0`", but the `Complex(s)` arm returns the raw string verbatim, operators included, and `Wildcard` returns `"1.2.*"`. Callers in `ccu`/`ncu` parsers assert on it as if it were a bare version.

### 11. `max_major()` is inconsistent and unused
`Range { max }` returns `max.major` (an *exclusive* bound) while `Caret(v)` returns `v.major` (inclusive) and `LessThan(v)` returns `v.major` (exclusive). No caller outside `core` uses it — grep found none in ccu/pcu/ncu. Dead API with mutually incompatible semantics per variant; delete it or define the bound.

### 12. npm `latest` can be a pre-release even when pre-releases are excluded, and a total fetch failure reads as "up to date"
`ncu/src/npm.rs`: `versions` is filtered by `include_prerelease`, but `latest` is taken from the `dist-tags.latest` field with no such filter, so a package whose `latest` tag points at a pre-release yields a `PackageInfo` whose `latest` is not in `versions`. The resolver uses `package_info.latest` (never `latest_stable` — that field is populated by all three registry clients and read by nobody outside tests) as the force/fallback target, so `ncu` will recommend and, under `--force`, write a pre-release the user asked to exclude.
The fallback `.unwrap_or_else(|| Version::new(0, 0, 0))` turns "no parseable versions at all" into `latest = 0.0.0`, which compares below everything and renders as "All dependencies are up to date!" — a partial failure presented as a clean result. `ccu`'s equivalent path correctly errors out.

### 13. `TableRenderer` column widths use `str::len()` (bytes)
`output.rs` computes `max_name`/`max_from`/`max_to` from `.len()` and pads with `{:<name_w$}`, which also counts bytes. Any non-ASCII package name or version string misaligns the table. `chars().count()` (or a width crate) is the fix. Minor, but it is a correctness claim the renderer makes.

## Structural recommendation
Items 1, 2, 3, 7 and much of 6 are all one root cause: `Version` hand-rolls a parser that is neither semver nor PEP 440 and gets both wrong, then papers over failures with `unwrap_or(0)`. The `pre_release: Option<String>` representation cannot support correct ordering no matter how `Ord` is written. Given pre-1.0 and the stated willingness to rewrite internals: replace the body of `version.rs` with two real parsers behind one `Version` type — a semver path (structured `Vec<PreReleaseIdentifier>`, build metadata excluded from comparison) for ccu/ncu, and a PEP 440 path (epoch, release tuple of arbitrary length, `{a,b,rc}` pre, `.postN`, `.devN`, local) for pcu — with an explicit `Ecosystem` discriminant carried on the value so `Ord`, `is_prerelease`, and severity classification dispatch correctly. `VersionSpec` should likewise be a list of comparators per ecosystem rather than thirteen ad-hoc variants plus a `Complex` escape hatch that silently disables updating; the `Complex` fallthrough is how bug 4 stays invisible. `satisfies` and `calculate_in_range` should then share one definition (item 5), and `with_version` should preserve declared precision uniformly (item 8).
