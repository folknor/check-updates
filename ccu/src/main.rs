use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use ccu::cli::Args;
use ccu::cratesio::CratesIoClient;
use ccu::detector::ProjectDetector;
use ccu::parsers::{CargoLockParser, CargoTomlParser, DependencyParser};
use ccu::updater::FileUpdater;
use check_updates_core::{DependencyCheck, DependencyResolver, TableRenderer};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    run_project_mode(&args).await
}

async fn run_project_mode(args: &Args) -> Result<()> {
    let project_path = args.project_path();

    // Validate project path exists
    if !project_path.exists() {
        anyhow::bail!("Project path does not exist: {:?}", project_path);
    }

    if !project_path.is_dir() {
        anyhow::bail!("Project path is not a directory: {:?}", project_path);
    }

    // 1. Detect Cargo.toml
    let detector = ProjectDetector::new(project_path.clone());
    let detected_files = detector.detect()?;

    if detected_files.is_empty() {
        println!("No Cargo.toml found in {:?}", project_path);
        return Ok(());
    }

    // 2. Parse Cargo.toml
    let cargo_toml_parser = CargoTomlParser::new();
    let lockfile_parser = CargoLockParser::new();

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
            .unwrap()
            .progress_chars("#>-"),
    );

    let progress_bar_clone = Arc::new(Mutex::new(progress_bar.clone()));

    let cratesio_result = cratesio_client
        .get_packages(&package_names, move |current, _total| {
            let pb = progress_bar_clone.lock().unwrap();
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
            let installed = installed_versions.get(&dependency.name);
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
                c.target.as_ref().map(|v| v.to_string()).unwrap_or_default()
            );
            seen.insert(key)
        })
        .collect();

    // 6. Display results
    let renderer = TableRenderer::new(true);
    renderer.render_deduped(&deduplicated);

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
