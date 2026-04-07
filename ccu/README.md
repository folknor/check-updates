# cargo-check-updates

Check for outdated Rust dependencies. Compares installed versions from `Cargo.lock` against the latest on [crates.io](https://crates.io).

## Install

```
cargo install cargo-check-updates
```

## Usage

```
ccu [OPTIONS] [PATH]
```

Run `ccu` in a Rust project directory to see outdated dependencies. Supports workspaces.

| Flag | Description |
|------|-------------|
| `-u` | Update `Cargo.toml` (patch updates only) |
| `-m` | Include minor updates (use with `-u` as `-um`) |
| `-f` | Force update all to absolute latest (use with `-u` as `-uf`) |
| `-p` | Include pre-release versions |

### Example

```
$ ccu
Outdated dependencies:

  tokio       1.50.0 -> 1.51.0  minor
  serde       1.0.200 -> 1.0.210  patch

Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.
```

## Supported files

- `Cargo.toml` (root + workspace members, glob patterns, auto-discovery)
- `Cargo.lock` (for installed version resolution)

## Related

Part of the [check-updates](https://github.com/folknor/check-updates) family:

- [python-check-updates](https://crates.io/crates/python-check-updates) - Python dependency checker (`pcu`)
- [node-check-updates](https://crates.io/crates/node-check-updates) - Node.js dependency checker (`ncu`)
