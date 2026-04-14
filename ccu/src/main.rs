use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use ccu::cli::Args;
use ccu::cratesio::CratesIoClient;
use ccu::detector::ProjectDetector;
use ccu::global::{
    check_git_updates, check_path_updates, generate_upgrade_commands, GlobalCheck,
    GlobalPackageDiscovery, GlobalSource,
};
use ccu::output::GlobalTableRenderer;
use ccu::parsers::{CargoLockParser, CargoTomlParser, DependencyParser};
use ccu::updater::FileUpdater;
use check_updates_core::{DependencyCheck, DependencyResolver, TableRenderer, Version};

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
    if args.update {
        println!(
            "Note: --update flag is ignored in global mode. Commands will be shown instead.\n"
        );
    }

    // 1. Discover installed crates from ~/.cargo/.crates.toml
    let discovery = GlobalPackageDiscovery::new();
    let packages = discovery.discover()?;

    if packages.is_empty() {
        println!("No globally installed cargo crates found.");
        return Ok(());
    }

    let registry_names: Vec<String> = packages
        .iter()
        .filter(|p| p.source == GlobalSource::Registry)
        .map(|p| p.name.clone())
        .collect();
    let git_count = packages.iter().filter(|p| p.source == GlobalSource::Git).count();

    // 2. Check path repos (local git fetch), query crates.io, and check git repos concurrently
    let path_statuses = check_path_updates(&packages);

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

    let progress_bar_clone = Arc::new(Mutex::new(progress_bar.clone()));
    let pb_for_registry = Arc::clone(&progress_bar_clone);

    let (cratesio_result, git_statuses) = tokio::join!(
        cratesio_client.get_packages(&registry_names, move |current, _total| {
            let pb = pb_for_registry.lock().expect("lock poisoned");
            pb.set_position(current as u64);
        }),
        async {
            let result = check_git_updates(&packages).await;
            let pb = progress_bar_clone.lock().expect("lock poisoned");
            pb.set_position(pb.length().unwrap_or(0));
            result
        }
    );

    progress_bar.finish_and_clear();

    let cratesio_result = cratesio_result?;
    let package_infos = cratesio_result.packages;

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
                    });
                } else {
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: None,
                        latest_hash: None,
                        commits_behind: None,
                        has_dirty_changes: false,
                        has_update: false,
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
                    });
                } else {
                    checks.push(GlobalCheck {
                        package: pkg.clone(),
                        latest_version: None,
                        latest_hash: None,
                        commits_behind: None,
                        has_dirty_changes: false,
                        has_update: false,
                    });
                }
            }
        }
    }

    // 4. Render results
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
        println!("No Cargo.toml found in {project_path:?}");
        return Ok(());
    }

    // 2. Parse Cargo.toml
    let mut cargo_toml_parser = CargoTomlParser::new();
    let lockfile_parser = CargoLockParser::new();

    // Load workspace dependency versions from root so member crates'
    // `.workspace = true` references can be resolved
    let root_cargo_toml = project_path.join("Cargo.toml");
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
        println!("No dependencies found in Cargo.toml");
        return Ok(());
    }

    // Get installed versions from Cargo.lock
    let installed_versions = lockfile_parser.find_and_parse(&project_path)?;

    // 3. Query crates.io for latest versions
    let package_names: Vec<String> = all_dependencies
        .iter()
        .map(|d| d.name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

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
    let fetch_errors = cratesio_result.errors;
    progress_bar.finish_and_clear();

    // Print fetch errors if any
    if !fetch_errors.is_empty() {
        println!("{}", "Crates not found on crates.io:".dimmed());
        for error in &fetch_errors {
            println!("  {}", error.dimmed());
        }
        println!();
    }

    // 4. Resolve updates
    let resolver = DependencyResolver::new();
    let mut checks: Vec<DependencyCheck> = Vec::new();

    for dependency in &all_dependencies {
        if let Some(package_info) = package_infos.get(&dependency.name) {
            let installed = installed_versions.get(&dependency.name).and_then(|versions| {
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
            let key = format!(
                "{}:{}",
                c.dependency.name,
                c.target.as_ref().map(std::string::ToString::to_string).unwrap_or_default()
            );
            seen.insert(key)
        })
        .collect();

    // 6. Display results
    let renderer = TableRenderer::new(true);
    let header = if args.update {
        "Dependencies updated:"
    } else {
        "Outdated dependencies:"
    };
    renderer.render_deduped(&deduplicated, header);

    // 7. If --update, apply updates based on severity filter
    if args.update {
        let updater = FileUpdater::new();
        let result = updater.apply_updates(&checks, args.minor, args.force)?;

        println!();
        if !result.modified_files.is_empty() {
            println!("Updated {} file(s):", result.modified_files.len());
            for file in &result.modified_files {
                println!("  - {}", file.display());
            }
        }

        result.print_summary();
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
