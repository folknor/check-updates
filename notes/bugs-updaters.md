# File-rewriting defects (UPD)

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.

Findings about the one operation in these tools that mutates the user's files:
`ccu/src/updater.rs`, `ncu/src/updater.rs`, `pcu/src/updater.rs`.

## UPD-001 - ccu rewrites a crate name in every section of the manifest

Reported by ccu. Called "a data-loss-class bug in the one operation that mutates
the user's files".

`update_dependency` walks `dependencies`, `dev-dependencies`,
`build-dependencies`, `workspace.dependencies` and every
`target.*.{dependencies,dev-dependencies}`, and writes `new_version` into *all*
entries matching the key. It has no notion of which section the
`DependencyCheck` actually came from: `Dependency` carries `source_file` but not
the section it was parsed from, so the information is not captured at parse
time.

Concretely: `foo = "1.0"` under `[dependencies]` and `foo = "0.9"` under
`[dev-dependencies]` produce two checks with different targets; each application
writes *both* entries, so the dev-dependency gets force-bumped across a major
(or the normal dep gets downgraded) depending on the iteration order of the
`HashMap<PathBuf, Vec<...>>`. Which one wins is nondeterministic across runs.

No amount of care inside `update_dependency` fixes this without `Dependency`
carrying the manifest section (and ideally a `toml_edit` path).

## UPD-002 - ncu writes version strings that are not valid npm syntax

Reported by ncu as "the single worst bug in scope".

`ncu/src/updater.rs` builds the replacement with `spec.to_string()`, i.e.
`impl Display for VersionSpec` in `core/src/version.rs`, which is
Python/PEP-440 flavoured:

- `Pinned(v)` -> `"==4.18.2"`. A bare `"express": "4.18.2"` parses to `Pinned`,
  so exact-pinned deps - extremely common - are rewritten to `"==4.18.2"`, which
  npm rejects.
- `Wildcard` -> `"==1.2.*"`, `Compatible` -> `"~=1.2.3"`, `Range` ->
  `">=1.0.0,<2.0.0"` (npm ranges are space-separated, not comma-separated).

Only `Caret`/`Tilde`/`Minimum`/`Maximum`/`GreaterThan`/`LessThan` round-trip.
ccu has `to_cargo_string()` for exactly this reason; there is no
`to_npm_string()` and ncu never asks for one.

## UPD-003 - `ncu -u` alphabetically reorders the entire package.json

Reported by ncu.

`update_file` does `serde_json::from_str::<Value>` -> mutate ->
`to_string_pretty` -> overwrite. `serde_json` is declared as plain `"1"` in the
root `Cargo.toml` with no `preserve_order` feature, so `Value::Object` is a
`BTreeMap`: every key at every nesting level comes back sorted.
`"name"`/`"version"`/`"scripts"`/`"dependencies"` get shuffled, dependency order
inside each table is re-sorted, indentation is forced to serde's 2-space style,
and the original layout is gone. The comment directly above the call says
"update, and write back preserving formatting", which is false. ccu uses
`toml_edit` precisely to avoid this.

## UPD-004 - ncu writes the name into every dependency table it appears in

Reported by ncu. Same shape as UPD-001.

`update_dependency` loops over `dependencies`, `devDependencies`,
`peerDependencies` and `optionalDependencies` and sets the value in *all* of
them containing the key. If a package is in `dependencies` at `^1.0.0` and in
`peerDependencies` at `^1 || ^2`, a `-u` run clobbers the peer range with the
dependency's new pin. Peer ranges are deliberately wide; bumping them to the
resolved latest is a semantic change nobody asked for. As with ccu,
`Dependency` has no field recording the source table (`manifest_key` is for
Cargo renames only).

## UPD-005 - pcu's `modified_files` means "files we opened", not "files we changed"

Reported by pcu-runtime.

`update_file` never checks whether `replace_version_in_line` altered anything,
and every replacement path ends in an infallible fallback
(`Ok(line.replace(old_spec, new_spec))`) that returns the line unchanged on no
match. The file is `fs::write`-ten regardless, `modified_files.insert` runs
unconditionally, and `main.rs` prints `Updated N file(s)` under the header
`Dependencies updated:`. Ways to hit it:

- `line_idx >= lines.len()` - `continue`, silently (updater.rs:118-120).
- The parser normalises a spec the file writes differently:
  `VersionSpec::to_string()` emits `>=1.0,<2.0`, and a file containing
  `>= 1.0, < 2.0` matches neither the name-qualified branch nor the fallback.
  Nothing is written; pcu says it updated the file.

Should compare new content against old, record a file only when it differs, and
surface a per-check "not found in source line".

## UPD-006 - `pcu -uf` writes a different version than it reports

Reported by pcu-runtime.

`main.rs` builds the display set from `c.target` and `core/src/output.rs::print_row`
prints `check.target`, but `apply_updates` under `force` writes
`check.force_spec`, which `calculate_force_spec` computes from **`latest`**, not
`target`. For any dependency whose constraint caps it below latest, `pcu -uf`
prints `requests 2.28.0 -> 2.30.0 (2.32.3 available)` and then writes `2.32.3`.
The severity column is computed from `target` too, so a major bump can display
as minor.

The hunter notes this is the same class as commit dc62172 ("Report only the
updates that were actually applied"): the severity-filter half was fixed, the
force-target half was not. The same `force_spec`-vs-`target` split exists in ccu
and ncu, which the hunters did not separately test.

## UPD-007 - pcu's replacement is an unanchored whole-line substring swap

Reported by pcu-runtime.

The fallback rewrites text outside the version spec: trailing comments
(`flask==2.0.3  # pin matches 2.0.3 in docs`), environment markers, and any
second occurrence of the spec on the line. Even the "good" requirements branch
uses `String::replace`, which is global rather than first-occurrence.

`replace_in_pyproject` gates on `line.to_lowercase().contains(package_name)` and
then does a blind quoted-spec swap. For the PEP 621 form `"requests>=2.28.0",`
the quoted form is `"requests>=2.28.0"`, not `">=2.28.0"`, so both quoted
branches miss and it drops to the unanchored fallback. It is not TOML-aware at
all: a `dependencies = [...]` array folded onto one line with two packages
sharing a spec string cross-contaminates through that path.

## UPD-008 - Fabricated and fuzzy line numbers feed a line-index rewriter

Reported by pcu-parsers (pcu), ccu (ccu), ncu (ncu). `pcu/src/updater.rs`
indexes `lines[line_number - 1]` and rewrites in place, so in pcu this is
actively dangerous rather than merely cosmetic.

- `pcu/src/parsers/conda.rs` invents `line_number = idx + 2` from the array
  index, ignoring `name:`/`channels:` blocks entirely. In its own test fixture
  the first dep is at file line 6 and gets 2. `-u` on an `environment.yml`
  therefore rewrites arbitrary lines, guarded only by
  `line.replace(old_spec, new_spec)` no-op'ing when nothing matches.
- `pcu/src/parsers/pyproject.rs::find_line_in_content` is a case-insensitive
  substring search over the whole file: it matches comments, the `name = "..."`
  key, and longer packages containing the shorter one (`requests` matches
  `requests-oauthlib`, `pytest` matches `pytest-cov`). First match wins.
  Fallback is line 1 with a synthesized `pkg = "spec"` string that never existed
  in the file.
- `ccu/src/parsers/cargo_toml.rs::find_line_number` matches the first line whose
  trimmed text starts with the name followed by `=` or `.`, so a `[features]`
  entry like `tokio = []` earlier in the file wins over the real dependency
  line. It also re-reads the root `Cargo.toml` from disk once per inherited dep
  per member (O(members x deps) file reads).
- `ncu`'s `find_line_number` returns the first line containing `"<name>"`
  anywhere - the `"name"` field, a `scripts` entry, an `overrides`/`resolutions`
  block - falling back to 1.

All four are serialized into `--json` as fact. In ccu and ncu the updater does
not use them (luckily); in pcu it does.

## UPD-009 - Non-atomic truncating writes

Reported by pcu-runtime and ncu.

Both write with a plain `fs::write` straight over the manifest. A crash, an
interrupt or a full disk mid-write leaves a truncated `pyproject.toml` /
`package.json`. Write-temp-then-rename is the norm for a tool whose entire job
is editing other people's manifests.

## UPD-010 - CRLF files are silently converted to LF

Reported by pcu-runtime and pcu-parsers.

`content.lines()` strips the trailing `\r`; `lines.join("\n")` does not restore
it. pcu reformats every line of a CRLF manifest, including lines it had no
business touching.

## UPD-011 - pcu writes unchanged files unconditionally

Reported by pcu-runtime.

An `fs::write` on a file with no effective change still bumps mtime and triggers
watchers and rebuilds for a no-op run. Related to UPD-005.

## UPD-012 - ccu prints "Dependencies updated:" before performing the write

Reported by ccu.

In non-JSON update mode the table is rendered before `apply_updates` is called.
If the write fails (read-only file, TOML parse failure) the process has already
claimed success, then errors out.

## UPD-013 - pcu's post-update advice names the wrong package manager

Reported by pcu-runtime.

`detect_package_manager` returns `PackageManager::Uv` for any `pyproject.toml`
(acknowledged by its own comment), so a Poetry or PDM project is told to
`Run uv lock to sync dependencies`. Wrong, actionable-looking advice. See also
DSC-018, where the detector's manager sniffing is wrong in the other direction.

## UPD-014 - `[target.*.build-dependencies]` is handled by neither parser nor updater

Reported by ccu.

`ccu` reads `[target.*.dependencies]` and `[target.*.dev-dependencies]` but not
`[target.*.build-dependencies]`; the updater has the same gap. Consistently
incomplete in both halves.

## UPD-015 - Dead control flow in `replace_in_requirements`

Reported by pcu-runtime.

Lines 188-193: `line.replace(...).into()` on a `String` bound by
`if let Some(new_line)` - the `.into()` produces `Option<String>` and is always
`Some`. Fallible-looking code that cannot fail.

## Structural recommendation, as filed by the hunters

pcu-runtime and ncu both argue the line/string-replacement approach is the root
cause and should be replaced by format-aware editors:

- `pyproject.toml` through `toml_edit` (already a workspace dependency, already
  used by ccu); `environment.yml` through a YAML-aware editor; line-based
  replacement kept only for `requirements.txt`.
- `package.json` through a text-span JSON editor that edits the value span in
  the original string, so formatting and key order survive `-u` (UPD-003), paired
  with a real npm range parser that rewrites only the comparator it is allowed to
  touch and refuses anything it did not fully understand (see DSC-007).
- Parsers should hand the updater a **byte span** recorded at parse time, not a
  line number plus a spec string to re-find (UPD-008).
- `apply_updates` should return per-check outcomes
  (`Written | Unchanged | NotFound | Skipped(reason)`), with both the table and
  the JSON envelope driven off that one value, so "what we said" and "what we
  wrote" cannot diverge (UPD-005, UPD-006, RPT-003). Write atomically (UPD-009).
- `Dependency` needs to carry the manifest *section* it came from, which is the
  missing piece behind UPD-001 and UPD-004.
