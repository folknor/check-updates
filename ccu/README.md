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
| `-g` | Check globally installed cargo binaries (crates.io, git, local path) |
| `-u` | Update `Cargo.toml` (patch updates only) |
| `-m` | Include minor updates (use with `-u` as `-um`) |
| `-f` | Force update all to absolute latest (use with `-u` as `-uf`) |
| `-p` | Include pre-release versions |
| `--json` | Emit machine-readable JSON on stdout (status messages go to stderr) |

### Example

```
$ ccu
Outdated dependencies:

  tokio       1.50.0 -> 1.51.0  minor
  serde       1.0.200 -> 1.0.210  patch

Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.
```

### JSON output

`--json` emits a versioned envelope. Versions and specs come through as strings.

```
$ ccu --json | jq '.checks[] | select(.severity != null) | {name: .dependency.name, installed, latest, severity}'
{
  "name": "tokio",
  "installed": "1.50.0",
  "latest": "1.51.0",
  "severity": "minor"
}
```

Schema: `{ schema_version, tool, mode, checks[], errors[] }`. In `-g` mode each check includes `source` (`registry` / `git` / `path`) and source-specific fields (`latest_version`, `git_url`, `git_hash`, `latest_hash`, `commits_behind`, `local_path`, `has_dirty_changes`).

## Supported files

- `Cargo.toml` (root + workspace members, glob patterns, auto-discovery)
- `Cargo.lock` (for installed version resolution)

## Related

Part of the [check-updates](https://github.com/folknor/check-updates) family:

- [python-check-updates](https://crates.io/crates/python-check-updates) - Python dependency checker (`pcu`)
- [node-check-updates](https://crates.io/crates/node-check-updates) - Node.js dependency checker (`ncu`)
