# File-rewriting defects (UPD)

0. Not every entry here is a bug. These documents were produced by automated
   hunters and mix genuine defects with opinions about how the tools ought to
   behave. Before acting on an entry, apply the test in
   `reference/resolution-principles.md`: a bug is the code contradicting
   something stated - its own doc comment, a README, the CLI help, a spec it
   claims to implement, or itself. A preference about semantics is a feature
   request; leave the behaviour alone and say so.
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

No longer blocked. The reason to wait was that
`pyproject.rs::find_line_in_content` returned a fuzzy substring hit with a
synthesized line-1 fallback, so a format-aware editor would only have been a
more confident way to edit the wrong node. That function is gone, locations are
now anchored, and an unproven location is `None` rather than a guess. What
remains is the structural change itself.

## UPD-008 - Fabricated and fuzzy line numbers feed a line-index rewriter

Reported by pcu-parsers (pcu), ccu (ccu), ncu (ncu). The pcu half - the only one
where the updater actually consumes the number - is fixed. Both fabricators are
gone: conda's `idx + 2` is replaced by a forward-only cursor over the raw YAML
that walks in step with the parsed sequence (exact lines, including `pip:`
nesting, with repeated items resolving to distinct lines), and pyproject's
whole-file `find_line_in_content` is deleted in favour of locating array items
by their quoted literal with an occurrence counter, and Poetry keys by `key =`
*inside the named table's span*, skipping comment lines.

Two halves remain, both in tools whose updater does not consume the number, so
these are `--json` correctness rather than data loss:

- `ccu/src/parsers/cargo_toml.rs::find_line_number` matches the first line whose
  trimmed text starts with the name followed by `=` or `.`, so a `[features]`
  entry like `tokio = []` earlier in the file wins over the real dependency
  line. It also re-reads the root `Cargo.toml` from disk once per inherited dep
  per member (O(members x deps) file reads).
- `ncu`'s `find_line_number` returns the first line containing `"<name>"`
  anywhere - the `"name"` field, a `scripts` entry, an `overrides`/`resolutions`
  block - falling back to 1. (Note ncu's updater no longer needs it at all: it
  locates the value by byte span.)

"Real dependency, location unproven" is now expressible:
`Dependency::line_number` is `Option<usize>` with `skip_serializing_if`, so an
unknown location is omitted from `--json` rather than shipped as a sentinel. The
pcu updater skips `None` before its bounds guard. Two *other* fabricated
fallbacks fell out with it - `find_line_number` in both
`ccu/src/parsers/cargo_toml.rs` and `ncu/src/parsers/package_json.rs` returned
`1` when they found nothing, so ccu reported the `[package]` header line for
every workspace-inherited dep.

The remaining ask is the byte span. It is deliberately a wave of its own: it
changes `core::Dependency`, all three parser families and all three updaters,
and the line-number half is now honest enough that nothing is bleeding while it
waits.

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
