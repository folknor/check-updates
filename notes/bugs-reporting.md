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

## RPT-005 - Non-GitHub git installs are reported as up to date

Reported by ccu.

`ccu/src/global.rs::parse_github_url` takes the last two `/`-separated segments
of *any* URL, so `https://gitlab.com/o/r`, `https://codeberg.org/o/r`, a
self-hosted gitea and `https://git.sr.ht/~user/repo` all yield an
`(owner, repo)` and get sent to `https://api.github.com/repos/...`.
`check_github_repo` returns `None` on non-success, `check_git_updates` then
omits the package from its map, and `run_global_mode` pushes a `GlobalCheck`
with `has_update: false` - a non-GitHub git install is reported as up to date
rather than as unknown. There is no "could not determine" state in `GlobalCheck`
at all. The same silent-success-by-omission applies to GitHub rate limiting
(unauthenticated compare API, no token, no 403/429 handling).

## RPT-006 - Global mode's "concurrent" work is strictly serial and blocks the runtime

Reported by ccu and pcu-runtime.

- ccu: `check_path_updates` is a synchronous function that shells out to
  `git fetch` for every path install, serially, on the async runtime thread,
  *before* the progress bar exists - with no timeout, so it hangs on any
  unreachable remote. `check_git_updates` is likewise a sequential `for` loop of
  awaits despite sitting in `tokio::join!`, and bumps the bar to `len` only once,
  at the very end.
- pcu: both arms of `tokio::join!` in `run_global_mode` are
  `async { <blocking sync call> }`, and `get_python_info(true)` is called
  synchronously before the join. Three subprocess trees run strictly serially on
  the runtime thread. The comment says "concurrently".

Either `spawn_blocking` them or make them genuinely async.

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

## RPT-014 - ccu's global upgrade commands are wrong for dirty and for clean-but-dirty repos

Reported by ccu.

`global.rs::generate_upgrade_commands` emits
`cd <path> && git pull && cargo install --path .` for path installs even when
`has_dirty_changes` is true, where `git pull` will refuse. Conversely, a path
repo that is only dirty (0 commits behind) has `has_update: false`, so it renders
as "dirty" in the table but gets no command at all.

## RPT-015 - Table column widths are computed in bytes

Reported by core.

`core/src/output.rs` computes `max_name`/`max_from`/`max_to` with `str::len()`
and pads with `{:<name_w$}`, which also counts bytes. Any non-ASCII package name
or version string misaligns the table. `chars().count()` or a width crate is the
fix.

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
- `core::VersionSpec::max_major()` is unused and inconsistent: see VER-016.
