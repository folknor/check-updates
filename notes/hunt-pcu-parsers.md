## pcu input side — findings (read-only; nothing edited)

Files read: `/home/folk/Programs/check-updates/pcu/src/detector.rs`, `pcu/src/parsers/{requirements,pyproject,conda,lockfiles,mod}.rs`, `pcu/tests/integration.rs`, plus `core/src/version.rs`, `pcu/src/updater.rs` (line-rewrite path) and `pcu/src/main.rs` (wiring) where questions crossed over.

### A. Wrong registry: conda deps are resolved against PyPI
`main.rs` builds one `package_names` list from every parsed dependency and hands it all to `PyPiClient`. Conda dependencies from `environment.yml` (the non-`pip:` section) are conda-forge/defaults packages, not PyPI packages. `python=3.9.*`, `mkl`, `libgcc-ng`, `cudatoolkit`, `pytorch` (PyPI has an abandoned 0.1.2 stub under that name; conda has 2.x) are either reported as fetch errors or, worse, compared against a completely unrelated project's version and offered as an update. Poetry's `python` key is explicitly skipped; conda's `python` is not. This is the single largest correctness problem in the scope — conda support as written cannot be right without a conda channel client.

### B. Operator scanning picks the first operator *in the list*, not the leftmost in the string
- `requirements.rs::split_package_version` iterates `["==", ">=", "<=", "~=", "!=", ">", "<"]` and `break`s on the first one found anywhere. For `pkg<3.0,>=2.0` it matches `>=` (later in the string) and yields package name `pkg<3.0,`. For `pkg>1.0,<=2.0` it matches `<=` and yields `pkg>1.0,`. Garbage package names go straight to PyPI.
- `pyproject.rs::parse_dependency_string` has the identical bug with `[">=", "<=", "==", "!=", "~=", ">", "<", "^", "~"]`: `"django<3.0,>=2.0"` → name `django<3.0,`.
- `conda.rs::parse_pip_dependency` is the only one that does it correctly (tracks min position). Three hand-rolled splitters, one correct — this should be one shared PEP 508 tokenizer.

### C. pyproject silently discards the version spec of any dependency with extras
`parse_dependency_string` truncates at `[` *before* looking for operators:
```rust
let dep_str_no_extras = if let Some(idx) = dep_str.find('[') { &dep_str[..idx] } else { dep_str };
```
`"requests[security]>=2.28.0"` becomes name `requests`, `VersionSpec::Any`. The declared constraint is gone; the table shows `*`, `is_rewritable()` is false so `-u` silently skips it. The existing test `test_parse_dependency_with_extras` only asserts the name, so it passes. `requirements.rs` gets this right (splits version first, strips extras after) — the two parsers disagree on the same input string.

### D. Conda `==` pinning degrades to "no constraint"
`parse_conda_dependency` checks `>=`, `<=`, `!=`, `>`, `<`, then plain `=`. For the perfectly legal conda form `numpy==1.24.0`, `find('=')` hits the first `=`, so `version_str = "=1.24.0"`, and it builds `"===1.24.0"`. `VersionSpec::parse` strips `==`, `Version::from_str("=1.24.0")` fails, and the `Err(_)` arm returns **`VersionSpec::Any`**. A hard pin is reported as unconstrained.
Related in the same function:
- Every failure path collapses to `Any` (claims the file said nothing) whereas `requirements.rs` falls back to `Complex(raw)` (preserves the text). Opposite lies about the same failure.
- Conda `=` is a *prefix* match (`numpy=1.24` means 1.24.*), but it is mapped to `Pinned`. `Pinned` is rewritable, so `-u` will rewrite prefix pins as exact ones.
- Build strings (`numpy=1.24.0=py39h1234`) are not modelled: `Version::from_str` fails to parse the patch, silently uses `patch = 0`, and keeps the build string in `Version.original`, which is what `Display` prints.
- Channel-qualified specs (`conda-forge::numpy=1.24`) keep the channel in the package name. Space-separated MatchSpec (`numpy 1.24.0 py39_0`) has no operator, so the whole string becomes the package name.
- Conda names are only lowercased, not `_`→`-` normalized, unlike requirements/pyproject/lockfiles — so `typing_extensions` in a conda pip section will never match the `typing-extensions` key produced everywhere else.

### E. Fabricated and fuzzy line numbers feed a line-index rewriter
`updater.rs::update_file` indexes `lines[line_number - 1]` and rewrites in place.
- `conda.rs` invents `line_number = idx + 2` from the array index, ignoring `name:`/`channels:` blocks entirely. In its own test fixture the first dep is at file line 6 and gets 2. Any `-u` on an `environment.yml` rewrites arbitrary lines (guarded only by `line.replace(old_spec, new_spec)` no-op'ing when nothing matches).
- `pyproject.rs::find_line_in_content` is a case-insensitive substring search for the package name over the whole file — it matches comments, the `name = "..."` key, and any longer package containing the shorter one (`requests` matches `requests-oauthlib`, `pytest` matches `pytest-cov`). First match wins, so the reported `line_number`/`original_line` can point at a different dependency. Fallback when nothing matches is line 1 with a synthesized `pkg = "spec"` string that never existed in the file.
- Unrelated but in the same path: `update_file` does `lines.join("\n")`, so a CRLF file is silently rewritten to LF.

### F. pyproject: parse failure drops the dependency entirely
`parse_poetry_dependency` and `parse_dependency_string` both end `VersionSpec::parse(...).ok()?` — a spec the parser doesn't model (PEP 440 epoch `1!2.0`, multi-clause, `===`) makes the dependency vanish from the report with no warning. Poetry multi-constraint arrays (`pkg = [{version=...},{version=...}]`) and git/path tables also return `None` silently. `requirements.rs` at least keeps them as `Complex`.

### G. pyproject: sections the scope claims are covered, aren't
No handling of `[tool.uv]` at all — `[tool.uv.dev-dependencies]` (array, widely used pre-PEP-735) and `[tool.uv.sources]` are ignored, so a uv project's dev deps are silently missing while the tool prints "uv" as the detected manager. Also unread: `[build-system].requires`, `[tool.setuptools.dynamic]`, PEP 508 direct references (`name @ git+https://…` becomes a package literally named `name @ git+https://…`).

Dedup by name across all sections (`retain(|d| seen.insert(name))`) discards the distinct constraints in different optional-dependency groups; the surviving entry's line number is the first occurrence, so `-u` updates one of N occurrences and leaves the others stale.

### H. requirements.txt: whole categories of line dropped without a word
- `-r`/`-c` includes are not followed (contradicts the scope's expectation; `requirements/base.txt` style layouts yield nothing). Also not detected by `detector.rs` since it doesn't recurse.
- `-e .`, `-e git+…` and any `--hash`/`--find-links` continuation: dropped by the blanket `starts_with('-')`.
- Line continuations (`\`) are not joined; the trailing backslash lands inside the version string and the spec degrades to `Complex`.
- Bare URL requirements and `name @ url` are turned into package "names".
- Inline-comment stripping cuts at the first `#` anywhere (PEP 508 requires ` #`), so `#egg=` and `#sha256=` fragments truncate the line.
- Environment markers are discarded, so `pkg==1.0; python_version<'3.8'` and `pkg==2.0; python_version>='3.8'` produce two entries with the same name and different pins; nothing reconciles them.
- Name normalization does `lowercase` + `_`→`-` but not `.`→`-` and no run-collapsing, so it is not PEP 503 normalization: `zope.interface` and `foo--bar` produce keys that won't match the registry/lockfile keys.

### I. lockfiles.rs: `can_parse` is a lie, `find_and_parse` is narrower still
`can_parse` returns true for `Pipfile.lock` and `conda-lock.yml`, but `parse` has no arm for either and `bail!`s "Unsupported lock file". Any caller that trusts `can_parse` gets a hard error. `find_and_parse` only probes `uv.lock`, `poetry.lock`, `pdm.lock` — no `Pipfile.lock`, no `requirements.lock`, and no search outside the top directory, so most pip projects have *no* installed versions at all and every row is compared against the declared spec instead.
Also: `insert` on duplicate package names (common in `poetry.lock`/`uv.lock` for platform- or marker-split resolutions) silently keeps whichever came last rather than the applicable or maximum one. Unparsable versions are `eprintln!`'d and dropped from the map — the dependency then looks uninstalled. `PdmLockFile`/`PdmPackage` are byte-for-byte duplicates of `TomlLockFile`/`TomlPackage`; the three parse functions are the same function three times.

### J. detector.rs
- Top-directory only: no recursion. `requirements/*.txt`, `src/<pkg>/pyproject.toml`, monorepo members are invisible. `ccu` recurses workspaces; `pcu` does not, and the README's "requirements.txt, pyproject.toml …" claim doesn't qualify this.
- `fs::read_dir` order is unspecified, so the order of detected `requirements*.txt` files — and therefore the order of the output table and which duplicate wins downstream — varies run to run.
- No `is_file` check, so a *directory* named `requirements-x.txt` is "detected" and then fails to read.
- `detect_pyproject_manager` does raw substring matching on file text: `[tool.poetry]` inside a comment or string counts; conversely a valid Poetry file that only has `[tool.poetry.dependencies]` (no bare `[tool.poetry]` header) is not matched, and `[tool.pdm.dev-dependencies]` does not contain the literal `[tool.pdm]` so a dev-deps-only PDM project isn't PDM either. When it falls through to `Ok(None)` the **pyproject.toml is dropped from `detected_files` entirely** and none of its dependencies are ever parsed — e.g. a file with only `[dependency-groups]` and no `[project]`, which `parse_dependency_groups` would otherwise handle fine.
- Unknown-but-has-`[project]` defaults to `Uv`, so a plain pip/setuptools project is told to run `uv lock`.
- The `?` on `fs::read_to_string(pyproject_path)` aborts the whole run on one unreadable file, while the requirements scan swallows `read_dir` errors. Inconsistent failure policy.

### K. `core::version` — the shared foundation these all sit on (out of scope, load-bearing)
- `parse_prerelease` scans for `["dev","post","alpha","beta","rc","a","b","c","-"]` and takes the first *pattern in the list* found anywhere with `idx > 0`. `1.2.3-beta` matches `beta` at index 6, leaving base `"1.2.3-"`; `"3-".parse()` fails and `patch` silently becomes **0**. So `1.2.3-beta == 1.2.0-beta`. Any version with an unparsable minor/patch segment silently zeroes it rather than erroring.
- `post` releases are classified as pre-releases, so `1.2.3.post1 < 1.2.3` — inverted against PEP 440.
- Pre-release ordering is a plain string compare: `rc10 < rc2`, and `dev` sorts above `beta`/`alpha` instead of below.
- PEP 440 epochs (`1!2.0`) fail to parse outright.
- `s.to_lowercase().find(pattern)` then slices the *original* `s` at that byte index — a non-ASCII char whose lowercase differs in byte length gives a wrong slice or a panic on a char boundary.
- `PartialEq`/`Ord` ignore `local`, so `1.0.0+cu118` and `1.0.0+cpu` are equal.
- `VersionSpec::parse` only recognises a two-clause `>=X,<Y` range; `>1.0,<2.0` or three clauses become `Complex`, and `Complex::satisfies` returns `false` (claims out of range).

### Recommendation
B, C, D, E and I are not independently patchable without repeating this next month. The right move is a rewrite of pcu's input layer around (1) one real PEP 508 requirement parser shared by requirements.txt, pyproject arrays and the conda `pip:` section, returning name + extras + full multi-clause specifier set + marker as structured data; (2) a real PEP 440 version/specifier type in `core` (epoch, release tuple of arbitrary length, pre/post/dev, local) replacing the current major/minor/patch-plus-guesswork; (3) a separate conda MatchSpec parser with its own channel-aware resolver, or dropping the conda claim until one exists; (4) span-based source locations (`toml_edit` / `serde_yaml` spans / real line indices) instead of the fabricated and fuzzy-searched line numbers the updater currently trusts; (5) parsing `pyproject.toml` unconditionally when it exists, with manager detection being a label rather than a gate.
