# node-check-updates

Check for outdated npm dependencies. Compares installed versions against the latest on the [npm registry](https://www.npmjs.com).

## Install

```
cargo install node-check-updates
```

## Usage

```
ncu [OPTIONS] [PATH]
```

Run `ncu` in a Node.js project directory to see outdated dependencies. Supports workspaces.

| Flag | Description |
|------|-------------|
| `-g` | Check globally installed packages (npm only for now) |
| `-u` | Update `package.json` (patch updates only) |
| `-m` | Include minor updates (use with `-u` as `-um`) |
| `-f` | Force update all to absolute latest (use with `-u` as `-uf`) |
| `-p` | Include pre-release versions |
| `--json` | Emit machine-readable JSON on stdout (human-readable output is suppressed; warnings go to stderr) |

### Example

```
$ ncu
Outdated dependencies:

  express     4.18.2 -> 4.21.0  minor
  typescript  5.4.5 -> 5.6.3  minor

Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.
```

### JSON output

`--json` emits a versioned envelope. Versions and specs come through as strings.

```
$ ncu --json | jq '.checks[] | select(.severity != null) | {name: .dependency.name, installed, latest, severity}'
{
  "name": "express",
  "installed": "4.18.2",
  "latest": "4.21.0",
  "severity": "minor"
}
```

Schema: `{ schema_version, tool, mode, checks[], errors[] }`.

## Supported files

- `package.json` (root + workspace members; `node_modules` is skipped when expanding workspace globs)
- Lock files: `package-lock.json` (npm), `pnpm-lock.yaml`, `yarn.lock`, `bun.lock`

Bun's text `bun.lock` is read for installed versions and is preferred when both bun lock files are present. The older binary `bun.lockb` is detected but cannot be read: ncu warns on stderr that the installed column is the ranges' base versions rather than what is installed, and suggests `bun install --save-text-lockfile`.

## Related

Part of the [check-updates](https://github.com/folknor/check-updates) family:

- [cargo-check-updates](https://crates.io/crates/cargo-check-updates) - Rust dependency checker (`ccu`)
- [python-check-updates](https://crates.io/crates/python-check-updates) - Python dependency checker (`pcu`)
