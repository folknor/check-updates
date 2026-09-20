## ncu review — findings (read-only, no files changed)

Ordered roughly by severity. Paths absolute; all under /home/folk/Programs/check-updates.

### 1. `ncu -u` writes version strings that are not valid npm syntax
`ncu/src/updater.rs` builds the replacement with `spec.to_string()`, i.e. `impl Display for VersionSpec` in `core/src/version.rs`. That Display is Python/PEP-440 flavoured:
- `Pinned(v)` → `"==4.18.2"`. A bare `"express": "4.18.2"` in package.json parses to `Pinned` (VersionSpec::parse falls through to `Pinned` for a bare version), so exact-pinned deps — extremely common — get rewritten to `"==4.18.2"`, which npm rejects.
- `Wildcard` → `"==1.2.*"`, `Compatible` → `"~=1.2.3"`, `Range` → `">=1.0.0,<2.0.0"` (npm ranges are space-separated, not comma-separated).
Only `Caret`/`Tilde`/`Minimum`/`Maximum`/`GreaterThan`/`LessThan` round-trip. ccu has `to_cargo_string()` for exactly this reason; there is no `to_npm_string()`, and ncu never asks for one. This is the single worst bug in scope.

### 2. `ncu -u` alphabetically reorders the whole package.json
`updater.rs::update_file` does `serde_json::from_str::<Value>` → mutate → `to_string_pretty` → overwrite. `serde_json` is declared as plain `"1"` in the root `Cargo.toml` with no `preserve_order` feature, so `Value::Object` is a `BTreeMap`: every key in the file, at every nesting level, comes back sorted. `"name"`/`"version"`/`"scripts"`/`"dependencies"` get shuffled, dependency order inside each table is re-sorted, indentation is forced to serde's 2-space style, and any original spacing/blank-line layout is gone. The comment directly above the call says "update, and write back preserving formatting", which is false. Compare ccu, which uses `toml_edit` precisely to avoid this. A JSON-text-surgery rewrite (edit the value span in the original string) is the right structure here.

### 3. The updater writes the name into every dependency table, not the one it came from
`updater.rs::update_dependency` loops over `dependencies`/`devDependencies`/`peerDependencies`/`optionalDependencies` and sets the value in *all* of them that contain the key. Combined with finding 4, if a package appears in `dependencies` at `^1.0.0` and in `peerDependencies` at `^1 || ^2`, a `-u` run clobbers the peer range with the dependency's new pin. Peer ranges are deliberately wide; bumping them to the resolved latest is a semantic change nobody asked for. `Dependency` has no field recording which table it came from (`manifest_key` is used for Cargo renames only), so the information is lost at parse time.

### 4. Global dedup by name silently drops workspace members and cross-table duplicates
`ncu/src/main.rs` (project mode) does `all_deps.retain(|d| seen.insert(d.name.clone()))` — first occurrence wins, across all detected package.json files and all four tables. Consequences:
- In a workspace, `lodash@^3` in `packages/a` and `lodash@^4` in `packages/b`: only one is checked, only that file's entry is considered by the updater, and the other member is never reported as outdated at all. The README says "Supports workspaces".
- Two different ranges for one name produce one check, whose result is then written to all tables/files per finding 3.

### 5. `core::Version::from_str` drops the patch component of every named prerelease
`core/src/version.rs::parse_prerelease` scans for `["dev","post","alpha","beta","rc","a","b","c","-"]` in that order and cuts at the first hit. For `1.2.3-beta.1` the `"beta"` match at index 6 leaves `base_part = "1.2.3-"`, so `"3-".parse::<u64>()` fails and `patch` silently becomes **0**. So `1.2.3-beta.1` compares as `1.2.0-beta.1`. Same for `-alpha`, `-rc`, `-dev`, `-post`. Display uses `original`, so the corruption is invisible in output but drives ordering, `max()`, severity and `in_range`. Bites `ncu -p` directly, and equally affects pcu/ccu.

### 6. Prerelease ordering is lexicographic
`Ord for Version` compares `pre_release` as `String`: `1.0.0-beta.10 < 1.0.0-beta.2`, and `-alpha` vs `-rc` only sorts right by accident of the alphabet. With `-p`, `versions.sort()` / `.max()` therefore pick the wrong "latest".

### 7. npm range syntax the parser does not model is silently reinterpreted, then rewritten
`parsers/package_json.rs::parse_npm_version` just calls `VersionSpec::parse`, which knows nothing of npm's grammar. What actually happens:
- `"^1.0.0 || ^2.0.0"` → strip `^` → `Version::from_str("1.0.0 || ^2.0.0")` → `parts = ["1","0","0 || ^2","0","0"]`, patch parse fails → `Caret(1.0.0)`. The union is discarded, and `-u` rewrites the whole string to `"^1.0.5"`, destroying it.
- `">=1.0.0 <2.0.0"` (npm's space-separated AND) → `Minimum(1.0.0)` with patch silently 0; the upper bound vanishes, and rewriting emits `">=1.0.2"`.
- `"1.2.3 - 2.3.4"` (hyphen range) → `parse_prerelease` cuts at `-` → `Pinned` version with `pre_release = Some(" - 2.3.4")`, i.e. treated as a prerelease of 1.2.3.
- `"1.x"` / `"1.2.x"` → no `*`, so not `Wildcard`; `Version::from_str("1.x")` yields `1.0.0` → `Pinned`. Severity is computed against 1.0.0, and a rewrite emits `"==1.4.2"`.
Only `*` and unparseable text (`latest`, `next`) land in `Any`/`Complex` and are correctly left alone. The safe rule is: anything ncu cannot exactly model should become `Complex` (non-rewritable), not a lossy approximation. Today the lossiness is invisible because `Version::Display` echoes `original`.

### 8. `npm:` aliases are queried under the wrong name, violating a documented contract
`core/src/types.rs` states `Dependency::name` is "the upstream package name on the registry ... For renamed/aliased deps this is the real package, not the local key." `parsers/package_json.rs` skips `git`/`file:`/`link:`/`workspace:`/`://`/`github:` but not `npm:`. `"lodash4": "npm:lodash@^4.17.0"` is kept with `name = "lodash4"`, and ncu queries registry.npmjs.org for `lodash4` — either a 404 in the errors list or, worse, a real unrelated package. Also unhandled and sent to the registry: pnpm `catalog:`, yarn berry `patch:`/`portal:`/`exec:`.

### 9. `-p` / `--pre-release` does not change the target version
`ncu/src/npm.rs::get_package` sets `latest` from the `dist-tags.latest` entry regardless of `include_prerelease`; the flag only widens the `versions` vector. Since `force_spec` and the fallback target both come from `latest`, `ncu -p -uf` will never upgrade to a prerelease. The flag only has an effect when a prerelease happens to fall inside the declared range.

### 10. `line_number` in the JSON envelope is frequently wrong
`find_line_number` returns the first line containing `"<name>"` anywhere in the file — it will happily match the `"name"` field, a `scripts` entry, or an `overrides`/`resolutions` block before the actual dependency line, and falls back to 1. It is serialized into the `--json` output as fact. `original_line` derived from it is equally unreliable (unused by the updater, luckily).

### 11. "Installed version" from lock files is whichever copy the parser saw last/first
- `parse_package_lock` (v7+) keys on the path after stripping one `node_modules/` prefix and inserts; hoisted duplicates resolve by map iteration order.
- `parse_yarn_lock` uses `entry().or_insert()` — *first* wins.
- `parse_pnpm_lock` uses `insert` for `packages` (last wins) then `or_insert` for `snapshots`.
Three different tie-break rules for the same question, and none of them resolves "which copy satisfies the root dependency's range" — which is what severity is computed against. package-lock.json's root `""` entry records the declared ranges and would let this be done correctly.

### 12. No gitignore/node_modules guard on workspace glob expansion
`detector.rs::expand_workspace_pattern` globs `<pattern>/package.json` with no filtering. A `"workspaces": ["packages/**"]` entry (legal, and common) will match every `package.json` under `packages/*/node_modules/`, so ncu parses and then registry-queries thousands of transitive packages, and `-u` would rewrite files inside `node_modules`. ccu got exactly this fix in b24f805 ("skip gitignored dirs during workspace auto-discovery"); ncu never did.

### 13. Smaller things
- `updater.rs` writes with a plain truncating `fs::write`; an interrupt mid-write leaves a truncated package.json. Write-temp-then-rename is cheap here.
- `npm.rs` fetches the full registry packument (`GET /{name}` with `Accept: application/json`) — megabytes for popular packages — when the abbreviated doc would do for everything except the `time` map. Nothing caches or conditionally requests (no ETag/If-None-Match).
- `npm.rs::get_packages`: if a spawned task panics the result is recorded under the literal name `"unknown"`, so the affected package just disappears from the report with a meaningless error line.
- Scoped names are interpolated into the URL unencoded (`{registry}/@scope/name`); works against registry.npmjs.org but is not the documented form (`@scope%2Fname`) and will break against stricter mirrors.
- `detect_lockfile` knows `bun.lockb` but not bun's newer text `bun.lock`; `parse_bun_lock` returns an empty map, so with a bun project every dep silently falls back to the spec's base version with no warning that "installed" is a guess. The README's "(bun.lockb detection only)" is honest, but the tool itself says nothing at runtime.
- `output.rs::GlobalTableRenderer::render` has dead `first_group` bookkeeping plus a `let _ = first_group;` to silence the warning — leftover scaffolding for a second source that does not exist.
- `core::resolver::calculate_severity` classifies a same-major, lower-minor target as `Patch` (falls through to the `patch >` arm). Unreachable today because `calculate_target` only returns targets greater than current, but it is a latent mislabel if anything ever proposes a downgrade.
- `cli.rs` help for `--json` says "status messages go to stderr"; the global-mode `--update` note is suppressed under `--json` rather than sent to stderr, which is fine, but note that in `--json` project mode `checks` contains *every* resolved dependency including up-to-date ones, while the human path filters to updates. Not documented either way in the READMEs.

### Structural recommendation
The two top findings share one root cause: ncu has no npm-specific spec type. `core::VersionSpec` is a Python-shaped enum with a Python-shaped `Display`, reused for a grammar (`||`, space-AND, hyphen ranges, `x` ranges, dist-tags, protocol specifiers) it cannot represent. Given the pre-1.0 latitude, I would write a real npm range parser in `ncu` that (a) parses the full grammar into a structure, (b) knows how to rewrite only the comparator it is allowed to touch while leaving the rest of the range text intact, and (c) refuses to rewrite anything it did not fully understand — and pair it with a text-span JSON editor so `package.json` formatting and key order survive `-u`. `core` should keep only the version type and the severity/target logic, and the version type needs findings 5 and 6 fixed regardless.
