# python-check-updates

Check for outdated Python dependencies. Compares installed versions against the latest on [PyPI](https://pypi.org).

## Install

```
cargo install python-check-updates
```

## Usage

```
pcu [OPTIONS] [PATH]
```

Run `pcu` in a Python project directory to see outdated dependencies.

| Flag | Description |
|------|-------------|
| `-g` | Check globally installed packages (uv tools, pipx, pip --user) |
| `-u` | Update dependency files (patch updates only) |
| `-m` | Include minor updates (use with `-u` as `-um`) |
| `-f` | Force update all to absolute latest (use with `-u` as `-uf`) |
| `-p` | Include pre-release versions |

### Example

```
$ pcu
Outdated dependencies:

  requests    2.31.0 -> 2.32.3  minor
  flask       3.0.0 -> 3.1.0  minor

Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.
```

## Supported files

- `requirements.txt` (and variants like `requirements-dev.txt`)
- `pyproject.toml` (Poetry, PDM, uv, and standard `[project]` dependencies)
- `environment.yml` / `environment.yaml` (Conda)

## Related

Part of the [check-updates](https://github.com/folknor/check-updates) family:

- [cargo-check-updates](https://crates.io/crates/cargo-check-updates) - Rust dependency checker (`ccu`)
- [node-check-updates](https://crates.io/crates/node-check-updates) - Node.js dependency checker (`ncu`)
