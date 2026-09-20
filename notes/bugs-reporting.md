# Reporting, registry and global-mode defects (RPT)

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.

Findings about what the tools tell the user and how they reach the network:
registry clients, error surfacing, the `--json` envelope, progress reporting,
global mode, and documentation that does not match the code.

## RPT-002 - A failed fetch cannot be represented as a `DependencyCheck`

Reported by ccu and pcu-runtime. The entry described a symptom; the cause is
structural and was found independently by two fixers in different crates.

`core::DependencyCheck.latest` is a non-`Option<Version>`, so there is no way to
construct a check meaning "we could not reach the registry for this". That is
why both tools do `if let Some(package_info) = ...` and drop the dependency
entirely: the type leaves them no alternative.

Worked around, not fixed, in two places:

- `ccu/src/main.rs` emits a separate `unchecked` array in the project JSON
  envelope (`name`, `source_file`, `section`), deduped by name.
- `pcu`'s global mode added `check_failed` to its own `GlobalCheck` and sets
  `latest` to the installed version, with a doc comment warning consumers not to
  read `has_update: false` on such a row as "up to date".

The real fix is in `core`: either `latest: Option<Version>` or a `check_failed`
flag on `DependencyCheck`, which `GlobalCheck` already has. Both workarounds
should be retired when it lands - ccu's `unchecked` array collapses back into
`checks` cleanly. The machine-readable name the entry asks for now exists as
`FetchError::package`.

Still true and unaddressed: pcu's `get_packages` errors out hard only when
*every* package fails, so partial and total failure are handled on opposite
policies.

## RPT-003 - `--json --update` discards the record of what was written

Reported by ccu, pcu-runtime, and implied by ncu.

All three do `let _ = updater.apply_updates(&checks, args.minor, args.force)?;`
and then emit the full unfiltered `checks` array. A JSON consumer has no way to
tell which dependencies were actually rewritten or which files changed - the
exact information commit dc62172 added for the human-readable path.
`modified_files` (and per-check outcomes, per UPD-005) should be in the
envelope.

Related, from ncu: in `--json` project mode `checks` contains *every* resolved
dependency including up-to-date ones, while the human path filters to updates.
Neither behavior is documented in the READMEs.

## RPT-024 - `uv python list` has a JSON output mode

Lateral finding from the RPT-012 work, and it makes that entry's whole class of
defect avoidable.

`uv python list --output-format json` exists on current uv. Every parsing bug
that was filed under RPT-012 - first-row-per-series, system interpreters read as
uv-managed, `parts[1]` taken unconditionally as a path - is a consequence of
scraping the text table. Switching to the JSON form, with the text parser kept
as a fallback for older uv, would remove the category.

Not done in the wave that found it, because learning the schema would have meant
running uv repeatedly against the real machine.

## RPT-027 - `FetchError` is triplicated across the three registry clients

`FetchError`, `FetchErrorKind`, the `classify_*` helpers and `warn_unparsed` are
roughly 120 identical lines in each of `ccu/src/cratesio.rs`,
`pcu/src/pypi.rs` and `ncu/src/npm.rs`, written independently from one
description.

The same shape happened with the atomic write and was consolidated into
`core::fs` after the three copies turned out to be byte-identical - agreement
that was luck rather than correctness. This one should move to `core` too. Only
`classify_transport` genuinely needs `reqwest` and can stay per crate.

## RPT-025 - pcu silently drops non-CPython and freethreaded interpreters

Lateral finding from the RPT-012 work.

`parse_uv_python_list` drops pypy, graalpy and every `+freethreaded` build
without a word. A user whose only 3.13 is freethreaded gets no row and no
explanation - the same silent-omission class as RPT-010, which was about
subprocess failures being indistinguishable from a clean machine.

## RPT-020 - Workspace manifest warnings on every check run

Lateral finding, reported independently by four hunters in this wave.

`cargo` reports `workspace.package.rust-version` as unused at the root
`Cargo.toml`, and `package.readme` as inferable in all three binary crates.
Cosmetic, but they appear in the output of every `brokkr check` and so add
constant noise to every future wave's diagnostics.



1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.

Findings about what the tools tell the user and how they reach the network:
registry clients, error surfacing, the `--json` envelope, progress reporting,
global mode, and documentation that does not match the code.
