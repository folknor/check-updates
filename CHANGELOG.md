# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
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
