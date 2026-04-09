# check-updates

Check for outdated dependencies. Supports Python, Rust, and npm ecosystems.

Built with LLMs. See [LLM.md](LLM.md).


## Install

```
cargo install --path pcu   # Python
cargo install --path ccu   # Cargo/Rust
cargo install --path ncu   # npm/pnpm/yarn/bun
```

## Usage

```
pcu [PATH]          # Check Python project
pcu -g              # Check global packages (uv tools, pipx, pip --user)
ccu [PATH]          # Check Cargo project
ccu -g              # Check global cargo binaries (crates.io, git, local path)
ncu [PATH]          # Check npm/pnpm/yarn/bun project
```

## Options

| Flag | Description |
|------|-------------|
| `-u` | Update dependency files (patch only by default) |
| `-m` | Include minor updates (use with -u) |
| `-f` | Force update to absolute latest |
| `-p` | Include pre-release versions |
| `-g` | Global mode (pcu, ccu) |

Combine flags: `-um` for patch+minor, `-uf` for everything.

## Notes

**pcu -g**: For uv tools, only shows main tool packages, not dependencies within tool environments. Run `uv tool upgrade --all` to upgrade everything including dependencies.

**ccu -g**: Reads `~/.cargo/.crates.toml` to discover installed binaries. For crates.io packages, checks for newer versions. For git installs, queries GitHub to show commits behind. For local path installs, runs `git fetch` and checks for commits behind upstream or dirty working trees.

## Supported Files

**pcu**: `requirements.txt`, `pyproject.toml` (PEP 621/Poetry/PDM), `environment.yml`, lock files

**ccu**: `Cargo.toml`, `Cargo.lock`, workspaces

**ncu**: `package.json`, `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock` (bun.lockb detection only)

## Output

Colors indicate severity: green=patch, yellow=minor, red=major.

Run without `-u` to preview. Run with `-u` to apply. Run your package manager afterward.

## License

MIT
