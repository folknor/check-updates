use anyhow::Result;
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use python_check_updates::cli::Args;
use python_check_updates::detector::ProjectDetector;
use python_check_updates::output::TableRenderer;
use python_check_updates::parsers::{
    CondaParser, DependencyParser, LockfileParser, PyProjectParser, RequirementsParser,
};
use python_check_updates::pypi::PyPiClient;
use python_check_updates::resolver::{DependencyCheck, DependencyResolver};
use python_check_updates::updater::FileUpdater;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let project_path = args.project_path();

    // Validate project path exists
    if !project_path.exists() {
        anyhow::bail!("Project path does not exist: {:?}", project_path);
    }

    if !project_path.is_dir() {
        anyhow::bail!("Project path is not a directory: {:?}", project_path);
    }

    // 1. Detect project type and find dependency files
    let detector = ProjectDetector::new(project_path.clone());
    let detected_files = detector.detect()?;

    if detected_files.is_empty() {
        println!("No dependency files found in {:?}", project_path);
        return Ok(());
    }

    // 2. Parse all dependency files
    let requirements_parser = RequirementsParser::new();
    let pyproject_parser = PyProjectParser::new();
    let conda_parser = CondaParser::new();
    let lockfile_parser = LockfileParser::new();

    let mut all_dependencies = Vec::new();

    for detected in &detected_files {
        let deps = if requirements_parser.can_parse(&detected.path) {
            requirements_parser.parse(&detected.path)?
        } else if pyproject_parser.can_parse(&detected.path) {
            pyproject_parser.parse(&detected.path)?
        } else if conda_parser.can_parse(&detected.path) {
            conda_parser.parse(&detected.path)?
        } else {
            Vec::new()
        };

        all_dependencies.extend(deps);
    }

    if all_dependencies.is_empty() {
        println!("No dependencies found in any files");
        return Ok(());
    }

    // Get installed versions from lock file
    let installed_versions = lockfile_parser.find_and_parse(&project_path)?;

    // 3. Query PyPI for latest versions
    let package_names: Vec<String> = all_dependencies
        .iter()
        .map(|d| d.name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let pypi_client = PyPiClient::new(args.pre_release);

    // Create progress bar
    let progress_bar = ProgressBar::new(package_names.len() as u64);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );

    let progress_bar_clone = Arc::new(Mutex::new(progress_bar.clone()));
    let package_infos = pypi_client
        .get_packages(&package_names, move |current, _total| {
            let pb = progress_bar_clone.lock().unwrap();
            pb.set_position(current as u64);
        })
        .await?;

    progress_bar.finish_and_clear();

    // 4. Resolve updates based on flags
    let resolver = DependencyResolver::new(args.clone());
    let mut checks: Vec<DependencyCheck> = Vec::new();

    for dependency in &all_dependencies {
        if let Some(package_info) = package_infos.get(&dependency.name) {
            let installed = installed_versions.get(&dependency.name);
            let check = resolver.resolve(dependency, package_info, installed);
            checks.push(check);
        }
    }

    // 5. Display results table
    let has_updates = checks.iter().any(|c| c.has_update());

    if !has_updates {
        println!("All dependencies are up to date!");
        return Ok(());
    }

    let renderer = TableRenderer::new(true);
    renderer.render(&checks);

    // 6. If --update, apply updates to files
    if args.update {
        let updater = FileUpdater::new();
        let result = updater.apply_updates(&checks)?;

        println!();
        if !result.modified_files.is_empty() {
            println!(
                "Updated {} file(s):",
                result.modified_files.len()
            );
            for file in &result.modified_files {
                println!("  - {}", file.display());
            }
        }

        result.print_summary();
    }

    Ok(())
}
