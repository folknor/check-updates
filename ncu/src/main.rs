use anyhow::{Context, Result};
use check_updates_core::{DependencyResolver, Version};
use clap::Parser;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};

use ncu::cli::Args;
use ncu::detector::ProjectDetector;
use ncu::global::{generate_upgrade_commands, GlobalCheck, GlobalPackageDiscovery};
use ncu::npm::NpmClient;
use ncu::output::{GlobalTableRenderer, TableRenderer};
use ncu::parsers::{LockfileParser, PackageJsonParser};
use ncu::updater::FileUpdater;

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

    // 1. Discover global packages
    let discovery = GlobalPackageDiscovery::new();
    let packages = discovery.discover();

    if packages.is_empty() {
        println!("No globally installed npm packages found.");
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
    let mut errors: Vec<(String, String)> = Vec::new();

    for (name, result) in results {
        match result {
            Ok(info) => {
                package_infos.insert(name, info);
            }
            Err(e) => {
                errors.push((name, e.to_string()));
            }
        }
    }

    // 3. Build check results
    let mut checks: Vec<GlobalCheck> = Vec::new();

    for package in packages {
        if let Some(info) = package_infos.get(&package.name) {
            let target = if args.minor {
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
    if !errors.is_empty() {
        println!();
        println!("{}", "Packages not found on npm:".dimmed());
        for (name, error) in errors {
            println!("  {}: {}", name.dimmed(), error.dimmed());
        }
    }

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
        println!("No package.json files found in {project_path:?}");
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
        println!("No dependencies found");
        return Ok(());
    }

    // Deduplicate by package name (keep first occurrence)
    let mut seen = HashSet::new();
    all_deps.retain(|d| seen.insert(d.name.clone()));

    // Query npm registry
    let client = NpmClient::new(args.pre_release);
    let package_names: Vec<String> = all_deps.iter().map(|d| d.name.clone()).collect();

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
    let mut errors: Vec<(String, String)> = Vec::new();

    for (name, result) in results {
        match result {
            Ok(info) => {
                package_infos.insert(name, info);
            }
            Err(e) => {
                errors.push((name, e.to_string()));
            }
        }
    }

    // Resolve dependencies
    let resolver = DependencyResolver::new();
    let mut checks = Vec::new();

    for dep in &all_deps {
        if let Some(info) = package_infos.get(&dep.name) {
            let installed = installed_versions.get(&dep.name);
            let check = resolver.resolve(dep, info, installed);
            checks.push(check);
        }
    }

    // Render output
    let renderer = TableRenderer::new(true);
    renderer.render(&checks);

    // Apply updates if requested
    if args.update {
        let updater = FileUpdater::new();
        let result = updater.apply_updates(&checks, args.minor, args.force)?;
        result.print_summary();
    } else if checks.iter().any(check_updates_core::DependencyCheck::has_update) {
        println!();
        println!("Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.");
    }

    // Show errors at the end
    if !errors.is_empty() {
        println!();
        println!("Packages not found on npm:");
        for (name, error) in errors {
            println!("  {name}: {error}");
        }
    }

    Ok(())
}
