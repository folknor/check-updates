# pcu output-side review (registry, interpreters, updater, global, orchestration)

Read-only. Files: `/home/folk/Programs/check-updates/pcu/src/{main,cli,pypi,python,uv_python,updater,global,output}.rs`, plus `/home/folk/Programs/check-updates/core/src/{version,resolver,types,output}.rs` where questions led there.

## A. Correctness - version model, as used by the PyPI filter

**A1. PEP 440 post-releases are treated as pre-releases, producing downgrade advice.** `core/src/version.rs::parse_prerelease` lists `"post"` among the pre-release patterns. So `1.4.0.post1` parses with `pre_release = Some("post1")`, `is_prerelease()` is true, and `Ord` places it *below* `1.4.0`. Two consequences:
- `pypi.rs` filters every `.postN` release out of `filtered_versions` and out of `latest_stable`, so a package whose newest stable release is a post-release is reported one release behind.
- In global mode a locally installed `1.4.0.post1` compares `<` `1.4.0`, so `has_update` is true and pcu prints `1.4.0.post1 → 1.4.0` and generates an upgrade command for a **downgrade**. Same in project mode via `calculate_severity` (patch 0 vs 0 → severity `None`, so the row shows with an empty severity column and `-u` skips it, but the table still claims an update exists).
Post-releases are stable releases under PEP 440 and must sort above the base version.

**A2. Epoch versions are silently dropped.** `Version::from_str("1!2.0.0")` → `split('.')` yields `"1!2"` for major → parse fails → `Err`. In `pypi.rs::get_package` that release is skipped by the `if let Ok(version)` with no diagnostic. A package that has performed an epoch reset will have its newest releases invisible, and pcu will confidently report an older version as latest. `Ord` also has no epoch field, so even if parsed they would order wrong.

**A3. Hyphenated pre-releases lose their patch number.** `parse_prerelease` scans patterns in list order rather than by position. For `1.2.3-rc1`, `"rc"` matches at index 6 before `"-"` at index 5, so the base becomes `"1.2.3-"`, `parts[2] = "3-"`, `.parse().ok()` fails, and `unwrap_or(0)` silently yields **patch 0**. `1.2.3-rc1` is modelled as `1.2.0-rc1`. PyPI normalises to `1.2.3rc1` so the registry side is mostly safe, but user-authored specs in `requirements.txt`/`pyproject.toml` are parsed by the same code. The `unwrap_or(0)` on a *failed parse* (as opposed to a missing component) is the root smell - a malformed component should be an error, not zero.

**A4. `parse_prerelease` patterns match anywhere in the string with `idx > 0`.** Combined with the lowercase-find/original-slice pair (`s.to_lowercase().find(pattern)` then `&s[..idx]`), any non-ASCII case-changing character would desynchronise the byte index and panic on a non-boundary slice. Latent, but it is an unguarded slice on an index computed from a different string.

**A5. Yanked handling is coarser than the comment claims.** `pypi.rs` drops a version only when *every* file is yanked. A release with one yanked wheel and one live sdist is kept - defensible - but `yanked` also carries `#[allow(dead_code)]` while being read on line 94, i.e. the annotation is stale and would hide a genuine future dead field.

## B. `-uf` writes a different version than it reports (contract violation)

`main.rs` builds the display set from `c.target`, and `core/src/output.rs::print_row` prints `check.target`. But `updater.rs::apply_updates` under `force` writes `check.force_spec`, which `resolver.rs::calculate_force_spec` computes from **`latest`**, not `target`. For any dependency whose constraint caps it below latest, `pcu -uf` prints `Dependencies updated: requests 2.28.0 → 2.30.0 (2.32.3 available)` and then writes `2.32.3` into the file. The severity column is likewise computed from `target`, so a major bump can be displayed as `minor`. This is exactly the class of thing commit dc62172 ("Report only the updates that were actually applied") set out to fix; the severity-filter half was fixed, the force-target half was not.

## C. The updater reports work it did not do

**C1. `UpdateResult.modified_files` means "files we opened", not "files we changed".** `update_file` never checks whether `replace_version_in_line` actually altered anything, and every replacement path ends in an infallible fallback (`Ok(line.replace(old_spec, new_spec))`) that returns the line unchanged on no match. The file is then `fs::write`-ten regardless, `modified_files.insert` runs unconditionally, and `main.rs` prints `Updated N file(s)` and the header `Dependencies updated:`. Concrete ways to hit it:
- `line_idx >= lines.len()` → `continue` at updater.rs:118-120, silently.
- The parser normalises a spec the file writes differently. `VersionSpec::to_string()` emits `>=1.0,<2.0`; a file containing `>= 1.0, < 2.0` matches neither the name-qualified branch nor the fallback. Nothing is written; pcu says it updated the file.
This should be a comparison of new content against old, with files only recorded when they actually differ, and a per-check "not found in source line" surfaced to the user.

**C2. Unconditional `fs::write` on an unchanged file** bumps mtime and triggers watchers/rebuilds for a no-op run.

**C3. Non-atomic truncating write.** `fs::write` directly over the manifest. A crash or a full disk mid-write leaves a truncated `pyproject.toml`. Write-temp-then-rename is the norm for a tool whose whole job is editing other people's manifests.

**C4. CRLF files are silently converted to LF.** `content.lines()` strips the trailing `\r`; `lines.join("\n")` does not restore it. pcu reformats every line of a CRLF manifest including lines it had no business touching.

**C5. The fallback replacement is an unanchored whole-line substring substitution**, so it rewrites text outside the version spec: trailing comments (`flask==2.0.3  # pin matches 2.0.3 in docs`), environment markers, and any second occurrence of the spec on the line. Even the "good" requirements branch uses `String::replace`, which is global, not first-occurrence. The scope brief asks specifically what the updater does to comments and markers it did not write: it rewrites them.

**C6. `replace_in_pyproject` gates on `line.to_lowercase().contains(package_name)`** and then does a blind quoted-spec swap. For the PEP 621 form `"requests>=2.28.0",` the quoted form is `"requests>=2.28.0"`, not `">=2.28.0"`, so both quoted branches miss and it drops to the unanchored fallback. It is not TOML-aware at all: a `dependencies = [...]` array folded onto one line with two packages sharing a spec string will cross-contaminate through the fallback path. This is the strongest argument in the whole review for a rewrite: pcu should edit `pyproject.toml` through `toml_edit` (already a workspace dependency, already used by ccu) and `environment.yml` through a YAML-aware editor, keeping line-based replacement only for `requirements.txt`.

**C7. `detect_package_manager` returns `PackageManager::Uv` for any `pyproject.toml`** (acknowledged by its own comment), so a Poetry or PDM project is told `Run uv lock to sync dependencies`. Wrong, actionable-looking advice.

**C8. `replace_in_requirements` line 188-193**: `line.replace(...).into()` on a `String` bound by `if let Some(new_line)` - the `.into()` produces `Option<String>` and is always `Some`. Dead control flow dressed up as a fallible match.

## D. Subprocess discovery - failures are invisible

**D1. Every shell-out swallows failure identically to "nothing found".** `uv_python.rs::discover_and_check` returns `Ok(vec![])` when `uv` is missing, when it exits non-zero, and when it legitimately manages no Pythons. stderr is discarded in all cases. Same in `global.rs::discover_uv_tools` / `discover_pipx_packages` (`_ => Ok(Vec::new())`) and `python.rs::fetch_latest_python_version` (`ok()?`). A broken or hung `uv` is indistinguishable from a clean machine.

**D2. The `--json` envelope's `errors` array never carries interpreter or discovery errors.** `main.rs` line 182-185 does `Err(_) => Vec::new()` on `uv_python_checks` and passes `&fetch_errors` (PyPI only) as `errors`; the empty-packages branch at line 108-109 passes a literal `&[]`. So a `pcu -g --json` run where uv failed emits `"python_versions": []` with `"errors": []` - it asserts there are no uv Pythons rather than admitting it does not know.

**D3. Failed PyPI fetches vanish from `checks`.** Both modes do `if let Some(info) = package_infos.get(...)`. In `--json` the package appears only as a free-text string in `errors`, with no machine-readable name, so a consumer cannot distinguish "up to date" from "we could not check". And `pypi.rs::get_packages` errors out hard (propagated by `?` in main) only when *all* packages fail - partial and total failure are handled on opposite policies.

**D4. `python.rs::detect_python_version` only reads stdout.** Python 2 prints `--version` to stderr, so the `python` fallback it explicitly tries can never succeed for the interpreter it exists to catch. It also inspects whatever `python3` is on `PATH`, not the project's venv, while the header it feeds is printed above a project's dependency table.

**D5. "Python 3.11.9 (latest)" is not what it says.** `fetch_latest_python_version` filters `uv python list` to the *current major.minor series*, so pcu prints "(latest)" while 3.13 exists. With uv absent, `latest` is `None` and the header degrades to a bare `Python 3.11.9` with no signal that the check did not run.

**D6. `uv python list` is executed two or three times per invocation** (`python.rs::fetch_latest_python_version`, `uv_python.rs::discover_and_check`, and the unused third copy below). One call should feed all of them.

**D7. `python.rs::fetch_all_latest_python_versions` is dead** - no caller anywhere in the workspace, kept alive only by `pub`. It duplicates `uv_python.rs::latest_versions_from_uv_list` verbatim.

## E. uv Python discovery

**E1. Per-series "installed version" is the first row in uv's output, not the newest installed.** `discover_and_check` lines 164-176 use `seen_series` to skip after the first hit. If `uv python list` emits ascending, or emits a system interpreter before a uv-managed one, pcu reports the older patch as installed and tells you to `uv python install 3.11.14` when 3.11.14 is already there. Should take the max installed per series.

**E2. System interpreters are reported as uv-managed.** `uv python list` includes `/usr/bin/python3.12`; `parse_uv_python_list` keeps it (`is_installed` is just "line lacks `<download available>`"), the renderer files it under `uv-managed Python installations:`, and `generate_uv_python_upgrade_commands` proposes `uv python install X` for it - which installs a *separate* uv copy rather than upgrading the system one.

**E3. The "latest available" baseline depends on an unstated uv flag.** `latest_versions_from_uv_list` derives latest-per-series from the same listing, which only contains not-yet-installed builds because uv happens to include download-available rows by default. If that default changes (or `UV_PYTHON_DOWNLOADS=never` style config suppresses them), latest always equals installed and pcu reports everything up to date - a silent false negative, not an error.

**E4. `path` is `parts[1]` unconditionally** when installed, so any uv output variant that puts something other than a path in the second column (symlink arrows, annotations) stores garbage in a field serialised into the JSON envelope.

## F. Orchestration

**F1. `tokio::join!` in `run_global_mode` provides no concurrency.** Both arms are `async { <blocking sync call> }` - `discovery.discover()` and `discover_and_check()` (which itself calls `Command::output()` synchronously). `get_python_info(true)` is called synchronously before the join. The comment says "concurrently"; in practice three subprocess trees run strictly serially on the runtime thread, blocking it. Either `spawn_blocking` them or make them genuinely async.

**F2. The progress callback passes the spawn index, not a completion counter.** `pypi.rs` line 191 calls `callback(index + 1, total)`, and both call sites do `pb.set_position(current)`. With a 10-permit semaphore and out-of-order completion the bar jumps backwards and its final position is whichever task finished last, not `total`. Use an `AtomicUsize::fetch_add`.

**F3. Task-panic errors lose the package name** - `("unknown".to_string(), format!("Task failed: {e}"))` - and are then rendered under the heading `Packages not found on PyPI:`, which is a lie for a timeout, a 5xx, or a JSON parse failure. That heading is applied to every error in the vector regardless of cause.

**F4. `-m` means two different things.** In project mode it is "patch + minor". In global mode (`main.rs` 157-164) it is "highest version with the same major", i.e. it is a range restriction rather than a severity filter. Also, `if args.minor` is checked before force, so `pcu -g -mf` silently ignores `-f`.

**F5. `--json` help text is wrong.** `cli.rs`: "status messages go to stderr". Nothing is redirected to stderr; status messages are simply suppressed by `!args.json` guards. (indicatif writes to stderr on its own, which is the only thing that happens to match.)

**F6. `pcu --json -u` discards the update result** (`let _ = updater.apply_updates(...)`) and emits the unfiltered `checks` array, so a JSON consumer gets no record of which files were written or which dependencies were actually rewritten - the exact information the human-readable path prints.

**F7. `GlobalPackageDiscovery::_include_prerelease` is stored and never read.** `pcu -g -p` only affects the PyPI client, which is fine, but the field advertises a behaviour that does not exist.

## Recommended structural moves (given pre-1.0 and no time constraint)

1. Replace `core::Version` with a real PEP 440 model for pcu: epoch, release tuple of arbitrary length, and an explicitly ordered `pre < release < post` segment with `dev` below all. A shared `Version` across Cargo/PyPI/npm semantics is the source of A1-A4; the three ecosystems do not agree on ordering and the current lowest-common-denominator struct is wrong for at least one of them.
2. Rewrite `updater.rs` around format-aware editors (`toml_edit` for pyproject, a YAML editor for conda, a small span-based requirements editor that records byte ranges at parse time instead of re-locating the spec by string search). Have the parser hand the updater a byte span, not a line number plus a spec string to re-find.
3. Make `apply_updates` return per-check outcomes (`Written | Unchanged | NotFound | Skipped(reason)`), and drive both the table and the JSON envelope off that, so "what we said" and "what we wrote" come from one value. Write atomically.
4. Give registry/subprocess failures a typed place in the JSON envelope (`{name, kind, message}`), and stop conflating "tool absent" with "tool returned nothing".
