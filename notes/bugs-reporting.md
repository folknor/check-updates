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

## RPT-001 - Every registry failure is reported as "not found"

Reported by ccu, pcu-runtime, ncu.

`ccu/src/main.rs` prints every entry in `fetch_errors` under "Crates not found
on crates.io:", but `get_package` pushes timeouts, 5xx, 429 rate limits and JSON
parse failures into the same bucket. A rate-limited run tells the user their
crates do not exist. pcu does the same under "Packages not found on PyPI:".

Task panics lose the package name entirely - recorded as the literal
`"unknown"` in both `pcu/src/pypi.rs` and `ncu/src/npm.rs` - so the affected
package disappears from the report behind a meaningless error line.

## RPT-002 - A package whose fetch failed vanishes from the results

Reported by ccu and pcu-runtime.

Both tools do `if let Some(package_info) = package_infos.get(...)`, so a failed
fetch drops the dependency from `checks` entirely: it is absent from the table
*and* from the JSON `checks` array. In `--json` it survives only as a free-text
string in `errors` with no machine-readable name, so a consumer cannot
distinguish "up to date" from "we could not check".

Related policy inconsistency in pcu: `get_packages` errors out hard (propagated
by `?`) only when *all* packages fail. Partial and total failure are handled on
opposite policies.

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

## RPT-004 - The progress bar reports the spawn index, not a completion count

Reported by ccu and pcu-runtime.

`callback(index + 1, total)` passes the enumeration position of the task that
just finished. With a bounded semaphore (5 permits in ccu, 10 in pcu),
completions arrive out of order, so `pb.set_position` jumps backwards (40 -> 7
-> 41) and the final position is whichever task finished last rather than
`total`. Should be a shared `AtomicUsize::fetch_add`.

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

## RPT-006 - pcu global mode's "concurrent" work is strictly serial

Reported by ccu and pcu-runtime. Narrowed: the ccu half is fixed.
`check_path_updates` is async and fans out over `spawn_blocking`;
`check_git_updates` runs concurrently under a 5-permit semaphore and reports
completions through a shared `AtomicUsize`.

The pcu half stands: both arms of `tokio::join!` in `run_global_mode` are
`async { <blocking sync call> }`, and `get_python_info(true)` is called
synchronously before the join, so three subprocess trees run strictly serially
on the runtime thread while the comment says "concurrently".

Note for whoever takes the ccu side further: there is no hard wall-clock
timeout on `git fetch`, because the workspace enables tokio's
`rt-multi-thread, macros, sync` only - no `time`, no `process`. The current
mitigation is git's own knobs (`http.lowSpeedLimit`/`lowSpeedTime`,
`GIT_TERMINAL_PROMPT=0`, `ssh -o BatchMode=yes -o ConnectTimeout=10`). A real
timeout means adding those two tokio features.

## RPT-007 - npm `latest` ignores the prerelease filter, and a total parse failure reads as "up to date"

Reported by core and ncu.

`ncu/src/npm.rs` filters `versions` by `include_prerelease` but takes `latest`
from the `dist-tags.latest` field with no such filter, so a package whose
`latest` tag points at a prerelease yields a `PackageInfo` whose `latest` is not
in `versions`. The resolver uses `package_info.latest` for the force/fallback
target, so ncu will recommend and, under `--force`, write a prerelease the user
asked to exclude.

The converse is also true: since `force_spec` and the fallback target both come
from `latest`, `ncu -p -uf` will never upgrade *to* a prerelease. The `-p` flag
only has an effect when a prerelease happens to fall inside the declared range.

Separately, the fallback `.unwrap_or_else(|| Version::new(0, 0, 0))` turns "no
parseable versions at all" into `latest = 0.0.0`, which compares below
everything and renders as "All dependencies are up to date!" - a total failure
presented as a clean result. ccu's equivalent path errors out.

Note: `latest_stable` is populated by all three registry clients and read by
nobody outside tests.

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

## RPT-010 - pcu's subprocess failures are indistinguishable from "nothing found"

Reported by pcu-runtime.

`uv_python.rs::discover_and_check` returns `Ok(vec![])` when `uv` is missing,
when it exits non-zero, and when it legitimately manages no Pythons; stderr is
discarded in all cases. Same in `global.rs::discover_uv_tools` /
`discover_pipx_packages` (`_ => Ok(Vec::new())`) and
`python.rs::fetch_latest_python_version` (`ok()?`). A broken or hung `uv` looks
exactly like a clean machine.

The `--json` envelope never carries these: `main.rs` does `Err(_) => Vec::new()`
on `uv_python_checks` and passes `&fetch_errors` (PyPI only) as `errors`, and
the empty-packages branch passes a literal `&[]`. So a `pcu -g --json` run where
uv failed emits `"python_versions": []` with `"errors": []` - asserting there
are no uv Pythons rather than admitting it does not know.

## RPT-011 - pcu's Python version reporting is wrong in several ways at once

Reported by pcu-runtime.

- `detect_python_version` only reads stdout, but Python 2 prints `--version` to
  stderr - so the `python` fallback that exists to catch Python 2 can never
  succeed. It also inspects whatever `python3` is on `PATH`, not the project's
  venv, while the header it feeds sits above a project's dependency table.
- "Python 3.11.9 (latest)" is not what it says:
  `fetch_latest_python_version` filters `uv python list` to the *current
  major.minor series*, so pcu prints "(latest)" while 3.13 exists. With uv
  absent, `latest` is `None` and the header degrades to a bare version with no
  signal that the check did not run.
- `uv python list` is executed two or three times per invocation.
- `python.rs::fetch_all_latest_python_versions` is dead - no caller in the
  workspace, kept alive by `pub`, and a verbatim duplicate of
  `uv_python.rs::latest_versions_from_uv_list`.

## RPT-012 - pcu's uv Python discovery misreads uv's output

Reported by pcu-runtime.

- Per-series "installed version" is the *first* row in uv's output, not the
  newest installed (`seen_series` skips after the first hit). If uv emits
  ascending, or emits a system interpreter before a uv-managed one, pcu reports
  the older patch and tells you to `uv python install 3.11.14` when 3.11.14 is
  already there.
- System interpreters are reported as uv-managed: `/usr/bin/python3.12` is kept
  (`is_installed` is just "line lacks `<download available>`"), filed under
  "uv-managed Python installations:", and given a `uv python install X` command
  that installs a *separate* uv copy rather than upgrading the system one.
- The "latest available" baseline depends on an unstated uv default: the listing
  only contains not-yet-installed builds because uv includes download-available
  rows by default. If that changes, latest always equals installed and pcu
  reports everything up to date - a silent false negative, not an error.
- `path` is `parts[1]` unconditionally when installed, so any uv output variant
  that puts something else in the second column stores garbage in a field
  serialised into the JSON envelope.

## RPT-013 - `-m` means two different things in pcu, and `-g -mf` ignores `-f`

Reported by pcu-runtime.

In project mode `-m` is "patch + minor" (a severity filter). In global mode
(`main.rs` 157-164) it is "highest version with the same major" - a range
restriction. And `if args.minor` is checked before force, so `pcu -g -mf`
silently ignores `-f`.

## RPT-015 - Table column widths are computed in bytes (three CLI renderers left)

Reported by core. Narrowed: fixed in `core/src/output.rs` only.

The filed mechanism was slightly wrong, and the correction matters for the
remaining sites. `{:<w$}` does *not* count bytes: string padding goes through
`Formatter::pad`, which measures in `chars().count()`. The defect is a unit
mismatch - widths computed in bytes, padding applied in chars - so a multi-byte
name over-pads its column by the number of UTF-8 continuation bytes. The fix is
to compute widths with `chars().count()` so both sides use one metric.

The same `.len()`-into-`{:<w$}` pattern is replicated verbatim in the three CLI
renderers, which the entry as filed did not name:

- `ccu/src/output.rs` - two width blocks (project table, global table)
- `pcu/src/output.rs` - two blocks (package table, series table)
- `ncu/src/output.rs` - one block

Adding `unicode-width` was considered and argued down: `Formatter::pad` has no
hook for a custom metric, so display-width correctness would mean hand-rolled
padding in every row printer, and no string that reaches these renderers can be
non-ASCII (crates.io, npm and PEP 508 names are all ASCII by grammar, as are
semver and PEP 440 versions). The reasoning is recorded at the code site.

## RPT-019 - `ccu/src/output.rs` shortens a git hash with an unguarded byte slice

Lateral finding from the RPT-015 work.

`&h[..7.min(h.len())]` guards the length but not char-boundary alignment: a
non-ASCII `h` whose byte 7 falls mid-codepoint panics. Not reachable today,
since git hashes are hex, but `str::get(..7)` or `chars().take(7)` removes the
sharp edge for free.

## RPT-020 - Workspace manifest warnings on every check run

Lateral finding, reported independently by four hunters in this wave.

`cargo` reports `workspace.package.rust-version` as unused at the root
`Cargo.toml`, and `package.readme` as inferable in all three binary crates.
Cosmetic, but they appear in the output of every `brokkr check` and so add
constant noise to every future wave's diagnostics.

## RPT-016 - Both registry clients fetch far more than they need, with no caching

Reported by ccu and ncu as lateral findings.

- `ccu/src/cratesio.rs` fetches the full `/crates/{name}` endpoint (entire
  version history, every dependency) for every crate when only versions and
  dates are needed; the sparse index or `/versions` would be far lighter.
- `ncu/src/npm.rs` fetches the full registry packument - megabytes for popular
  packages - when the abbreviated document would do for everything except the
  `time` map.
- Neither caches or issues conditional requests (no ETag / If-None-Match).

## RPT-017 - ncu interpolates scoped names into the URL unencoded

Reported by ncu.

`{registry}/@scope/name` works against registry.npmjs.org but is not the
documented form (`@scope%2Fname`) and will break against stricter mirrors.

## RPT-018 - Dead fields and leftover scaffolding advertising behavior that does not exist

Reported by ncu, pcu-runtime, core.

- `ncu/src/output.rs::GlobalTableRenderer::render` has dead `first_group`
  bookkeeping plus a `let _ = first_group;` to silence the warning - scaffolding
  for a second source that does not exist.
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
