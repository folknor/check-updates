# Resolution principles

What these tools are for, and the rules that follow from it. This page is
binding: code that contradicts it is wrong, and a finding that proposes
contradicting it should be refused with a pointer here.

## The purpose

`ccu`, `pcu` and `ncu` are **opportunistic**. They exist to report newer
versions that the ecosystem's own updater does not surface.

`cargo update`, `pnpm update`, `npm update`, `uv lock` and their kin all
resolve *within the declared constraint* and, where the registry has one, within
the maintainer's recommended tag. Anything they already tell you is not this
tool's job. The value is entirely in the gap: the release that exists, is
newer, and that your package manager will never mention because your manifest or
the registry's metadata keeps it out of view.

The consequence, stated as a rule:

> **When in doubt, inform.** If a higher version exists, say so. A tool that
> stays quiet about a newer release has failed at the only thing it does.

## Rules that follow

**1. Never cap the reported latest.** `latest` means the highest version the
registry publishes, subject only to the user's explicit prerelease policy. It is
not the maintainer's recommendation, not the highest version inside the
constraint, and not the highest version on the current major series. Those are
all useful numbers and they live in other fields.

The npm case shows why, and the mechanism is worth stating precisely because it
is easy to get backwards. `dist-tags.latest` is set to whatever was published
most recently without an explicit `--tag`; it is not maintained as "the highest
version". A maintainer who backports 1.5.1 after shipping 2.0.0 moves the tag
down to 1.5.1, where it stays.

The two npm tools then disagree with each other and both miss 2.0.0, for
different reasons: `npm update` resolves inside the declared range and stops at
1.5.1, while `npm outdated` reports the tag in its Latest column and also says
1.5.1. Treating the tag as a ceiling here would reproduce that blind spot in the
one tool meant to cover it.

The residual risk is accepted knowingly: a 2.0.0 published on an abandoned line
and never promoted to `latest` is still semver-stable, so it will be reported.
Under rule 2 that is the right trade - the user is told it exists, and
`will_update` keeps `-u` from writing it.

**2. Withholding an update and hiding it are different acts.** It is correct to
refuse to *write* a risky update without `--force`. It is not correct to stop
*mentioning* it. Every safety mechanism belongs on the write path - severity
classification and `will_update` - never on the reporting path.

This is why `in_range` for an unbounded floor (`>=`, `>`) is the newest release
in the dependency's current major series rather than the absolute latest: it
keeps `in_range` distinct from `latest`, so `has_newer_available()` can fire and
the "(x.y.z available)" hint still names the major release. Collapsing the two
fields removes a safe intermediate target *and* silences the hint about the
version it was protecting the user from. See
`DependencyResolver::calculate_in_range`.

**3. A row that cannot be written must still be shown, and must say so.** An
unmodellable constraint, a registry that could not be reached, a dependency
whose declaration could not be located - each is a thing the user should know
about. Dropping it from the table or the JSON is the worst outcome; showing it
with an honest reason is the right one. "We could not check" and "up to date"
must never render the same way.

**4. Silence is a defect, not a default.** A parse failure, a skipped release,
an unreadable lock entry or a subprocess that did not run are all reasons a
newer version might exist and go unreported. They get a diagnostic. The
recurring failure in this codebase has been the quiet `unwrap_or`, `ok()?`,
`_ => Ok(Vec::new())` and `if let Ok(..)` that turn "we do not know" into "there
is nothing".

## What this does not license

Being opportunistic is about *reporting*, not about writing. These tools edit
manifests, and the bar there is the opposite: write only what was asked for,
only where it was read from, and never a value that was not understood. An
update that is reported enthusiastically and written conservatively is the
intended shape.

Nor does it license inventing information. Reporting a version that does not
exist, or comparing against an unrelated package, is worse than silence.

The case that settled this: pcu used to resolve conda-channel dependencies from
`environment.yml` against PyPI, because it had one registry client and used it
for everything. `python`, `mkl` and `cudatoolkit` came back as fetch errors, and
`pytorch` matched an abandoned PyPI stub with no relation to the conda package
of the same name - so the user was offered an update computed from a different
project's version history, and `-u` would have written it. Those dependencies
are now listed as deliberately unchecked, with the reason, which is the correct
shape: say what you do not know rather than guessing at it.
