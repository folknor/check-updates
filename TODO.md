# TODO

## PEP 440 compatible release (~=)

- **4-segment `~=` is not fully supported.** The `Version` struct only stores major.minor.patch, so `~=1.4.5.0` is treated as `~=1.4.5` (same major+minor) when PEP 440 says it should be capped at `1.4.5.*`. This would require adding a 4th version segment to `Version`. Unlikely to matter in practice — 4-segment compatible releases are rare.

## Version ordering

- **Pre-release string comparison is lexicographic, not semantic.** `Version::cmp` compares pre-release strings with plain string ordering (`core/src/version.rs:141`). This means `rc9 > rc10` (wrong), and Python `post` releases are treated as less-than-release when they should be greater (PEP 440).

## Lock file parsing

- **pcu lockfile parser returns single version per package.** `pcu/src/parsers/lockfiles.rs` uses `HashMap<String, Version>` (last write wins). If a Python lockfile has multiple versions of the same package (platform-specific pins in uv.lock or poetry.lock), only the last parsed version survives. ccu was fixed to return `Vec<Version>` per package; pcu should follow the same pattern if this becomes an issue.

## Code quality

- **Duplicated `update_severity()` logic.** The severity calculation exists in `core/src/resolver.rs`, `pcu/src/global.rs`, and `ncu/src/global.rs` as separate methods instead of sharing the core implementation.

- **Unnecessary `.into()` in pcu updater.** `pcu/src/updater.rs:187` calls `.into()` on a `String` to get `Option<String>`, making the `if let Some(...)` always succeed. Dead pattern that's misleading but not a bug.
