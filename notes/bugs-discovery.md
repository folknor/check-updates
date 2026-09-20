# Discovery and parsing defects (DSC)

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.

Findings about what the tools find on disk and what they make of it: detectors,
manifest parsers, lock-file readers.

## DSC-001 - ccu run inside a workspace member reports nothing, silently

Reported by ccu.

The root manifest is only ever looked for at `project_path/Cargo.toml`, and
`Cargo.lock` likewise (`CargoLockParser::find_and_parse`). Neither walks upward.
In this very repo, `ccu/Cargo.toml` has 17 `.workspace = true` entries; `ccu ccu/`
never calls `load_workspace_deps` with the real root, so
`extract_version_or_workspace` returns `None` for all of them and they are
dropped with no message. The user gets "No dependencies found in Cargo.toml" or a
short, misleadingly clean table.

The same path yields zero installed versions, so `current` falls back to the
spec's base version and severities are computed against the declared spec rather
than the lock - contradicting the resolution semantics `CLAUDE.md` states ccu
keeps ("compares the installed version from Cargo.lock"). Fix: discover the
workspace root by walking up for a `Cargo.toml` with a `[workspace]` table, the
way cargo does.

## DSC-002 - ncu's workspace glob expansion has no node_modules or gitignore guard

Reported by ncu.

`detector.rs::expand_workspace_pattern` globs `<pattern>/package.json` with no
filtering. A `"workspaces": ["packages/**"]` entry - legal and common - matches
every `package.json` under `packages/*/node_modules/`, so ncu parses and then
registry-queries thousands of transitive packages, and `-u` would rewrite files
inside `node_modules`. ccu received exactly this fix in b24f805 ("skip gitignored
dirs during workspace auto-discovery"); ncu never did.

## DSC-003 - ccu's two discovery paths have different exclusion semantics

Reported by ccu.

- `is_excluded` compares the member directory against the exclude pattern
  exactly, but cargo excludes whole subtrees: `exclude = ["vendor"]` does not
  exclude `vendor/foo`.
- Gitignore filtering applies only to `auto_discover_members`; explicit glob
  members go through `glob::glob` with no ignore awareness.
- `expand_workspace_member` builds a glob from `project_path.to_string_lossy()`,
  so a project path containing `*`, `?` or `[` is silently reinterpreted as a
  pattern.

## DSC-004 - pcu resolves conda dependencies against PyPI

Reported by pcu-parsers as "the single largest correctness problem in the
scope".

`main.rs` builds one `package_names` list from every parsed dependency and hands
it all to `PyPiClient`. Conda dependencies from `environment.yml` (the non-`pip:`
section) are conda-forge/defaults packages, not PyPI packages. `python=3.9.*`,
`mkl`, `libgcc-ng`, `cudatoolkit`, `pytorch` (PyPI has an abandoned 0.1.2 stub
under that name; conda has 2.x) are either reported as fetch errors or, worse,
compared against a completely unrelated project's version and offered as an
update. Poetry's `python` key is explicitly skipped; conda's `python` is not.

The hunter's position: conda support as written cannot be right without a conda
channel client - either write one, or drop the conda claim.

## DSC-005 - Operator scanning picks the first operator in the list, not the leftmost in the string

Reported by pcu-parsers.

- `requirements.rs::split_package_version` iterates
  `["==", ">=", "<=", "~=", "!=", ">", "<"]` and breaks on the first found
  anywhere. `pkg<3.0,>=2.0` matches `>=` and yields the package name
  `pkg<3.0,`. `pkg>1.0,<=2.0` yields `pkg>1.0,`.
- `pyproject.rs::parse_dependency_string` has the identical bug with
  `[">=", "<=", "==", "!=", "~=", ">", "<", "^", "~"]`: `django<3.0,>=2.0` gives
  the name `django<3.0,`.
- `conda.rs::parse_pip_dependency` is the only one that does it correctly
  (tracks minimum position).

Garbage package names go straight to PyPI. Three hand-rolled splitters, one
correct; this should be one shared PEP 508 tokenizer.

## DSC-006 - pyproject silently discards the version spec of any dependency with extras

Reported by pcu-parsers.

`parse_dependency_string` truncates at `[` *before* looking for operators:

```rust
let dep_str_no_extras = if let Some(idx) = dep_str.find('[') { &dep_str[..idx] } else { dep_str };
```

`"requests[security]>=2.28.0"` becomes name `requests`, spec `Any`. The declared
constraint is gone, the table shows `*`, `is_rewritable()` is false so `-u`
silently skips it. The existing test `test_parse_dependency_with_extras` only
asserts the name, so it passes. `requirements.rs` gets this right (splits
version first, strips extras after) - the two parsers disagree on the same
input string.

## DSC-007 - npm range syntax the parser cannot model is silently reinterpreted, then rewritten

Reported by ncu.

`parsers/package_json.rs::parse_npm_version` just calls `VersionSpec::parse`,
which knows nothing of npm's grammar:

- `"^1.0.0 || ^2.0.0"` - strip `^` - `Version::from_str("1.0.0 || ^2.0.0")` -
  patch parse fails - `Caret(1.0.0)`. The union is discarded and `-u` rewrites
  the whole string to `"^1.0.5"`, destroying it.
- `">=1.0.0 <2.0.0"` (npm's space-separated AND) - `Minimum(1.0.0)` with patch
  silently 0; the upper bound vanishes and a rewrite emits `">=1.0.2"`.
- `"1.2.3 - 2.3.4"` (hyphen range) - `parse_prerelease` cuts at `-`, giving a
  `Pinned` version with `pre_release = Some(" - 2.3.4")`.
- `"1.x"` / `"1.2.x"` - no `*`, so not `Wildcard`; yields `1.0.0` - `Pinned`.
  Severity is computed against 1.0.0 and a rewrite emits `"==1.4.2"` (UPD-002).

Only `*` and unparseable text (`latest`, `next`) land in `Any`/`Complex` and are
correctly left alone. The hunter's rule: anything ncu cannot exactly model
should become non-rewritable, not a lossy approximation. The lossiness is
invisible today because `Version::Display` echoes `original`.

Partly overtaken by events, in a way that changes what is left to do. The
destructive half is closed at the write end: `Version::from_str` is now strict,
so `1.x`, `1.2.x` and `1.2.3 - 2.3.4` land in `Complex` instead of a bogus
`Pinned(1.0.0)`, and `ncu/src/updater.rs` refuses to write any spec it cannot
render as valid npm syntax. A union like `^17 || ^18` is no longer corrupted.

What remains is the *reporting* half, and it is now the whole of this entry,
in two distinct shapes:

- A spec that parses to `Complex` (`1.x`, a hyphen range) is still resolved,
  still displayed as checkable with a computed target, and then silently not
  written. The user is shown an update that `-u` will never apply.
- A spec that makes `VersionSpec::parse` return `Err` - npm's space-separated
  AND (`>=1.2.3 <2.0.0`) and `||` unions - is worse: `parse_deps` does
  `if let Ok(..)`, so the dependency vanishes from the report entirely. Before
  the strict-parsing change, `>=1.2.3 <2.0.0` at least appeared, as a garbage
  `Minimum(1.2.0)`.

A real npm range parser is still the fix. Routing the `Err` path to `Complex`
would at least convert a disappearance into a visible un-actionable row, but
only the display change makes either honest.

## DSC-008 - ncu dedups by name globally, dropping workspace members and cross-table duplicates

Reported by ncu.

`main.rs` does `all_deps.retain(|d| seen.insert(d.name.clone()))` - first
occurrence wins, across all detected `package.json` files and all four tables.

- In a workspace, `lodash@^3` in `packages/a` and `lodash@^4` in `packages/b`:
  only one is checked, only that file's entry is considered by the updater, and
  the other member is never reported as outdated. The README says "Supports
  workspaces".
- Two different ranges for one name produce one check, whose result is then
  written to all tables and files (UPD-004).

pcu has the same defect shape across optional-dependency groups: see DSC-012.

## DSC-009 - ncu queries `npm:` aliases under the wrong name, violating a documented contract

Reported by ncu.

`core/src/types.rs` states `Dependency::name` is "the upstream package name on
the registry ... For renamed/aliased deps this is the real package, not the
local key." `parsers/package_json.rs` skips `git`/`file:`/`link:`/`workspace:`/
`://`/`github:` but not `npm:`. `"lodash4": "npm:lodash@^4.17.0"` is kept with
`name = "lodash4"` and queried as such - either a 404 in the errors list or,
worse, a real unrelated package. Also unhandled and sent to the registry: pnpm
`catalog:`, yarn berry `patch:`/`portal:`/`exec:`.

## DSC-010 - ncu's three lock-file parsers use three different tie-break rules

Reported by ncu.

- `parse_package_lock` (v7+) keys on the path after stripping one
  `node_modules/` prefix and inserts; hoisted duplicates resolve by map
  iteration order.
- `parse_yarn_lock` uses `entry().or_insert()` - *first* wins.
- `parse_pnpm_lock` uses `insert` for `packages` (last wins) then `or_insert`
  for `snapshots`.

None of them resolves "which copy satisfies the root dependency's range", which
is what severity is computed against. `package-lock.json`'s root `""` entry
records the declared ranges and would allow doing this correctly.

## DSC-011 - Conda `==` pinning degrades to "no constraint", plus the rest of MatchSpec

Reported by pcu-parsers.

`parse_conda_dependency` checks `>=`, `<=`, `!=`, `>`, `<`, then plain `=`. For
the legal conda form `numpy==1.24.0`, `find('=')` hits the first `=`, so
`version_str = "=1.24.0"` and it builds `"===1.24.0"`; `VersionSpec::parse`
strips `==`, `Version::from_str("=1.24.0")` fails, and the `Err(_)` arm returns
**`VersionSpec::Any`**. A hard pin is reported as unconstrained.

In the same function:

- Every failure path collapses to `Any` (claims the file said nothing), whereas
  `requirements.rs` falls back to `Complex(raw)` (preserves the text). Opposite
  lies about the same failure.
- Conda `=` is a *prefix* match (`numpy=1.24` means 1.24.*) but is mapped to
  `Pinned`, which is rewritable - so `-u` rewrites prefix pins as exact ones.
- Build strings (`numpy=1.24.0=py39h1234`) are not modelled: the patch fails to
  parse, silently becomes 0, and the build string stays in `Version.original`,
  which is what `Display` prints.
- Channel-qualified specs (`conda-forge::numpy=1.24`) keep the channel in the
  package name. Space-separated MatchSpec (`numpy 1.24.0 py39_0`) has no
  operator, so the whole string becomes the package name.
- Conda names are lowercased but not `_`->`-` normalized, unlike everywhere
  else, so `typing_extensions` in a conda pip section never matches the
  `typing-extensions` key produced elsewhere.

## DSC-012 - pyproject: unparsed dependencies vanish, and whole sections are unread

Reported by pcu-parsers.

- `parse_poetry_dependency` and `parse_dependency_string` both end
  `VersionSpec::parse(...).ok()?`, so a spec the parser does not model (PEP 440
  epoch `1!2.0`, multi-clause, `===`) makes the dependency disappear from the
  report with no warning. Poetry multi-constraint arrays
  (`pkg = [{version=...},{version=...}]`) and git/path tables also return `None`
  silently. `requirements.rs` at least keeps them as `Complex`.
- No handling of `[tool.uv]` at all: `[tool.uv.dev-dependencies]` (array, widely
  used pre-PEP-735) and `[tool.uv.sources]` are ignored, so a uv project's dev
  deps are silently missing while the tool prints "uv" as the detected manager.
  Also unread: `[build-system].requires`, `[tool.setuptools.dynamic]`, and PEP
  508 direct references (`name @ git+https://...` becomes a package literally
  named that).
- Dedup by name across all sections (`retain(|d| seen.insert(name))`) discards
  the distinct constraints in different optional-dependency groups; the
  surviving entry's line number is the first occurrence, so `-u` updates one of
  N occurrences and leaves the others stale.

## DSC-013 - requirements.txt: whole categories of line dropped without a word

Reported by pcu-parsers.

- `-r`/`-c` includes are not followed, so `requirements/base.txt` layouts yield
  nothing; the detector does not recurse to find them either (DSC-018).
- `-e .`, `-e git+...` and any `--hash`/`--find-links` continuation are dropped
  by the blanket `starts_with('-')`.
- Line continuations (`\`) are not joined; the trailing backslash lands inside
  the version string and the spec degrades to `Complex`.
- Bare URL requirements and `name @ url` are turned into package "names".
- Inline-comment stripping cuts at the first `#` anywhere (PEP 508 requires
  ` #`), so `#egg=` and `#sha256=` fragments truncate the line.
- Environment markers are discarded, so `pkg==1.0; python_version<'3.8'` and
  `pkg==2.0; python_version>='3.8'` produce two same-named entries with
  different pins and nothing reconciles them.
- Name normalization does lowercase + `_`->`-` but not `.`->`-` and no run
  collapsing, so it is not PEP 503: `zope.interface` and `foo--bar` produce keys
  that will not match registry or lock-file keys.

## DSC-014 - pcu's `can_parse` claims lock formats `parse` does not handle

Reported by pcu-parsers.

`can_parse` returns true for `Pipfile.lock` and `conda-lock.yml`, but `parse`
has no arm for either and `bail!`s "Unsupported lock file" - any caller trusting
`can_parse` gets a hard error. `find_and_parse` is narrower still: it probes only
`uv.lock`, `poetry.lock`, `pdm.lock`, with no `Pipfile.lock`, no
`requirements.lock`, and no search outside the top directory. Most pip projects
therefore have *no* installed versions and every row is compared against the
declared spec instead.

Also: `insert` on duplicate package names (common in `poetry.lock`/`uv.lock` for
platform- or marker-split resolutions) keeps whichever came last rather than the
applicable or maximum one. Unparsable versions are `eprintln!`'d and dropped, so
the dependency looks uninstalled. `PdmLockFile`/`PdmPackage` are byte-for-byte
duplicates of `TomlLockFile`/`TomlPackage`, and the three parse functions are
the same function three times.

## DSC-015 - ncu does not support bun's text lock file, and says nothing

Reported by ncu.

`detect_lockfile` knows `bun.lockb` but not bun's newer text `bun.lock`;
`parse_bun_lock` returns an empty map, so with a bun project every dep silently
falls back to the spec's base version with no warning that "installed" is a
guess. The README's "(bun.lockb detection only)" is honest, but the tool itself
says nothing at runtime.

## DSC-016 - ccu emits workspace-inherited deps once per inheriting member

Reported by ccu.

`CargoTomlParser::parse` reads `[workspace.dependencies]` from the root
manifest, and each member's `.workspace = true` entry also produces a
`Dependency` whose `source_file` points at the root. The display path dedupes
via the `seen` HashSet in `main.rs`, but the JSON `checks` array does not - a
`--json` consumer sees N near-identical entries for every shared dep. The
updater also rewrites the same root line N times (harmless, but N file
parses/writes of redundant work per run).

## DSC-017 - pcu's detector is top-directory only and order-dependent

Reported by pcu-parsers.

- No recursion: `requirements/*.txt`, `src/<pkg>/pyproject.toml` and monorepo
  members are invisible. ccu recurses workspaces; pcu does not, and the README's
  claim does not qualify this.
- `fs::read_dir` order is unspecified, so the order of detected
  `requirements*.txt` files - and therefore the output table order and which
  duplicate wins downstream - varies run to run.
- No `is_file` check, so a *directory* named `requirements-x.txt` is "detected"
  and then fails to read.
- The `?` on `fs::read_to_string(pyproject_path)` aborts the whole run on one
  unreadable file, while the requirements scan swallows `read_dir` errors.
  Inconsistent failure policy.

## DSC-018 - pcu's package-manager sniffing gates parsing, and gets it wrong both ways

Reported by pcu-parsers. See UPD-013 for the same detection being wrong in the
updater.

`detect_pyproject_manager` does raw substring matching on file text:
`[tool.poetry]` inside a comment or string counts; a valid Poetry file with only
`[tool.poetry.dependencies]` and no bare `[tool.poetry]` header is not matched;
`[tool.pdm.dev-dependencies]` does not contain the literal `[tool.pdm]`, so a
dev-deps-only PDM project is not PDM.

When it falls through to `Ok(None)` the **pyproject.toml is dropped from
`detected_files` entirely** and none of its dependencies are ever parsed - e.g.
a file with only `[dependency-groups]` and no `[project]`, which
`parse_dependency_groups` would otherwise handle fine. Unknown-but-has-`[project]`
defaults to `Uv`, so a plain pip/setuptools project is told to run `uv lock`.

The hunter's recommendation: parse `pyproject.toml` unconditionally when it
exists, with manager detection as a label rather than a gate.

## Structural recommendation, as filed by the hunters

pcu-parsers argues DSC-004 through DSC-014 are not independently patchable
without repeating the exercise, and proposes: one real PEP 508 requirement
parser shared by requirements.txt, pyproject arrays and the conda `pip:` section,
returning name + extras + full multi-clause specifier set + marker as structured
data; a real PEP 440 type in `core` (see the VER document); a separate conda
MatchSpec parser with a channel-aware resolver, or dropping the conda claim
until one exists; span-based source locations instead of fabricated line numbers
(UPD-008); and unconditional pyproject parsing.

ncu argues for a real npm range parser in `ncu` that parses the full grammar
(`||`, space-AND, hyphen ranges, `x` ranges, dist-tags, protocol specifiers) into
a structure, rewrites only the comparator it is allowed to touch, and refuses to
rewrite anything it did not fully understand (DSC-007, UPD-002).
