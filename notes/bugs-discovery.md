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

## DSC-002 - ncu's workspace globs are still not gitignore-aware

Reported by ncu. Narrowed: the `node_modules` hole is closed.

`ncu/src/detector.rs` now filters glob hits through `is_excluded`, which rejects
any path with a component below `project_path` that is in
`{node_modules, bower_components, jspm_packages}` or starts with `.`. It also
uses `is_file()` rather than `exists()` and sorts for deterministic order.

b24f805's mechanism did not transfer literally, which is worth knowing before
the next attempt: ccu walks the tree itself and could swap its recursion for
`ignore::WalkBuilder`, whereas an ncu workspace entry is a user-supplied glob
and the path set comes from `glob::glob`. The exclusion is a filter over glob
hits, not a walk filter.

Residue: a gitignored directory that is neither hidden nor in that list -
`dist/`, `fixtures/` - is still matched. Closing it means adding `ignore` to
`ncu/Cargo.toml` (ccu already declares it, so it is already in `Cargo.lock`)
and rewriting `expand_workspace_pattern` as an `ignore::WalkBuilder` walk
matched against a compiled `glob::Pattern`. That subsumes the hand-rolled list.
Promoting `ignore` to `[workspace.dependencies]` at the same time is worth
considering, since pcu will want it.

## DSC-019 - ncu's workspace globs accept patterns escaping the project root

Lateral finding from the DSC-002 work.

npm rejects a workspace pattern that resolves outside the project root. ncu does
not check, so `"workspaces": ["../../*"]` is happily detected and, under `-u`,
rewrites manifests outside the tree the user pointed at.

`expand_workspace_pattern` also builds its glob from
`self.project_path.join(pattern).to_string_lossy()`, so a *project path*
containing `*`, `?` or `[` is silently reinterpreted as a pattern. That is the
same defect as DSC-003's third bullet, which is filed against ccu only; ccu's
half is fixed (the project-path prefix goes through `glob::Pattern::escape`),
ncu's is not.

## DSC-003 - closed, with one sub-point adjudicated against

Reported by ccu. Two of three fixed in `ccu/src/detector.rs`: the project-path
prefix now goes through `glob::Pattern::escape` so metacharacters in a path the
user did not choose are literal (in `expand_workspace_member` and in
`is_excluded`'s glob branch), and `is_excluded` uses a component-wise
`strip_prefix` so `exclude = ["vendor"]` covers `vendor/foo` without `vendored`
falsely matching.

The gitignore sub-point was **refused, and should not be re-hunted.** b24f805's
rationale is specific to auto-discovery, which *guesses* at membership and must
not adopt unrelated vendored checkouts found by scanning. An explicit `members`
entry is the opposite case: the user declaring what belongs. Cargo itself
expands member globs with no ignore awareness, and a gitignored-but-listed
member is a real member whose dependencies must be reported - filtering it would
make ccu report a strict subset of what `cargo build` builds. The reasoning is a
doc comment on `expand_workspace_member`.

Kept only until the ncu half of the glob-injection bullet lands: see DSC-019.

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

The hunter's position was: write a conda channel client, or drop the conda
claim. A third option shipped instead - keep parsing and listing conda
dependencies, stop resolving them against PyPI, and say plainly why. The
principles page decides it: inventing information is worse than silence, but a
row that cannot be checked must still be shown with an honest reason rather than
dropped.

`pcu/src/main.rs` now partitions conda-channel dependencies out before
`package_names` is built, so nothing conda-owned reaches `PyPiClient`, the
resolver, **or `FileUpdater`** - the entry did not mention the write path, but
`-u` could previously rewrite a conda pin in `environment.yml` from a PyPI
stub's version history. They are listed under "Not checked (N) - conda channel
packages, which pcu cannot resolve" and emitted in `--json` with
`kind: registry_unsupported`.

Note the discriminator is source file *plus* section, not section alone:
`pyproject.rs` also emits `section: Some("dependencies")`, so keying on the
section string would have silently excluded every PEP 621 dependency.

What remains open:

- A conda channel client, honestly scoped: `repodata.json` is one document per
  (channel, subdir), hundreds of MB, with no per-package endpoint, so it needs a
  disk cache keyed on ETag - and this workspace has no HTTP caching layer at
  all. Plus conda's own version semantics (build strings, `build_number` as a
  tiebreaker) as a third comparison dialect, and `python` is itself a conda
  dependency whose selection interacts with the solver. Weeks, not a wave.
- A conda dependency is still a footnote rather than a table row, because
  `DependencyCheck::latest` is a non-optional `Version` and cannot represent
  "found, deliberately not checked". Same root cause as RPT-002.
- "The interpreter is not a registry package" is now enforced in two places by
  two different mechanisms - Poetry's `python` key is skipped in the pyproject
  parser, conda's `python` is excluded in `main.rs`. A third manifest format
  will get it wrong a third time.

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
- npm's space-separated AND (`>=1.2.3 <2.0.0`) and `||` unions land in the same
  `Complex` bucket. An earlier revision of this entry claimed they made
  `VersionSpec::parse` return `Err` and vanish from the report; that was wrong.
  `parse` cannot fail - it ends in `Ok(Complex(..))` - so they are displayed,
  not dropped. See VER-018, which is the general form of that mistake.

Since ncu now uses an allow-list of specifiers it understands, the protocol
forms (`catalog:`, `patch:`, `workspace:`) are deliberately skipped rather than
queried. What is left is the genuinely-npm-range-but-unmodellable case: still
resolved, still displayed with a computed target, still never written.

A real npm range parser is the fix. Until then the display needs to admit which
rows `-u` cannot act on.

## DSC-013 - package-name normalization is not PEP 503, and cannot be fixed alone

Reported by pcu-parsers. Narrowed: every other sub-point is fixed. Includes
(`-r`/`-c`/`--requirement`/`--constraint`) are followed with a canonicalized
visited-set for cycles and a depth bound, each dependency keeping its own
`source_file`; `\` continuations are joined; `--hash` tokens stripped; inline
comments cut only at a whitespace-preceded `#` so `#egg=` and `#sha256=`
survive; bare URLs and `name @ url` are reported as direct references and
skipped rather than becoming package names; marker-differentiated duplicates are
both kept on their own lines.

What remains is the normalization rule, and the reason it is still filed is that
**tightening it in one place is a regression, not a fix.**
`pep508::normalize_name` deliberately implements the current rule (lowercase +
`_`->`-`) rather than PEP 503's `re.sub(r"[-_.]+", "-", name).lower()`, because
`pcu/src/parsers/lockfiles.rs` folds only `_` at three sites. Tightening the
parser alone would turn `zope.interface` into `zope-interface` while the lock
file still produces `zope.interface`, so every dotted distribution would start
reporting as uninstalled.

The rule now lives in exactly one function with that cross-file constraint in
its doc comment. Upgrading it means changing that function and those three
lock-file sites in one commit.

The following sub-points were also refused, with reasons at the sites: `-e
git+...#egg=name` is not resolved against PyPI, because an editable VCS install
is by definition not taken from PyPI (same class of error as DSC-004); it is now
skipped by an explicit arm rather than the blanket `starts_with('-')` that also
ate the include directives.

The precise rule not yet implemented: `.`->`-` and run collapsing, so that
`zope.interface` and `foo--bar` produce keys matching registry and lock-file
keys.

## DSC-015 - binary `bun.lockb` still cannot be read

Reported by ncu. Closed except for the binary format. Text `bun.lock` is parsed
and wired through the detector; `bun.lockb` now warns on stderr naming the
consequence and the remedy instead of returning an empty map silently.

Residue is only the binary format itself, which is a real decoding job and may
never be worth it now that bun emits text lock files on request. The honest
warning may be the permanent answer.

The ncu README's "(bun.lockb detection only)" is now wrong in the other
direction and should be updated to say text `bun.lock` is read.

## DSC-016 - ccu emits workspace-inherited deps once per inheriting member

Reported by ccu.

`CargoTomlParser::parse` reads `[workspace.dependencies]` from the root
manifest, and each member's `.workspace = true` entry also produces a
`Dependency` whose `source_file` points at the root. The display path dedupes
via the `seen` HashSet in `main.rs`, but the JSON `checks` array does not - a
`--json` consumer sees N near-identical entries for every shared dep. The
updater also rewrites the same root line N times (harmless, but N file
parses/writes of redundant work per run).

## DSC-017 - closed, with the recursion sub-point adjudicated against

Reported by pcu-parsers. Three of four fixed in `pcu/src/detector.rs`:
`requirements*.txt` are `is_file`-filtered and sorted before being pushed, so
table order and which-duplicate-wins are deterministic; `environment.y[a]ml`
gained the same `is_file` check; and the failure policy is uniform - an
unreadable pyproject and a failed `read_dir` both warn on stderr and continue,
neither aborting the run.

The recursion sub-point was **refused, and should not be re-hunted.** pcu
resolves installed versions from a single project-root lock file and prints one
set of sync commands per run. A nested `pyproject.toml` is a sibling
distribution with its own lock and its own manager, so discovering it would
merge unrelated dependency sets into one table and resolve them against the
wrong lock. It would also need an exclusion policy (`.venv`, `site-packages`,
`node_modules`, `.git`, build trees) and a depth bound, or a repo with a
vendored virtualenv detects hundreds of files. The reasoning is a block comment
above the detector tests.

Documentation consequence still outstanding: the pcu README claims recursive
discovery. It should be reconciled with top-level-only discovery, and ideally
note the now-deterministic ordering.

## DSC-020 - `parse::<toml::Value>()` does not round-trip a manifest

Lateral finding from the DSC-018 work, recorded because it is an easy trap.

In toml 1.x, `FromStr for Value` is implemented over the *value* deserializer,
not the document parser, so parsing a whole manifest that way gives nonsense -
`[tool.poetry]` comes back as an array and the `tool` key is absent. The correct
call is `toml::from_str::<toml::Table>(&contents)`.

No site in pcu, ccu, ncu or core does this today (grepped), so there is nothing
to fix; the note exists so the next person writing a manifest classifier does
not rediscover it. A comment naming it sits at the `classify_pyproject` call
site.

## Structural recommendation, as filed by the hunters

pcu-parsers argued its half was not independently patchable without repeating
the exercise. Most of that programme has now landed:

- The shared PEP 508 requirement parser exists as `pcu/src/parsers/pep508.rs`
  and all three call sites route through it. The argument that carried it is
  worth preserving: the three splitters disagreed with each other on the *same
  input string*, and each disagreement was one rule guessed rather than stated.
  Stating the rule - a name is `alnum (alnum | - | _ | .)* alnum`, everything
  after it opaque - makes the operator-ordering bug unrepresentable, turns
  `[extras]` into a position rather than a truncation point, and yields markers,
  direct references and multi-clause specifier sets for free.
- The conda MatchSpec parser exists. The channel-aware *resolver* does not, so
  the DSC-004 question (write one, or drop the conda claim) is still open and is
  now the whole of that entry.
- Unconditional pyproject parsing has landed.
- A real PEP 440 type in `core` has not; see the VER document.
- Span-based source locations have not. The fabricated line numbers are gone -
  both the conda `idx + 2` and pyproject's whole-file substring search - but
  what replaced them is still a line number, and "real dependency, location
  unproven" is currently spelled `usize::MAX`. See UPD-008.

ncu argues for a real npm range parser in `ncu` that parses the full grammar
(`||`, space-AND, hyphen ranges, `x` ranges, dist-tags, protocol specifiers) into
a structure, rewrites only the comparator it is allowed to touch, and refuses to
rewrite anything it did not fully understand (DSC-007). That has not landed, and
wave 1 made the *write* end safe without it, so what is left is the reporting
end: rows ncu cannot act on are still displayed as if it could.

A lateral gap found while fixing DSC-010, not filed elsewhere: ncu's yarn header
parser cannot distinguish protocol entries (`pkg@npm:...`, `pkg@workspace:^`,
`pkg@patch:...`) from ranges, because `extract_package_name` splits on the first
`@` regardless. Berry lock files therefore contribute `workspace:` self-entries
as resolution candidates.
