use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ccu::cli::Args;
use ccu::cratesio::{CratesIoClient, FetchError};
use ccu::detector::ProjectDetector;
use ccu::global::{
    GlobalCheck, GlobalPackageDiscovery, GlobalSource, check_git_updates, check_path_updates,
    generate_upgrade_commands,
};
use ccu::output::GlobalTableRenderer;
use ccu::parsers::{CargoLockParser, CargoTomlParser, DependencyParser};
use ccu::updater::FileUpdater;
use check_updates_core::{
    DependencyCheck, DependencyResolver, TableRenderer, UpdateSeverity, Version,
};

const SCHEMA_VERSION: u32 = 1;
const TOOL_NAME: &str = "ccu";

/// Wraps a GlobalCheck so JSON output includes the computed severity.
#[derive(Serialize)]
struct GlobalCheckJson<'a> {
    #[serde(flatten)]
    inner: &'a GlobalCheck,
    severity: Option<UpdateSeverity>,
}

/// One JSON object per failed registry lookup. `kind` is the stable tag from
/// `FetchErrorKind::as_str`, so a consumer can tell `not_found` from
/// `rate_limited` without parsing `message`.
fn errors_to_json(failures: &[FetchError]) -> Vec<serde_json::Value> {
    failures
        .iter()
        .map(|f| {
            serde_json::json!({
                "package": f.package,
                "kind": f.kind.as_str(),
                "message": f.detail,
            })
        })
        .collect()
}

/// Print registry failures under a header that matches what actually happened.
///
/// Only a 404 means "not on crates.io". A rate limit, a timeout or an outage
/// says nothing about whether the crate exists, and printing those under a
/// "not found" header told the user their dependencies do not exist when
/// the network was down.
fn print_fetch_failures(failures: &[FetchError]) {
    let (missing, unchecked): (Vec<&FetchError>, Vec<&FetchError>) =
        failures.iter().partition(|f| f.kind.is_missing());

    if !missing.is_empty() {
        println!("{}", "Crates not found on crates.io:".dimmed());
        for failure in missing {
            println!("  {}", failure.detail.dimmed());
        }
        println!();
    }
    if !unchecked.is_empty() {
        println!("{}", "Crates that could not be checked:".dimmed());
        for failure in unchecked {
            println!("  {}", failure.detail.dimmed());
        }
        println!();
    }
}

/// Dependencies we resolved from the manifests but could not check, because
/// crates.io never answered for that name. They cannot appear in `checks` -
/// a `DependencyCheck` requires a `latest` version we do not have - so they get
/// their own machine-readable array. Without it a consumer cannot tell
/// "up to date" from "we could not check": the free-text `errors` strings carry
/// no structured package name.
fn unchecked_to_json(unchecked: &[UncheckedDependency]) -> Vec<serde_json::Value> {
    unchecked
        .iter()
        .map(|u| {
            serde_json::json!({
                "name": u.name,
                "source_file": u.source_file,
                "section": u.section,
            })
        })
        .collect()
}

struct UncheckedDependency {
    name: String,
    source_file: std::path::PathBuf,
    section: Option<String>,
}

fn emit_json_project(
    checks: &[DependencyCheck],
    errors: &[FetchError],
    unchecked: &[UncheckedDependency],
) -> Result<()> {
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "tool": TOOL_NAME,
        "mode": "project",
        "checks": checks,
        "unchecked": unchecked_to_json(unchecked),
        "errors": errors_to_json(errors),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn emit_json_global(checks: &[GlobalCheck], errors: &[FetchError]) -> Result<()> {
    let with_severity: Vec<GlobalCheckJson<'_>> = checks
        .iter()
        .map(|c| GlobalCheckJson {
            inner: c,
            severity: c.update_severity(),
        })
        .collect();
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "tool": TOOL_NAME,
        "mode": "global",
        "checks": with_severity,
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

    // 1. Discover installed crates from ~/.cargo/.crates.toml
    let discovery = GlobalPackageDiscovery::new();
    let packages = discovery.discover()?;

    if packages.is_empty() {
        if args.json {
            emit_json_global(&[], &[])?;
        } else {
            println!("No globally installed cargo crates found.");
        }
        return Ok(());
    }

    let registry_names: Vec<String> = packages
        .iter()
        .filter(|p| p.source == GlobalSource::Registry)
        .map(|p| p.name.clone())
        .collect();
    let git_count = packages
        .iter()
        .filter(|p| p.source == GlobalSource::Git)
        .count();

    // 2. Check path repos (local git fetch), query crates.io, and check git repos concurrently
    let cratesio_client = CratesIoClient::new(args.pre_release);

    let progress_bar = ProgressBar::new((registry_names.len() + git_count) as u64);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .expect("valid progress template")
            .progress_chars("#>-"),
    );

    // The bar spans both producers (registry lookups plus git remote checks), so
    // each reports its own completed count into a shared pair of counters and the
    // position is their sum. Both callbacks receive an absolute count, not a delta.
    let registry_done = Arc::new(AtomicUsize::new(0));
    let git_done = Arc::new(AtomicUsize::new(0));

    let progress_bar_clone = Arc::new(Mutex::new(progress_bar.clone()));
    let pb_for_registry = Arc::clone(&progress_bar_clone);
    let pb_for_git = Arc::clone(&progress_bar_clone);
    let registry_done_reg = Arc::clone(&registry_done);
    let git_done_reg = Arc::clone(&git_done);
    let registry_done_git = Arc::clone(&registry_done);
    let git_done_git = Arc::clone(&git_done);

    let (path_statuses, cratesio_result, git_statuses) = tokio::join!(
        check_path_updates(&packages),
        cratesio_client.get_packages(&registry_names, move |current, _total| {
            registry_done_reg.store(current, Ordering::Relaxed);
            let total = current + git_done_reg.load(Ordering::Relaxed);
            let pb = pb_for_registry.lock().expect("lock poisoned");
            pb.set_position(total as u64);
        }),
        check_git_updates(&packages, move |current| {
            git_done_git.store(current, Ordering::Relaxed);
            let total = current + registry_done_git.load(Ordering::Relaxed);
            let pb = pb_for_git.lock().expect("lock poisoned");
            pb.set_position(total as u64);
        })
    );

    progress_bar.finish_and_clear();

    let cratesio_result = cratesio_result?;
    let package_infos = cratesio_result.packages;
    let fetch_failures = cratesio_result.failures;

    // 3. Build checks
    let mut checks: Vec<GlobalCheck> = Vec::new();

    for pkg in &packages {
        match pkg.source {
            GlobalSource::Registry => {
                if let Some(info) = package_infos.get(&pkg.name) {
                    let has_update = info.latest > pkg.installed_version;
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: Some(info.latest.clone()),
                        latest_hash: None,
                        commits_behind: None,
                        has_dirty_changes: false,
                        has_update,
                        // crates.io answered for this name, so the comparison is real.
                        check_failed: false,
                    });
                } else {
                    // crates.io never answered. The git and path arms below
                    // already keep a `check_failed` row for this case; a
                    // registry crate used to vanish from the table and the
                    // JSON `checks` array instead, which reads as "fine".
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: None,
                        latest_hash: None,
                        commits_behind: None,
                        has_dirty_changes: false,
                        has_update: false,
                        check_failed: true,
                    });
                }
            }
            GlobalSource::Git => {
                if let Some(status) = git_statuses.get(&pkg.name) {
                    let has_update = status.commits_behind > 0;
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: None,
                        latest_hash: Some(status.latest_hash.clone()),
                        commits_behind: Some(status.commits_behind),
                        has_dirty_changes: false,
                        has_update,
                        check_failed: status.unknown,
                    });
                } else {
                    // No entry at all means the check never ran (missing url or
                    // hash). Report "unknown", never "up to date".
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: None,
                        latest_hash: None,
                        commits_behind: None,
                        has_dirty_changes: false,
                        has_update: false,
                        check_failed: true,
                    });
                }
            }
            GlobalSource::Path => {
                if let Some(status) = path_statuses.get(&pkg.name) {
                    let has_update = status.commits_behind > 0;
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: None,
                        latest_hash: None,
                        commits_behind: Some(status.commits_behind),
                        has_dirty_changes: status.has_dirty_changes,
                        has_update,
                        check_failed: status.unknown,
                    });
                } else {
                    // The local repo was never interrogated, so we know nothing.
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: None,
                        latest_hash: None,
                        commits_behind: None,
                        has_dirty_changes: false,
                        has_update: false,
                        check_failed: true,
                    });
                }
            }
        }
    }

    // 4. Render results
    if args.json {
        emit_json_global(&checks, &fetch_failures)?;
        return Ok(());
    }

    let renderer = GlobalTableRenderer::new(true);
    renderer.render(&checks);

    // 5. Generate upgrade commands
    let commands = generate_upgrade_commands(&checks);
    if !commands.is_empty() {
        println!("\nTo upgrade, run:\n");
        for cmd in &commands {
            println!("  $ {cmd}");
        }
    }

    // 6. Registry failures last, so they are not lost above the table
    if !fetch_failures.is_empty() {
        println!();
        print_fetch_failures(&fetch_failures);
    }

    Ok(())
}

async fn run_project_mode(args: &Args) -> Result<()> {
    let project_path = args.project_path();

    // Validate project path exists
    if !project_path.exists() {
        anyhow::bail!("Project path does not exist: {project_path:?}");
    }

    if !project_path.is_dir() {
        anyhow::bail!("Project path is not a directory: {project_path:?}");
    }

    // 1. Detect Cargo.toml
    let detector = ProjectDetector::new(project_path.clone());
    let detected_files = detector.detect()?;

    if detected_files.is_empty() {
        if args.json {
            emit_json_project(&[], &[], &[])?;
        } else {
            println!("No Cargo.toml found in {project_path:?}");
        }
        return Ok(());
    }

    // 2. Parse Cargo.toml
    let mut cargo_toml_parser = CargoTomlParser::new();
    let lockfile_parser = CargoLockParser::new();

    // Load workspace dependency versions from root so member crates'
    // `.workspace = true` references can be resolved. Cargo resolves those from the
    // workspace root, not from the directory we were pointed at, so running against a
    // member crate must still read the root manifest - otherwise every
    // `.workspace = true` dependency silently drops out.
    let workspace_root = detector.workspace_root();
    let root_cargo_toml = workspace_root.join("Cargo.toml");
    if root_cargo_toml.exists() {
        cargo_toml_parser.load_workspace_deps(&root_cargo_toml)?;
    }

    let mut all_dependencies = Vec::new();

    for detected in &detected_files {
        if cargo_toml_parser.can_parse(&detected.path) {
            let deps = cargo_toml_parser.parse(&detected.path)?;
            all_dependencies.extend(deps);
        }
    }

    if all_dependencies.is_empty() {
        if args.json {
            emit_json_project(&[], &[], &[])?;
        } else {
            println!("No dependencies found in Cargo.toml");
        }
        return Ok(());
    }

    // Get installed versions from Cargo.lock. A workspace keeps a single lockfile at its
    // root, so look there rather than beside the member manifest.
    let installed_versions = lockfile_parser.find_and_parse(&workspace_root)?;

    // 3. Query crates.io for latest versions
    // Sorted, because `HashSet` iteration order is not stable between runs and it drives
    // the order of the `errors` array in `--json` output.
    let mut package_names: Vec<String> = all_dependencies
        .iter()
        .map(|d| d.name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    package_names.sort();

    let cratesio_client = CratesIoClient::new(args.pre_release);

    // Create progress bar
    let progress_bar = ProgressBar::new(package_names.len() as u64);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .expect("valid progress template")
            .progress_chars("#>-"),
    );

    let progress_bar_clone = Arc::new(Mutex::new(progress_bar.clone()));

    let cratesio_result = cratesio_client
        .get_packages(&package_names, move |current, _total| {
            let pb = progress_bar_clone.lock().expect("lock poisoned");
            pb.set_position(current as u64);
        })
        .await?;

    let package_infos = cratesio_result.packages;
    let fetch_failures = cratesio_result.failures;
    progress_bar.finish_and_clear();

    // Print fetch failures if any (suppress in JSON mode; included in payload below)
    if !args.json {
        print_fetch_failures(&fetch_failures);
    }

    // 4. Resolve updates
    let resolver = DependencyResolver::new();
    let mut checks: Vec<DependencyCheck> = Vec::new();
    // Dependencies crates.io never answered for. Kept rather than dropped, so
    // "we could not check this" is distinguishable from "it is up to date".
    let mut unchecked: Vec<UncheckedDependency> = Vec::new();

    for dependency in &all_dependencies {
        if let Some(package_info) = package_infos.get(&dependency.name) {
            let installed = installed_versions
                .get(&dependency.name)
                .and_then(|versions| {
                    // When multiple versions exist in Cargo.lock (e.g. direct + transitive),
                    // pick the highest version that satisfies the declared spec
                    let mut matching: Vec<&Version> = versions
                        .iter()
                        .filter(|v| dependency.version_spec.satisfies(v))
                        .collect();
                    matching.sort();
                    matching.last().copied().or_else(|| {
                        // Fallback: highest overall (shouldn't happen in practice)
                        versions.iter().max()
                    })
                });
            let check = resolver.resolve(dependency, package_info, installed);
            checks.push(check);
        } else if !unchecked.iter().any(|u| u.name == dependency.name) {
            // The fetch is per crate name, so one entry per name is the whole
            // story; a workspace-inherited dep would otherwise repeat once per
            // inheriting member.
            unchecked.push(UncheckedDependency {
                name: dependency.name.clone(),
                source_file: dependency.source_file.clone(),
                section: dependency.section.clone(),
            });
        }
    }

    // 5. Deduplicate for display (same crate with same target)
    let mut seen: HashSet<String> = HashSet::new();
    let deduplicated: Vec<&DependencyCheck> = checks
        .iter()
        .filter(|c| {
            if !c.has_update() {
                return false;
            }
            // In update mode only list what the severity filter will actually write,
            // so -u/-um never claim to have applied a major bump they skipped.
            if args.update && !c.will_update(args.minor, args.force) {
                return false;
            }
            let key = format!(
                "{}:{}",
                c.dependency.name,
                c.target
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default()
            );
            seen.insert(key)
        })
        .collect();

    // 6. Display results
    if args.json {
        if args.update {
            let updater = FileUpdater::new();
            let result = updater.apply_updates(&checks, args.minor, args.force)?;
            result.print_not_applied();
        }
        emit_json_project(&checks, &fetch_failures, &unchecked)?;
        return Ok(());
    }

    // Updates that exist but fall outside the requested severity filter
    let skipped: HashSet<&str> = checks
        .iter()
        .filter(|c| c.has_update() && !c.will_update(args.minor, args.force))
        .map(|c| c.dependency.name.as_str())
        .collect();
    let skipped = skipped.len();

    // 7. If --update, apply updates based on severity filter.
    //
    // The write happens *before* the table is rendered: printing
    // "Dependencies updated:" and then failing to write claims a success the
    // process is about to contradict with an error.
    let update_result = if args.update {
        let updater = FileUpdater::new();
        Some(updater.apply_updates(&checks, args.minor, args.force)?)
    } else {
        None
    };

    let renderer = TableRenderer::new(true);
    if args.update && deduplicated.is_empty() {
        println!("No dependencies updated.");
    } else {
        let header = if args.update {
            "Dependencies updated:"
        } else {
            "Outdated dependencies:"
        };
        renderer.render_deduped(&deduplicated, header);
    }

    if let Some(result) = update_result {
        result.print_not_applied();

        println!();
        if !result.modified_files.is_empty() {
            println!("Updated {} file(s):", result.modified_files.len());
            for file in &result.modified_files {
                println!("  - {}", file.display());
            }
        }

        result.print_summary();

        if skipped > 0 && !args.force {
            println!(
                "{skipped} update(s) outside the selected severity were skipped. Run {} to force upgrade all.",
                "-uf".cyan()
            );
        }
    } else if !deduplicated.is_empty() {
        println!();
        println!(
            "Run {} to upgrade patch, {} to upgrade patch+minors, and {} to force upgrade all.",
            "-u".cyan(),
            "-um".cyan(),
            "-uf".cyan()
        );
    }

    Ok(())
}
