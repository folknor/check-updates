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

## UPD-016 - ccu's updater has no "did this actually apply" signal

Surfaced while closing UPD-001, and made sharper by that fix.

`ccu/src/updater.rs::update_dependency` returns `()`, so `apply_updates`
reports every file it wrote as modified whether or not the lookup found
anything. This was survivable while the updater swept every section - a
mis-recorded section still hit the right entry somewhere. Now that a write is
scoped to the single recorded `section`, a stale or wrong value fails silently
with nothing to catch it.

`pcu` already threads an applied/not-applied result through its updater; `ccu`
does not. Same defect class as the "report only the updates that were actually
applied" work, left undone on the ccu side.

Related gap: no test covers a `target.'cfg(...)'` section round-trip.
`resolve_section` matches those by prefix/suffix rather than splitting on `.`,
because the target key is normally a quoted `cfg(...)` containing dots. That
reasoning is sound (both `toml` and `toml_edit` hand back unquoted keys) but is
only exercised through the plain `[dependencies]` case.

## UPD-005 - `apply_updates` does not report per-check outcomes

Reported by pcu-runtime. Narrowed: the dishonest-`modified_files` half is fixed
in `pcu/src/updater.rs` and `ncu/src/updater.rs` - both now record a file only
when its bytes actually changed, and the pcu replacement path fails closed
rather than silently returning the line unchanged.

The residue is the reporting shape. `apply_updates` still returns only a set of
modified files, so a check that found no anchor in the source line is
indistinguishable from one that was never attempted. The hunters' proposal -
`Written | Unchanged | NotFound | Skipped(reason)` per check, with both the
table and the JSON envelope driven off that one value - is unimplemented and
spans `main.rs` and `output.rs` in all three tools. Shares a fix with RPT-003.

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

## UPD-007 - pcu's pyproject rewriting is still not TOML-aware

Reported by pcu-runtime. Narrowed: the unanchored whole-line swap is gone.
`pcu/src/updater.rs` now anchors every replacement to a whole-token occurrence
of the package name, rewrites once, preserves trailing comments and environment
markers, and returns `None` rather than guessing when the spec is not where the
parser claimed.

The residue is that the updater still operates on one line of text. It has no
document, no table path and no key, so a `dependencies = [...]` array folded
onto one line is still addressed positionally rather than structurally. Going
format-aware through `toml_edit` requires addressing a dependency as (table
path, key), which `Dependency` does not carry: `manifest_key` is a Cargo-rename
field and the new `section` is a table *name*, not a path to an array element.

**Blocked on UPD-008.** While `pyproject.rs::find_line_in_content` returns a
fuzzy substring hit and a synthesized line-1 fallback, a format-aware editor
would only be a more confident way to edit the wrong node.

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

## UPD-009 - ncu and ccu still write manifests non-atomically

Reported by pcu-runtime and ncu. Narrowed: pcu now writes through a sibling
temp file and `fs::rename`, preserving permissions.

`ncu/src/updater.rs` and `ccu/src/updater.rs` still use a plain `fs::write`
straight over the manifest, so a crash or full disk mid-write leaves a truncated
`package.json` / `Cargo.toml`. The pcu implementation is the model to copy.

One caveat to copy knowingly rather than inherit: rename-into-place replaces a
*symlinked* manifest with a regular file, where the old `fs::write` wrote
through the link. Rare, but a behavior change, and pcu has it today.

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

## Structural recommendation, as filed by the hunters

pcu-runtime and ncu both argue the line/string-replacement approach is the root
cause and should be replaced by format-aware editors:

- `pyproject.toml` through `toml_edit` (already a workspace dependency, already
  used by ccu); `environment.yml` through a YAML-aware editor; line-based
  replacement kept only for `requirements.txt`.
- Parsers should hand the updater a **byte span** recorded at parse time, not a
  line number plus a spec string to re-find (UPD-008).
- `apply_updates` should return per-check outcomes
  (`Written | Unchanged | NotFound | Skipped(reason)`), with both the table and
  the JSON envelope driven off that one value, so "what we said" and "what we
  wrote" cannot diverge (UPD-005, UPD-006, RPT-003). Write atomically (UPD-009).

The `package.json` half of this has landed: `ncu/src/updater.rs` now splices the
value span in the original text, so key order, indentation and the trailing
newline survive `-u`, and it refuses to write any spec it cannot render as valid
npm syntax. The npm *range parser* it should be paired with does not exist yet -
a `Complex` spec such as `^17 || ^18` is now silently skipped at write time
rather than corrupted, but it is still displayed as checkable. See DSC-007.

`Dependency` now carries `section`, which was the missing piece behind UPD-001
and the ncu half. For a real `toml_edit` rewrite in pcu it is not sufficient: a
table *name* is not a path to an array element inside `project.dependencies`.
