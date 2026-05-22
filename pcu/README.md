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
| `--json` | Emit machine-readable JSON on stdout (status messages go to stderr) |

### Example

```
$ pcu
Outdated dependencies:

  requests    2.31.0 -> 2.32.3  minor
  flask       3.0.0 -> 3.1.0  minor

Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.
```

### JSON output

`--json` emits a versioned envelope. Versions and specs come through as strings.

```
$ pcu --json | jq '.checks[] | select(.severity != null) | {name: .dependency.name, installed, latest, severity}'
{
  "name": "requests",
  "installed": "2.31.0",
  "latest": "2.32.3",
  "severity": "minor"
}
```

Schema: `{ schema_version, tool, mode, checks[], errors[] }`. In `-g` mode the envelope also carries `python_versions[]` for uv-managed Python series.

## Supported files

- `requirements.txt` (and variants like `requirements-dev.txt`)
- `pyproject.toml` (Poetry, PDM, uv, and standard `[project]` dependencies)
- `environment.yml` / `environment.yaml` (Conda)

## Known limitations

- 4-segment PEP 440 compatible release constraints (`~=1.4.5.0`) are not fully supported. The version parser only stores major.minor.patch, so `~=1.4.5.0` is treated as `~=1.4.5`. This is unlikely to matter in practice.

## Related

Part of the [check-updates](https://github.com/folknor/check-updates) family:

- [cargo-check-updates](https://crates.io/crates/cargo-check-updates) - Rust dependency checker (`ccu`)
- [node-check-updates](https://crates.io/crates/node-check-updates) - Node.js dependency checker (`ncu`)
