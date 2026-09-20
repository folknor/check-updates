# Reporting, registry and global-mode defects (RPT)

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

## RPT-005 - An unknown git-install check is not surfaced to the user

Reported by ccu. Narrowed: the lying half is fixed in `ccu/src/global.rs`.
`parse_github_url` now accepts only github.com forms, `GitStatus` and
`PathStatus` carry `unknown`, every git- and path-sourced package gets an entry
inserted whatever happens, and `GlobalCheck::check_failed` suppresses the
severity. A non-GitHub remote, a transport failure, a 404 and a 403/429 rate
limit all now resolve to "unknown" rather than "up to date".

`ccu/src/output.rs::render_commits_group` now renders those rows as
`could not check` instead of filtering them out, so the state reaches the user.

Two pieces of residue:

- Path installs have the same defect one layer down. `check_local_git_repo`
  treats a failing `git rev-list HEAD..@{upstream}` - no upstream configured, or
  an offline fetch - as `commits_behind: 0, unknown: false`, i.e. current. Only
  a `rev-parse HEAD` failure is marked unknown.
- The GitHub compare API is still called unauthenticated - 60 requests/hour
  shared per IP - so a user with more than a handful of git installs now gets
  a table full of honest "unknown" instead of a table full of wrong "up to
  date". Reading `GITHUB_TOKEN` / `GH_TOKEN` would make the feature usable.

## RPT-008 - The `--json` help text promises stderr routing that does not exist

Reported by ccu, pcu-runtime, ncu.

`cli.rs` and the READMEs say "status messages go to stderr". Nothing in any of
the three tools ever writes to stderr; every status line is `println!`,
suppressed individually by `!args.json` guards. It happens to hold only because
each line is guarded, and one un-guarded `println!` corrupts the JSON stream.
(The indicatif bar does go to stderr on its own, which is the only part that
matches.)

## RPT-009 - `ccu/README.md` documents the wrong shape for the `-g` JSON

Reported by ccu.

The README says each check includes `source` and source-specific fields
(`latest_version`, `git_url`, `git_hash`, ..., `local_path`) at the check level.
In `main.rs`, `GlobalCheckJson` flattens a `GlobalCheck` whose
`package: GlobalPackage` is a *nested* object, so `source`, `git_url`,
`git_hash`, `local_path` and `binaries` live under `.package.*`. A `jq` filter
written from the README fails.

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

## RPT-026 - pcu project mode has the false-negative that global mode fixed

`fetch_latest_python_versions` computes "latest in series" from every row in the
`uv python list` output, including installed ones. A listing with no
download-available rows - `--offline`, `UV_PYTHON_DOWNLOADS=never`, or a config
setting - therefore reports the installed version as the latest, silently.

Global mode closed exactly this hole with a `NoDownloadBaseline` error, on the
reasoning that a baseline computed from installed rows alone is meaningless
rather than merely incomplete. Project mode needs the same check. It is a false
negative, not an error, which is why it will not show up in any failure count.

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

## RPT-018 - Dead fields and leftover scaffolding advertising behavior that does not exist

Reported by ncu, pcu-runtime, core.

- The ncu `first_group` scaffolding is removed. Judgement recorded at the site
  for when a second global source arrives: multi-source globals (pnpm/yarn)
  are a *discovery* feature first, the grouping is the trivial half, and ccu's
  `GlobalTableRenderer::render` is the working three-source model to copy.
- `pcu`'s `GlobalPackageDiscovery::_include_prerelease` is stored and never
  read. `pcu -g -p` only affects the PyPI client.
- `pypi.rs`'s `yanked` field carries `#[allow(dead_code)]` while being read, so
  the annotation is stale and would hide a genuinely dead field later. (The
  hunter judged the yanked *policy* - drop a release only when every file is
  yanked - defensible.)
- `core::VersionSpec::max_major()` was unused and inconsistent; it has since
  been deleted, with the reasoning recorded at the site.

## RPT-021 - `GitStatus::commits_behind` is populated from `ahead_by`

Lateral finding from the RPT-005 work.

`ccu/src/global.rs::check_github_repo` reads `ahead_by` from the GitHub compare
response - commits on HEAD that the installed hash lacks, which is the right
number - and stores it in a field named `commits_behind`. The value is correct
and the name inverts it. A comment or a rename, not a behavior change.
