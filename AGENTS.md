# check-updates

Multi-ecosystem dependency update checker (Rust CLI tools). Monorepo with workspace members: `core`, `pcu`, `ccu`, `ncu`.

## Project Structure

- `core/` - Shared library: version parsing, dependency resolution, table rendering
- `ccu/` - Cargo/Rust dependency checker (`Cargo.toml`, `Cargo.lock`, workspaces)
- `pcu/` - Python dependency checker (`requirements.txt`, `pyproject.toml`, `environment.yml`)
- `ncu/` - Node.js dependency checker (`package.json`, lock files)

## Commands

Use `brokkr` (not `cargo`) for check/test. Output is never capped or scoped: every diagnostic prints every time, and errors in files with unstaged changes are listed first.

- `brokkr check` - gremlins + clippy + all tests
- `brokkr check -p <crate>` - scope to one package (e.g. `-p ccu`). You generally do not want to run this; a single `brokkr check` is faster than 2-3 `-p` runs
- `brokkr check -- --test <file>` - forward args to `cargo test` (args after the second `--` go to the test binary)
- `brokkr test -p <crate> <NAME>` - release-mode focused single-test runner. Always passes `--release --include-ignored --nocapture --test-threads=1`. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time. Defaults to `--all-features`; runs a second sweep if `[check].consumer_features` is set in `brokkr.toml`.  - `-p, --package <PKG>` - cargo package. Required in this workspace - no default package, and overrides `[test] default_package` in `brokkr.toml` if set.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--raw` - bypass output filtering, print everything cargo emits.
  - `--debug` - build and run the test in dev profile instead of release. Use this for subprocess-lifecycle / IPC / boot-path tests where release-LTO compile time dominates wall time and the optimization level doesn't change the behavior under test. `BROKKR_TEST_BIN_DIR` points at `<target>/debug` accordingly.

`./release.sh` builds release and installs `ccu`, `pcu` and `ncu` to `~/.local/bin/`.

## Workspace Conventions

- Shared dependencies are defined in root `Cargo.toml` under `[workspace.dependencies]` and referenced via `.workspace = true` in member crates
- `[workspace.package]` sets shared version, edition (2024), rust-version (1.92), license, repository
- `[workspace.lints.clippy]` enforces strict lints across all members (unwrap_used = deny, etc.)

## Code Style

- Edition 2024, MSRV 1.92
- `#[deny(clippy::unwrap_used)]` - use `?`, `.expect()` with context, or handle errors explicitly
- Uses `anyhow` for application errors, `thiserror` for library error types
- Async runtime: `tokio` with full features
- TOML parsing: `toml` crate for reading, `toml_edit` for preserving-format writes
- No `.unwrap()` in non-test code

## Architecture (ccu)

**Project mode** (`ccu [PATH]`):
1. `detector.rs` - Finds `Cargo.toml` files (root + workspace members via glob expansion)
2. `parsers/cargo_toml.rs` - Extracts dependencies with version specs from `Cargo.toml`
3. `parsers/cargo_lock.rs` - Reads installed versions from `Cargo.lock`
4. `cratesio.rs` - Queries crates.io API for latest versions
5. `updater.rs` - Applies version updates back to `Cargo.toml` (preserves formatting via `toml_edit`)
6. `main.rs` - Orchestrates: detect -> parse -> query -> resolve -> display -> update

**Global mode** (`ccu -g`):
1. `global.rs` - Parses `~/.cargo/.crates.toml` to discover installed binaries (registry, git, path sources)
2. For registry crates: queries crates.io for latest versions via `cratesio.rs`
3. For git installs: queries GitHub compare API to check commits behind
4. For path installs: runs `git fetch` + `git rev-list` to check upstream, flags dirty working trees
5. `output.rs` - Renders results grouped by source type, generates upgrade commands

## Resolution Semantics (ccu)

ccu compares the **installed version** (from `Cargo.lock`) against the **latest on crates.io**, not the declared spec in `Cargo.toml`. A dependency like `slint-build = "1.14"` with `Cargo.lock` already at `1.15.1` (the latest) is correctly reported as up to date - Cargo's semver range (`^1.14`) already covers it. ccu does not flag "stale specs" where the declared minimum is lower than the installed version. This is by design.

## Testing

Detector tests use `TempDir` to create temporary workspace layouts.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.

### Bash rules

- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.

## Document folders

The standing layout, across every project. Three live folders plus one retired,
split by durability first, subject second.

| Folder | Contents | Rule |
|---|---|---|
| `reference/` | Durable in-repo reference for anyone working on or with the code - how the thing is built and why: `architecture.md`, `technical-implementation-spec.md`, `performance.md` (the durable record of measured numbers over time), invariants, protocol contracts | Citable from source as a source of truth. What it says must be true. |
| `docs/` | Durable in-repo documentation of how the thing is used - guides, CLI reference, the consumer-facing API surface. Sometimes exposed as a hand-edited VitePress gh-pages site | Same must-be-true rule. |
| `notes/` | Transient - work items (`todo.md`), future plans, hypotheticals, bug reports, research, analysis. Things that will die | No truth guarantee. Nothing durable cites it. |
| `plans/` | Retired | Plan documents are transient: they go in `notes/`. |

`reference/` and `docs/` are both durable and both binding. The difference is
subject, not audience: `reference/` covers how the thing is built and why - what
you need in order to change it safely - while `docs/` covers how it is used. A
developer or library consumer reads both. Where a project publishes a site,
`docs/` is what gets published; the folder means the same thing either way.
`notes/` is neither durable nor binding, which is the whole point of keeping it
separate: a document that may be wrong must not sit where a document that must
be right is expected.

The dependency direction is therefore one-way. `notes/` may cite `docs/` and
`reference/`; nothing durable may cite `notes/` - not a code comment, not
`docs/`, not `reference/`. A code comment must carry its full context, because
it outlives the note.

**Root-level convention files are exempt.** `AGENTS.md`, `CLAUDE.md`,
`README.md`, `LICENSE`, `CHANGELOG.md` and their kin are found by tooling and by
convention at the repository root, and stay there. These folders govern
documents we chose where to put, not files whose location is dictated.

In `notes/`, `docs/` and `reference/` alike, avoid citing source line numbers -
they drift fast.
