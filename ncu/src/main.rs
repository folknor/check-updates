use anyhow::{Context, Result};
use check_updates_core::{DependencyCheck, DependencyResolver, Version};
use clap::Parser;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};

use ncu::cli::Args;
use ncu::detector::ProjectDetector;
use ncu::global::{GlobalCheck, GlobalPackageDiscovery, generate_upgrade_commands};
use ncu::npm::{FetchError, NpmClient};
use ncu::output::{GlobalTableRenderer, TableRenderer};
use ncu::parsers::{LockfileParser, PackageJsonParser};
use ncu::updater::FileUpdater;

const SCHEMA_VERSION: u32 = 1;
const TOOL_NAME: &str = "ncu";

/// One JSON object per failed registry lookup. `kind` is the stable tag from
/// `FetchErrorKind::as_str`, so a consumer can tell `not_found` from
/// `rate_limited` without parsing `message`. `name` is kept for existing
/// consumers; `package` is the field the other two tools use.
fn errors_to_json(errors: &[FetchError]) -> Vec<serde_json::Value> {
    errors
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.package,
                "package": f.package,
                "kind": f.kind.as_str(),
                "message": f.detail,
            })
        })
        .collect()
}

/// Print registry failures under a header that matches what actually happened.
///
/// Only a 404 means "not on npm". A rate limit, a timeout or an outage says
/// nothing about whether the package exists, and printing those under a
/// "not found" header told the user their dependencies do not exist when the
/// network was down.
fn print_fetch_failures(failures: &[FetchError]) {
    let (missing, unchecked): (Vec<&FetchError>, Vec<&FetchError>) =
        failures.iter().partition(|f| f.kind.is_missing());

    if !missing.is_empty() {
        println!();
        println!("{}", "Packages not found on npm:".dimmed());
        for failure in missing {
            println!("  {}", failure.detail.dimmed());
        }
    }
    if !unchecked.is_empty() {
        println!();
        println!("{}", "Packages that could not be checked:".dimmed());
        for failure in unchecked {
            println!("  {}", failure.detail.dimmed());
        }
    }
}

/// Under `--update --force` the writer uses `force_spec`, which `resolve`
/// computes from `latest` - not from `target`, which is capped by the declared
/// range. The table printed `target` regardless, so `-uf` reported a smaller
/// bump than it went on to write, and derived the severity column from that
/// smaller bump too.
///
/// `-f` is documented as "force update all to absolute latest", so the write is
/// right and the row was wrong. This retargets the row onto `latest` and
/// recomputes the severity from the same pair, for display only: `force_spec`
/// is untouched, so `will_update` and `update_blocker` answer exactly as before
/// and `apply_updates` still sees the original checks.
fn retarget_forced(check: &DependencyCheck) -> DependencyCheck {
    // Nothing to retarget onto: a failed lookup has no latest version, and
    // `latest` holds a placeholder that must never become a displayed target.
    if check.check_failed {
        return check.clone();
    }

    let mut forced = check.clone();
    forced.severity =
        DependencyResolver::calculate_severity(check.current_version(), Some(&check.latest));
    forced.target = Some(check.latest.clone());
    forced.target_spec = check.force_spec.clone();
    forced.target_released_at = check.latest_released_at.clone();
    forced
}

fn emit_json_project(checks: &[DependencyCheck], errors: &[FetchError]) -> Result<()> {
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "tool": TOOL_NAME,
        "mode": "project",
        "checks": checks,
        "errors": errors_to_json(errors),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn emit_json_global(checks: &[GlobalCheck], errors: &[FetchError]) -> Result<()> {
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "tool": TOOL_NAME,
        "mode": "global",
        "checks": checks,
        "errors": errors_to_json(errors),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.global {
        run_global_mode(&args).await
    } else {
        run_project_mode(&args).await
    }
}

async fn run_global_mode(args: &Args) -> Result<()> {
    if args.update && !args.json {
        println!(
            "Note: --update flag is ignored in global mode. Commands will be shown instead.\n"
        );
    }

    // 1. Discover global packages
    let discovery = GlobalPackageDiscovery::new();
    let packages = discovery.discover();

    if packages.is_empty() {
        if args.json {
            emit_json_global(&[], &[])?;
        } else {
            println!("No globally installed npm packages found.");
        }
        return Ok(());
    }

    // 2. Query npm registry for latest versions
    let client = NpmClient::new(args.pre_release);
    let package_names: Vec<String> = packages
        .iter()
        .map(|p| p.name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let progress = ProgressBar::new(package_names.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .expect("valid progress template")
            .progress_chars("=>-"),
    );

    let pb = progress.clone();
    let results = client
        .get_packages(&package_names, move |done, _total| {
            pb.set_position(done as u64);
        })
        .await;
    progress.finish_and_clear();

    let mut package_infos: HashMap<String, _> = HashMap::new();
    let mut errors: Vec<FetchError> = Vec::new();

    for (name, result) in results {
        match result {
            Ok(info) => {
                package_infos.insert(name, info);
            }
            Err(e) => errors.push(e),
        }
    }

    // 3. Build check results
    let mut checks: Vec<GlobalCheck> = Vec::new();

    for package in packages {
        if let Some(info) = package_infos.get(&package.name) {
            // `-m` in global mode retargets the row to the newest release
            // sharing the installed major. `-f` is documented as "force update
            // all to absolute latest", so it takes precedence - previously
            // `-m` was tested first and `-g -mf` silently ignored `-f`.
            let target = if args.minor && !args.force {
                info.versions
                    .iter()
                    .filter(|v| v.major == package.installed_version.major)
                    .max()
                    .cloned()
                    .unwrap_or_else(|| package.installed_version.clone())
            } else {
                info.latest.clone()
            };
            let has_update = target > package.installed_version;

            checks.push(GlobalCheck {
                package,
                latest: target,
                has_update,
            });
        }
    }

    // 4. Display results
    if args.json {
        emit_json_global(&checks, &errors)?;
        return Ok(());
    }

    let renderer = GlobalTableRenderer::new(true);
    renderer.render(&checks);

    // 5. Print upgrade commands
    let commands = generate_upgrade_commands(&checks);
    if !commands.is_empty() {
        println!();
        println!("To upgrade, run:\n");
        for cmd in &commands {
            println!("  $ {cmd}");
        }
    }

    // 6. Print errors
    print_fetch_failures(&errors);

    Ok(())
}

async fn run_project_mode(args: &Args) -> Result<()> {
    let project_path = args.project_path();

    if !project_path.exists() {
        anyhow::bail!("Project path does not exist: {project_path:?}");
    }

    // Detect package.json files
    let detector = ProjectDetector::new(project_path.clone());
    let detected_files = detector.detect()?;

    if detected_files.is_empty() {
        if args.json {
            emit_json_project(&[], &[])?;
        } else {
            println!("No package.json files found in {project_path:?}");
        }
        return Ok(());
    }

    // Parse lock file for installed versions
    let installed_versions: HashMap<String, Version> =
        if let Some(lockfile_type) = detector.detect_lockfile() {
            let lockfile_path = detector.lockfile_path(lockfile_type);
            LockfileParser::new()
                .parse(&lockfile_path, lockfile_type)
                .unwrap_or_default()
        } else {
            HashMap::new()
        };

    // Parse all package.json files
    let parser = PackageJsonParser::new();
    let mut all_deps = Vec::new();

    for file in &detected_files {
        let deps = parser
            .parse(&file.path)
            .with_context(|| format!("Failed to parse {}", file.path.display()))?;
        all_deps.extend(deps);
    }

    if all_deps.is_empty() {
        if args.json {
            emit_json_project(&[], &[])?;
        } else {
            println!("No dependencies found");
        }
        return Ok(());
    }

    // Deduplicate by declaration site, not by package name. The same package may be
    // declared in several workspace members, or in both `dependencies` and
    // `devDependencies` of one manifest, each with its own range. Collapsing those by
    // name reported only the first and silently hid the rest.
    let mut seen = HashSet::new();
    all_deps.retain(|d| seen.insert((d.source_file.clone(), d.section.clone(), d.name.clone())));

    // Query npm registry. The registry answer is per package, so ask once per distinct
    // name even though several declarations may share it.
    let client = NpmClient::new(args.pre_release);
    let mut package_names: Vec<String> = all_deps.iter().map(|d| d.name.clone()).collect();
    package_names.sort();
    package_names.dedup();

    let progress = ProgressBar::new(package_names.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .expect("valid progress template")
            .progress_chars("=>-"),
    );

    let pb = progress.clone();
    let results = client
        .get_packages(&package_names, move |done, _total| {
            pb.set_position(done as u64);
        })
        .await;
    progress.finish_and_clear();

    // Build package info map
    let mut package_infos: HashMap<String, _> = HashMap::new();
    let mut errors: Vec<FetchError> = Vec::new();

    for (name, result) in results {
        match result {
            Ok(info) => {
                package_infos.insert(name, info);
            }
            Err(e) => errors.push(e),
        }
    }

    // Resolve dependencies
    let resolver = DependencyResolver::new();
    let mut checks = Vec::new();

    for dep in &all_deps {
        let installed = installed_versions.get(&dep.name);
        // A package the registry never answered for keeps a check of its own
        // rather than being dropped: the run was asked about it, and silence
        // about it reads as "up to date" in both the table and the JSON.
        let check = match package_infos.get(&dep.name) {
            Some(info) => resolver.resolve(dep, info, installed),
            None => DependencyCheck::unchecked(dep, installed),
        };
        checks.push(check);
    }

    if args.json {
        // Apply updates as a side effect if requested, then emit JSON (no human text).
        if args.update {
            let updater = FileUpdater::new();
            let _ = updater.apply_updates(&checks, args.minor, args.force)?;
        }
        emit_json_project(&checks, &errors)?;
        return Ok(());
    }

    // In update mode only list what the severity filter will actually write,
    // so -u/-um never claim to have applied a major bump they skipped.
    //
    // Dependencies are tracked per declaration site, so the same package declared in two
    // workspace members produces two checks. Collapse rows that agree on name and target
    // (matching ccu), so only genuinely different ranges show up as separate rows.
    //
    // Under `-uf` the rows are retargeted onto `latest` first, because that is
    // what `apply_updates` will write. The retargeting is a display copy; the
    // write below still goes through `checks`.
    let forced_display: Option<Vec<DependencyCheck>> =
        (args.update && args.force).then(|| checks.iter().map(retarget_forced).collect());
    let display_checks: &[DependencyCheck] = forced_display.as_deref().unwrap_or(&checks);

    let mut seen_rows: HashSet<String> = HashSet::new();
    let to_render: Vec<&DependencyCheck> = display_checks
        .iter()
        // A package that could not be checked is listed outside update mode,
        // where the table is a report of what the run found. Under `-u` the
        // header says "Dependencies updated:", which such a row would
        // contradict; it surfaces in the fetch-failure list instead.
        .filter(|c| {
            if c.check_failed {
                return !args.update;
            }
            c.has_update() && (!args.update || c.will_update(args.minor, args.force))
        })
        .filter(|c| {
            let key = format!(
                "{}:{}",
                c.dependency.name,
                c.target
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default()
            );
            seen_rows.insert(key)
        })
        .collect();

    // Updates that exist but fall outside the requested severity filter. A row
    // `-uf` could not write either is counted separately: telling the user to
    // "run -uf" for it would be false, and in update mode this count is the
    // only place such a row surfaces at all.
    let skipped: HashSet<&str> = checks
        .iter()
        .filter(|c| c.is_actionable() && c.has_update() && !c.will_update(args.minor, args.force))
        .map(|c| c.dependency.name.as_str())
        .collect();
    let skipped = skipped.len();
    let blocked: HashSet<&str> = checks
        .iter()
        .filter(|c| !c.is_actionable())
        .map(|c| c.dependency.name.as_str())
        .collect();
    let blocked = blocked.len();

    // Render output
    let renderer = TableRenderer::new(true);
    if args.update && to_render.is_empty() {
        println!("No dependencies updated.");
    } else {
        let header = if args.update {
            "Dependencies updated:"
        } else {
            "Outdated dependencies:"
        };
        renderer.render_deduped(&to_render, header);
    }

    // Apply updates if requested
    if args.update {
        let updater = FileUpdater::new();
        let result = updater.apply_updates(&checks, args.minor, args.force)?;
        result.print_summary();

        if skipped > 0 && !args.force {
            println!(
                "{skipped} update(s) outside the selected severity were skipped. Run -uf to force upgrade all."
            );
        }
        if blocked > 0 {
            println!(
                "{blocked} update(s) cannot be written by -u even with --force; run without -u to see them and why."
            );
        }
    } else if to_render.iter().any(|c| !c.check_failed) {
        // Rows that could not be checked are not updates, so a table holding
        // only those must not invite the user to run `-u` on them.
        println!();
        println!(
            "Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all."
        );
    }

    // Show errors at the end
    print_fetch_failures(&errors);

    Ok(())
}
