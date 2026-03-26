# check-updates

Multi-ecosystem dependency update checker (Rust CLI tools). Monorepo with workspace members: `core`, `pcu`, `ccu`, `ncu`.

## Project Structure

- `core/` - Shared library: version parsing, dependency resolution, table rendering
- `ccu/` - Cargo/Rust dependency checker (`Cargo.toml`, `Cargo.lock`, workspaces)
- `pcu/` - Python dependency checker (`requirements.txt`, `pyproject.toml`, `environment.yml`)
- `ncu/` - Node.js dependency checker (`package.json`, lock files)

## Build & Test

```bash
cargo build                    # Build all workspace members
cargo test                     # Run all tests
cargo test -p ccu              # Run ccu tests only
cargo clippy --workspace       # Lint (strict workspace lints enforced, see root Cargo.toml)
cargo install --path ccu       # Install ccu locally
```

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

1. `detector.rs` - Finds `Cargo.toml` files (root + workspace members via glob expansion)
2. `parsers/cargo_toml.rs` - Extracts dependencies with version specs from `Cargo.toml`
3. `parsers/cargo_lock.rs` - Reads installed versions from `Cargo.lock`
4. `cratesio.rs` - Queries crates.io sparse index for latest versions
5. `updater.rs` - Applies version updates back to `Cargo.toml` (preserves formatting via `toml_edit`)
6. `main.rs` - Orchestrates: detect -> parse -> query -> resolve -> display -> update

## Resolution Semantics (ccu)

ccu compares the **installed version** (from `Cargo.lock`) against the **latest on crates.io**, not the declared spec in `Cargo.toml`. A dependency like `slint-build = "1.14"` with `Cargo.lock` already at `1.15.1` (the latest) is correctly reported as up to date — Cargo's semver range (`^1.14`) already covers it. ccu does not flag "stale specs" where the declared minimum is lower than the installed version. This is by design.

## Testing

17 tests across parser, detector, updater, and crates.io modules. Detector tests use `TempDir` to create temporary workspace layouts.
