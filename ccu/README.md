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
| `--json` | Emit machine-readable JSON on stdout (human-readable output is suppressed; warnings go to stderr) |

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

Schema: `{ schema_version, tool, mode, checks[], unchecked[], errors[] }`. `unchecked[]` lists dependencies that were found but could not be looked up (`name`, `source_file`, `section`); `errors[]` carries one entry per failed registry lookup (`package`, `kind`, `message`), where `kind` separates `not_found` from failures like a rate limit.

In `-g` mode there is no `unchecked[]`, and each check nests the installed package under `package`:

```
$ ccu -g --json | jq '.checks[] | {name: .package.name, source: .package.source, installed: .package.installed_version, latest: .latest_version}'
```

`package` carries `name`, `installed_version`, `source` (`registry` / `git` / `path`), `binaries`, `git_url`, `git_hash` and `local_path`. The check itself carries `latest_version`, `latest_hash`, `commits_behind`, `has_dirty_changes`, `has_update`, `check_failed` and `severity`. `check_failed: true` means `has_update: false` is "unknown", not "up to date".

## Supported files

- `Cargo.toml` (root + workspace members, glob patterns, auto-discovery)
- `Cargo.lock` (for installed version resolution)

## Related

Part of the [check-updates](https://github.com/folknor/check-updates) family:

- [python-check-updates](https://crates.io/crates/python-check-updates) - Python dependency checker (`pcu`)
- [node-check-updates](https://crates.io/crates/node-check-updates) - Node.js dependency checker (`ncu`)
