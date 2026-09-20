# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- An unbounded `>=` or `>` constraint now reports the true latest version that satisfies it, across major boundaries. `>=2.28.0` with 3.1.0 published used to be shown as a minor update to the newest 2.x, and `-um` would raise the floor to that 2.x; there is now one definition of "in range" (the constraint itself), the crossing is classified as a major update, and only `-uf` will write it. The consequence is that for an unbounded `>=`/`>` spec the in-range latest and the absolute latest are always the same version, so when a newer major exists the intermediate same-major release is no longer offered as a separate update.
- `-m` in global mode (`pcu -g -m`, `ncu -g -m`) now hides major updates from the report instead of retargeting each row to the newest same-major release. The upgrade commands printed in global mode (`uv tool upgrade --all`, `pipx upgrade-all`, `npm install -g <name>`) can only install latest, so the retargeted version was one the printed command would never install. A count of hidden majors is printed; `-f` lifts the filter, and `-g -mf` is no longer swallowed by `-m`.
- The `--json` `errors` array entries now carry `package` and a stable `kind` (`not_found`, `rate_limited`, `server_error`, `http_status`, `timeout`, `network`, `parse`, `no_usable_versions`, `no_stable_versions`, `task_failed`) alongside `message`. `ccu --json` adds an `unchecked` array listing dependencies crates.io never answered for, so "could not check" is distinguishable from "up to date".

### Fixed
- A rate-limited, offline or erroring registry is no longer reported as "not found". All three tools printed every fetch failure under a "not found on crates.io/PyPI/npm" header; only a 404 goes there now, everything else is listed under "could not be checked". `ccu -g` and `pcu -g` keep a row for such a package (marked `check_failed` in JSON) instead of dropping it from the table.
- Progress bars count completions rather than spawn order, so they no longer jump around when concurrent registry requests finish out of sequence.
- `ncu` respects `--pre-release` when choosing `latest`: without the flag a prerelease `dist-tags.latest` no longer becomes the `-uf` target, and with it the stable tag no longer caps the target below a newer prerelease. A package where no version string could be parsed is now an error instead of a silent `latest = 0.0.0` that rendered as up to date. Scoped names are percent-encoded in the registry path.
- `ncu` uses an allow-list of registry specifiers. `catalog:`, `patch:`, `portal:`, `exec:`, bare `user/repo`, tarball URLs and dist-tags (`latest`, `next`) are skipped instead of being sent to the registry as package names; `npm:<pkg>@<range>` aliases are checked under the aliased package and `-u` rewrites only the range, keeping the `npm:` prefix.
- A wildcard constraint (`==1.24.*`, conda `numpy=1.24`) in a project with no lockfile now has a current version to compare against, so it gets a target, a severity and is updatable; it used to be reported with no severity and never written. Wildcard matching compares parsed fields rather than registry text, and a compound spec containing `*` (`>=1.0,<2.*`) is no longer swallowed as one wildcard.
- `~=X.Y` / `~=X.Y.Z` and `~X` / `~X.Y` rewrites keep the precision the user declared. `~=1.4` used to be rewritten as `~=2.0.0`, which silently narrowed the constraint from "lock major" to "lock major and minor".
- `ccu` reads and writes `[target.'cfg(..)'.build-dependencies]`, and `ccu -u` warns about an update it selected but could not locate in the manifest instead of reporting the file as modified. All three updaters write manifests atomically through one shared implementation; a crash or full disk cannot leave a truncated `Cargo.toml`, `pyproject.toml` or `package.json`.
- `ccu -u` writes the file before printing "Dependencies updated:", so a write failure is no longer preceded by a claim of success.
- `pcu -g` reports a `uv` or `pipx` that is installed but failing, and a `uv python list` that returns no download rows (`--offline`, `UV_PYTHON_DOWNLOADS=never`), instead of treating both as a clean machine with nothing installed. The three discovery probes now run on the blocking pool concurrently rather than serially. The Python header distinguishes "latest in the 3.11 series, 3.14 available" from "latest" and says when the latest could not be determined; a system interpreter that uv merely found on PATH is no longer offered `uv python install` as an in-place upgrade. The newest installed build per series is reported, not the first row listed.
- Table rows no longer end in trailing whitespace when the severity column is empty.
- Prerelease versions are now parsed and ordered correctly. `1.2.3-rc1` was read as `1.2.0` - the patch number was silently dropped for every `-rc`, `-beta`, `-alpha`, `-dev`, `-post` and `-a`/`-b`/`-c` spelling - so comparisons, severity and update targets were computed against a version that did not exist, while the table displayed the original string. Prerelease identifiers are also compared per semver rather than as plain strings, so `beta.10` now sorts above `beta.2` and `dev` below `alpha`.
- A version segment that cannot be parsed is now rejected instead of silently becoming `0`. An npm spec like `1.x` used to parse as an exact pin of `1.0.0`, which `-u` would then write back as a pin.
- `ncu -u` no longer writes version strings that npm rejects. Specs were rendered through a PEP 440-flavoured formatter, so a plain `"express": "4.18.2"` was rewritten to `"==4.18.2"`, and ranges were written comma-separated. Any spec that cannot be rendered as valid npm syntax is now left alone rather than approximated.
- `ncu -u` no longer reformats `package.json`. The file was parsed to a value and re-serialised, which alphabetically reordered every key at every level and forced serde's indentation. Updates are now spliced into the original text, so key order, indentation, blank lines and the trailing newline survive.
- `pcu -u` no longer rewrites text outside the version spec. Replacement was an unanchored whole-line substring swap that could corrupt trailing comments, environment markers and a second occurrence of the spec on the same line, and could match a longer package name containing the shorter one. Replacements are now anchored to the package name and applied once.
- `pcu -u` preserves CRLF line endings and a missing final newline instead of converting the whole file to LF.
- Table columns no longer misalign for non-ASCII names or versions; column widths were measured in bytes while the padding that consumed them counted characters.
- `ccu -u` and `ncu -u` now rewrite only the manifest section a dependency was read from. The same package routinely appears in `[dependencies]` and `[dev-dependencies]` (or in several `[target.'cfg(..)']` tables, or in more than one npm section) at deliberately different versions; the updater used to sweep every section and overwrite versions it had never checked.
- `ccu -g` no longer reports a failed check as "up to date". Git installs on non-GitHub remotes, GitHub queries that fail or hit the rate limit, and path installs whose local repo could not be read are now reported as unknown rather than current, and are excluded from the generated upgrade commands.
- `ccu -g` git-remote checks now run concurrently instead of one await at a time, the local path-repo checks run alongside the network work instead of blocking before it, and the progress bar tracks both producers instead of jumping to full when the git phase ended.
- `pcu -u` writes manifests atomically and leaves a file (and its mtime) untouched when an update produces no effective change or when the version spec could not be located on its line, instead of reporting an update it did not make.
- `-u` and `-um` no longer list updates they skipped. The "Dependencies updated:" table was rendered from every outdated dependency regardless of the severity filter, so `ccu -um` would report a MAJOR bump as applied while the updater correctly left the file untouched. The table now shows only what is actually written, prints "No dependencies updated." when the filter excludes everything, and reports how many updates were skipped with a pointer to `-uf`. Applies to `ccu`, `pcu`, and `ncu`.
- `--json` no longer fabricates a `line_number` for a dependency whose declaration could not be located in its file. Every parser used to fall back to line `1` (ccu, ncu) or an array-index guess (pcu conda), and `pcu -u` would then rewrite whatever happened to be on that line. The field is now omitted when unknown, and `pcu -u` leaves such dependencies alone.
- `ncu` now reports the installed version the root project actually resolves to when a lock file holds several copies of a package: the hoisted top-level copy for npm and bun, and `importers["."]` for pnpm. The previous rule took whichever copy happened to iterate last; a nested copy that a transitive dependency pulled in could be reported as the project's installed version.
- `ncu` reads bun's text `bun.lock` (JSONC) for installed versions. The binary `bun.lockb` still cannot be read, and now says so instead of silently reporting every dependency's base version as installed.
- `pcu` reads conda's `name=1.24.0` as an exact pin again, so `-u` can update it. A prefix pin on a complete release can only match builds of that release; only a genuine prefix such as `name=1.24` is a wildcard, which `-u` will not narrow to an exact version.
- `pcu` now parses Python requirement strings with a real PEP 508 grammar walk shared by `requirements.txt`, `pyproject.toml` and conda's `pip:` section, instead of three separate substring scans. Concretely: a dependency with extras (`requests[security]>=2.28`) keeps its version constraint instead of being reported as unconstrained; a range written with the upper bound first (`django<3.0,>=2.0`) is reported as `django` instead of as a package literally named `django<3.0,`; environment markers and URL requirements no longer leak into the package name.
- `pcu` follows `-r` and `-c` includes in `requirements.txt`, so dependencies declared in an included file are checked, and each one records the file it actually lives in so `-u` rewrites the right one. Requirements split across `\` line continuations are joined before parsing instead of yielding a broken version string, and a `#egg=` fragment is no longer truncated as if it were a comment.
- `pcu` understands conda MatchSpec properly: the channel qualifier (`conda-forge::numpy`) is stripped from the package name, the space-separated form (`numpy 1.24.* py311_0`) no longer turns the whole line into the package name, and a build string is kept as part of a constraint that `-u` will not rewrite.
- `pcu` reads every `pyproject.toml` it finds, whatever package manager the project uses. A file with only `[dependency-groups]`, or a plain setuptools project, used to be skipped entirely; PEP 518 `[build-system] requires` and uv's `[tool.uv] dev-dependencies` are now read too. The detected package manager is only a label for the post-update sync hint. That label is also no longer guessed: a Poetry or PDM project is told to run `poetry lock` / `pdm lock` rather than `uv lock`.
- `pcu` lists `requirements*.txt` files in a stable order. Directory listing order is unspecified, so the table order - and which of two conflicting declarations won - could change between runs on the same project.
- `ccu` resolves the Cargo workspace root when run against a member crate. `ccu some-member/` used to read `Cargo.toml` and `Cargo.lock` from that directory alone, so every `.workspace = true` dependency resolved to nothing and dropped silently out of the report; it now reads the root manifest and the workspace lockfile like cargo does.
- `ncu` no longer walks into `node_modules/`, `bower_components/` or `jspm_packages/` when expanding workspace globs. A recursive glob such as `packages/**` used to pull in every installed transitive package's `package.json`, flooding the report with third-party manifests and letting `-u` rewrite files inside `node_modules`.
- `ncu` reports a package once per declaration instead of once per name. The same package declared in two workspace members, or in both `dependencies` and `devDependencies`, was collapsed to whichever was read first, so a member pinned to an older major range was silently hidden behind another member's newer one. Each declaration is now checked and reported against its own range, while the registry is still queried once per package.

## [0.4.0] - 2026-07-17

### Added
- `--json` flag on `ccu`, `pcu`, and `ncu` for machine-readable output. Emits a versioned envelope on stdout (`schema_version`, `tool`, `mode`, `checks`, `errors`; `pcu -g` also includes `python_versions`). Status messages and the upgrade hint are suppressed; progress bars stay on stderr. Works alongside `-u` (file edits still happen). Versions and version specs serialize as their canonical string form (e.g. `"1.0.150"`, `"^0.22"`).
- Every JSON check now carries `installed_released_at`, `target_released_at`, and `latest_released_at` (ISO-8601, omitted when the registry didn't return a date or the corresponding version isn't applicable). Dates come straight from crates.io's `created_at`, PyPI's `upload_time_iso_8601` (earliest file per release), and npm's `time` map.
- `ccu -g` flag to check globally installed cargo binaries for updates
  - **crates.io** packages: checks for newer versions on crates.io
  - **git** installs (e.g. `cargo install --git`): queries GitHub API to show how many commits behind
  - **local path** installs: detects dirty working trees and commits behind upstream via `git fetch`

### Fixed
- `ccu` workspace auto-discovery (bare `[workspace]` with no `members` field) now honors `.gitignore`, so gitignored directories - vendored checkouts, scratch dirs - are no longer walked and their unrelated `Cargo.toml` files no longer pollute the dependency set. `target/` and hidden dirs are still skipped as before.
- `ccu` now handles renamed cargo dependencies - `local_alias = { package = "upstream", ... }` - by querying crates.io with the upstream name. Previously the four renamed deps in lettre (e.g. `tokio1_crate` -> `tokio`) reported as "not found on crates.io"; they now resolve and update normally. Multiple aliases for the same crate dedupe to one row.
- `pcu -g` no longer suggests Python versions that uv hasn't built yet (e.g. recommending `uv python install 3.14.4` when uv only has 3.14.3). Both the header and uv-managed Python sections now use `uv python list` as the source of truth instead of endoflife.date API.

## [0.3.0] - 2026-04-07

### Added
- `ncu -g` flag to check globally installed npm packages
- Crate READMEs for crates.io landing pages
- Cargo-specific version spec serializer (`to_cargo_string()`) preserving operator semantics

### Changed
- Renamed crates for publishing: `cargo-check-updates`, `python-check-updates`, `node-check-updates` (binaries remain `ccu`, `pcu`, `ncu`)
- Switched TLS backend from rustls to native-tls, significantly reducing dependency count
- Trimmed tokio features to only what's needed (rt-multi-thread, macros, sync)
- ncu npm registry queries now rate-limited (semaphore of 10) with working progress bar
- Complex constraints (e.g. `>=2,<3,!=2.31.0`) are no longer falsely reported as in-range or auto-rewritten; the tool shows latest available without offering a rewrite

### Fixed
- ccu now correctly identifies outdated dependencies when multiple versions of the same crate exist in `Cargo.lock` (e.g. a direct dep at 0.28.x and a transitive dep at 0.29.x)
- ccu `--update` no longer drops operators from version specs (e.g. `>=1.0, <2.0` was rewritten as bare `1.x.y`)
- Wildcard version specs (`==1.2.*`) no longer incorrectly match `1.20.x`
- Wildcard precision preserved on update (`1.*` stays `2.*`, not narrowed to `2.3.*`)
- Compatible release (`~=X.Y`) now correctly allows any same-major version per PEP 440
- `pcu -gm` and `ncu -gm` no longer fall back to latest when no same-major version exists
- Dependencies with complex constraints and no lockfile are no longer silently hidden from review

## [0.2.0] - 2025-12-30

### Added
- `ccu` - Cargo/Rust dependency checker
- `ncu` - Node.js dependency checker (npm, pnpm, yarn, bun)
- Workspace support for all ecosystems
- Strict workspace-wide clippy lints

### Changed
- Restructured from single `python-check-updates` into multi-ecosystem workspace
- Edition 2024, MSRV 1.92
- Severity-based update filtering (`-u` patch only, `-um` minor, `-uf` all)

## [0.1.0] - 2025-12-29

Initial release of `python-check-updates` (`pcu`).

### Added
- Check outdated Python dependencies against PyPI
- Support for `requirements.txt`, `pyproject.toml` (Poetry, PDM, uv), `environment.yml`
- Global mode (`-g`) for uv tools, pipx, and pip --user packages
- Python version checking for uv
- In-place update support (`-u`, `-um`, `-uf`)
