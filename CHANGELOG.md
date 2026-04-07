# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Crate READMEs for crates.io landing pages

### Changed
- Renamed crates for publishing: `cargo-check-updates`, `python-check-updates`, `node-check-updates` (binaries remain `ccu`, `pcu`, `ncu`)
- Switched TLS backend from rustls to native-tls, significantly reducing dependency count
- Trimmed tokio features to only what's needed (rt-multi-thread, macros, sync)

### Fixed
- ccu now correctly identifies outdated dependencies when multiple versions of the same crate exist in `Cargo.lock` (e.g. a direct dep at 0.28.x and a transitive dep at 0.29.x)

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
