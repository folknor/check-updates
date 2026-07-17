# check-updates

Multi-ecosystem dependency update checker (Rust CLI tools). Monorepo with workspace members: `core`, `pcu`, `ccu`, `ncu`.

## Project Structure

- `core/` - Shared library: version parsing, dependency resolution, table rendering
- `ccu/` - Cargo/Rust dependency checker (`Cargo.toml`, `Cargo.lock`, workspaces)
- `pcu/` - Python dependency checker (`requirements.txt`, `pyproject.toml`, `environment.yml`)
- `ncu/` - Node.js dependency checker (`package.json`, lock files)

## Commands

Use `brokkr` (not `cargo`) for check/test. By default output is filtered to changed files and capped at 20 diagnostics per phase.

- `brokkr check` - gremlins + clippy + all tests (changed-files scope)
- `brokkr check --all` - show every diagnostic, no cap, no scope filter
- `brokkr check -p <crate>` - scope to one package (e.g. `-p app`). You generally do not want to run this; a single `brokkr check` is faster than 2-3 `-p` runs, and brokkr intelligently filters which warnings and errors to show you
- `brokkr check -- --test <file>` - forward args to `cargo test` (args after the second `--` go to the test binary)
- `brokkr test -p <crate> <NAME>` - release-mode focused single-test runner. Always passes `--release --include-ignored --nocapture --test-threads=1`. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time. Defaults to `--all-features`; runs a second sweep if `[check].consumer_features` is set in `brokkr.toml`. Gated off for litehtml/sluggrs (use `brokkr visual` there).
  - `-p, --package <PKG>` - cargo package. Required in this workspace - no default package, and overrides `[test] default_package` in `brokkr.toml` if set.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--raw` - bypass output filtering, print everything cargo emits.
  - `--debug` - build and run the test in dev profile instead of release. Use this for subprocess-lifecycle / IPC / boot-path tests where release-LTO compile time (3-4 min for the full workspace) dominates wall time and the optimization level doesn't change the behavior under test. `BROKKR_TEST_BIN_DIR` points at `<target>/debug` accordingly.
  - Example: `brokkr test -p common truncates_without_splitting` or `brokkr test -p calendar extract_tag_value_flattens_nested_text -N 5` or `brokkr test -p app terminal_failure_at_initial_boot_does_not_respawn --debug`.

`cargo install --path ccu` (or `pcu`/`ncu`) to install a binary locally.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.
- Subagents must always be launched in the foreground, (never use `run_in_background: true`) so the user can approve tool requests.

### Memory rules

Do not use your Memory functionality. Do not read, write, or update memories. Do not suggest saving things to memory. Durable context belongs in CLAUDE.md or the relevant docs.

### Bash rules

- Never chain commands with `&&`.
- Never chain commands with `;`.
- Never chain/pipe commands with `|`. Exception: piping into `review` is allowed (writing scratch prompt files is wasteful).
- Never capture stdout into env vars (`UUID=$(...)`).
- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.
- Never use `sed`, `find`, `awk`, `head`, `tail`, or complex bash commands.
- Never `find /`.
- Never run `git` with `-C <path>`
- One Bash() invocation === one command

### git commit rules

- Never commit markdown changes alone. Bundle them with upcoming code commits.
- When committing other changes: always tag along markdown files if dirty.
- Write substantive engineering-focused commit messages.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless the user explicitly asks. Stop after the commit.
- Any user-facing change (new feature, bug fix, behavior change) gets an entry under `## [Unreleased]` in `CHANGELOG.md`, in the matching `Added`/`Changed`/`Fixed` subsection. Include it in the same commit as the code change (CHANGELOG updates are exempt from the "never commit markdown alone" rule since they always ride along with code).

## Multi-Agent Orchestration

Do NOT use worktree isolation for parallel agents. Instead, launch agents in the same tree with strict file ownership - zero overlap.

Agent coordination rules:

- Each agent gets exclusive ownership of specific files. No two agents touch the same file.
- Agents must NOT run `cargo` or `brokkr`. The orchestrator validates between agents.

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

25 tests across parser, detector, updater, crates.io, and global modules. Detector tests use `TempDir` to create temporary workspace layouts.
