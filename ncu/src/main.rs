use anyhow::{Context, Result};
use check_updates_core::{DependencyResolver, TableRenderer, Version};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;

use ncu::cli::Args;
use ncu::detector::ProjectDetector;
use ncu::npm::NpmClient;
use ncu::parsers::{LockfileParser, PackageJsonParser};
use ncu::updater::FileUpdater;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let project_path = args.project_path();

    if !project_path.exists() {
        anyhow::bail!("Project path does not exist: {:?}", project_path);
    }

    // Detect package.json files
    let detector = ProjectDetector::new(project_path.clone());
    let detected_files = detector.detect()?;

    if detected_files.is_empty() {
        println!("No package.json files found in {:?}", project_path);
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
    let mut seen = std::collections::HashSet::new();
    all_deps.retain(|d| seen.insert(d.name.clone()));

    // Query npm registry
    let client = NpmClient::new(args.pre_release);
    let package_names: Vec<String> = all_deps.iter().map(|d| d.name.clone()).collect();

    let progress = ProgressBar::new(package_names.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    let results = client.get_packages(&package_names).await;
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
    } else if checks.iter().any(|c| c.has_update()) {
        println!();
        println!("Run -u to upgrade patch, -um to upgrade patch+minors, and -uf to force upgrade all.");
    }

    // Show errors at the end
    if !errors.is_empty() {
        println!();
        println!("Packages not found on npm:");
        for (name, error) in errors {
            println!("  {}: {}", name, error);
        }
    }

    Ok(())
}
