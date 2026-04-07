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
| `-u` | Update `package.json` (patch updates only) |
| `-m` | Include minor updates (use with `-u` as `-um`) |
| `-f` | Force update all to absolute latest (use with `-u` as `-uf`) |
| `-p` | Include pre-release versions |

### Example

```
$ ncu
Outdated dependencies:

  express     4.18.2 -> 4.21.0  minor
  typescript  5.4.5 -> 5.6.3  minor

Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.
```

## Supported files

- `package.json` (root + workspace members)
- Lock files: `package-lock.json` (npm), `pnpm-lock.yaml`, `yarn.lock`, `bun.lockb`

## Related

Part of the [check-updates](https://github.com/folknor/check-updates) family:

- [cargo-check-updates](https://crates.io/crates/cargo-check-updates) - Rust dependency checker (`ccu`)
- [python-check-updates](https://crates.io/crates/python-check-updates) - Python dependency checker (`pcu`)
