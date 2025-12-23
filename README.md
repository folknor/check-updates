# python-check-updates

A program that tells you which of your Python packages are old.

## WHAT IT DOES

This program scans your Python project for dependency files and queries PyPI to determine if newer versions exist. It prints a table. That is all.

Supported files:

- `requirements.txt` (and variants like `requirements-dev.txt`)
- `pyproject.toml` (PEP 621, Poetry, PDM)
- `environment.yml` (Conda)
- Lock files (`uv.lock`, `poetry.lock`, `pdm.lock`)

## BUILDING

You will need Rust. I do not know why they did not write this in C, but here we are.

```
cargo build --release
```

The binary appears in `target/release/`. Copy it somewhere in your PATH. I suggest `/usr/local/bin` but you probably do not have write permission there anymore.

## USAGE

```
python-check-updates [OPTIONS] [PATH]
```

Run it in your project directory. It will find the files. It will check PyPI. It will print a table.

### OPTIONS

| Flag | What it does |
|------|--------------|
| `-u` | Actually modify the files. Without this flag, the program only looks. |
| `-m` | Update pinned versions to latest minor release only. Conservative. |
| `-f` | Update everything to absolute latest. Reckless. |
| `-p` | Include pre-release versions. You have been warned. |
| `-g` | Check globally installed packages instead of project. See below. |

### GLOBAL MODE

The `-g` flag checks packages installed with:

- `uv tool`
- `pipx`
- `pip install --user`

It cannot modify these. It prints commands you can copy and paste. If a Python version is no longer installed but its packages remain, it tells you to clean up your mess.

## EXAMPLES

Check current directory:
```
python-check-updates
```

Check specific directory:
```
python-check-updates /path/to/project
```

Check and update files:
```
python-check-updates -u
```

Check global packages:
```
python-check-updates -g
```

## OUTPUT

The program prints a table with columns:

- **Package** - The name. Obviously.
- **Defined** - What you wrote in your file.
- **Installed** - What is actually installed (from lock file).
- **In Range** - Latest version satisfying your constraints.
- **Latest** - Absolute latest on PyPI.
- **Update To** - What the spec will become if you use `-u`.

Colors indicate severity: red means major version bump, yellow means minor, green means patch. If your terminal does not support colors, get a better terminal.

### EXAMPLE OUTPUT (PROJECT MODE)

```
$ python-check-updates /home/user/projects/moronic-project
Python 3.13.11 (3.14.2 available)

Package     Defined  Installed  In Range   Latest  Update To
requests    >=2.28.0    2.28.0    2.32.5   2.32.5   >=2.32.5
mcp         >=0.1.0     1.25.0    1.25.0   1.25.0    >=1.25.0
ruff        >=0.8.0    0.14.10   0.14.10  0.14.10   >=0.14.10
pre-commit  >=3.0.0      4.5.1     4.5.1    4.5.1     >=4.5.1
```

The first line tells you your Python version and whether a newer one exists. The table shows the rest.

### EXAMPLE OUTPUT (GLOBAL MODE)

```
$ python-check-updates -g
Python 3.13.11 (3.14.2 available)

uv tools:
  All packages up to date.

pip --user (Python 3.11):
  Package                Installed        Latest
  attrs                     23.1.0        25.4.0
  poetry                     1.6.1         2.2.1
  poetry_core                1.7.0         2.2.1
  virtualenv               20.24.6       20.35.4

pip --user (Python 3.12):
  Package       Installed  Latest
  Brotli            1.1.0   1.2.0
  fonttools        4.51.0  4.61.1

pip --user (Python 3.13):
  All packages up to date.

To upgrade, run:

  $ uv tool upgrade --all
  # Python 3.11 is no longer installed. Consider removing /home/user/.local/lib/python3.11 if nothing uses it.
  # Python 3.12 is no longer installed. Consider removing /home/user/.local/lib/python3.12 if nothing uses it.

Packages not found on PyPI:
  modern-python-nonsense: Package 'modern-python-nonsense' not found on PyPI
```

Sections with no updates say so. Orphaned Python installations are noted. Commands are prefixed with `$` so you can copy them. Comments start with `#` because that is how comments work.

## NOTES

The program assumes semantic versioning. If a package does not follow semver, that is the package author's problem, not mine.

Network requests are made to PyPI. If your network is slow, the program will be slow. This is how networks work.

The program does not install anything. It only modifies text files. You must run your package manager afterward to actually install updates. The program tells you which command to run. Read the output.

## REQUIREMENTS

- Rust toolchain (for building)
- A network connection (for PyPI)
- Python project files (for scanning)
- Common sense (for deciding which updates to apply)

## BUGS

Report them. Or fix them yourself. The source code is right there.

## LICENSE

Do whatever you want with it. I am not your lawyer.
