use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use python_check_updates::cli::Args;
use python_check_updates::detector::ProjectDetector;
use python_check_updates::global::{
    generate_upgrade_commands, GlobalCheck, GlobalPackageDiscovery, UpgradeCommand,
};
use python_check_updates::output::{GlobalTableRenderer, TableRenderer, UvPythonTableRenderer};
use python_check_updates::parsers::{
    CondaParser, DependencyParser, LockfileParser, PyProjectParser, RequirementsParser,
};
use python_check_updates::pypi::PyPiClient;
use python_check_updates::python::get_python_info;
use python_check_updates::resolver::{DependencyCheck, DependencyResolver};
use python_check_updates::updater::FileUpdater;
use python_check_updates::uv_python::{generate_uv_python_upgrade_commands, UvPythonDiscovery};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

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
    // Warn if -u flag is used
    if args.update {
        println!(
            "Note: --update flag is ignored in global mode. Commands will be shown instead.\n"
        );
    }

    // 1. Discover global packages, fetch Python info, and check uv Python versions concurrently
    let discovery = GlobalPackageDiscovery::new(args.pre_release);
    let uv_python_discovery = UvPythonDiscovery::new();
    let (packages, python_info, uv_python_checks) = tokio::join!(
        async { discovery.discover() },
        get_python_info(true),
        async { uv_python_discovery.discover_and_check().await }
    );

    // Print Python version header
    if let Some(py_info) = python_info {
        let version_str = if let Some(ref latest) = py_info.latest {
            if py_info.has_update() {
                format!(
                    "Python {} ({} available)",
                    py_info.current,
                    latest.to_string().yellow()
                )
            } else {
                format!("Python {} (latest)", py_info.current)
            }
        } else {
            format!("Python {}", py_info.current)
        };
        println!("{}\n", version_str);
    }

    if packages.is_empty() {
        println!("No globally installed packages found.");
        println!("Checked: uv tools, pipx, pip --user");
        return Ok(());
    }

    // 2. Query PyPI for latest versions
    let package_names: Vec<String> = packages
        .iter()
        .map(|p| p.name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let pypi_client = PyPiClient::new(args.pre_release);

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

    let pb_clone = Arc::new(Mutex::new(progress_bar.clone()));
    let result = pypi_client
        .get_packages(&package_names, move |current, _total| {
            let pb = pb_clone.lock().unwrap();
            pb.set_position(current as u64);
        })
        .await?;

    progress_bar.finish_and_clear();

    let package_infos = result.packages;
    let fetch_errors = result.errors;

    // 3. Build check results
    let mut checks: Vec<GlobalCheck> = Vec::new();

    for package in packages {
        if let Some(info) = package_infos.get(&package.name) {
            // Determine target version based on flags
            let target = if args.minor {
                // -m flag: limit to same major
                info.versions
                    .iter()
                    .filter(|v| v.major == package.installed_version.major)
                    .max()
                    .cloned()
                    .unwrap_or_else(|| info.latest.clone())
            } else {
                // Default/force_latest: absolute latest (no constraints in global mode)
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

    // 4. Display results (renderer shows "All packages up to date." per section if needed)
    let renderer = GlobalTableRenderer::new(true);
    renderer.render(&checks);

    // 4b. Display uv Python version checks
    if let Ok(uv_checks) = &uv_python_checks {
        if !uv_checks.is_empty() {
            println!();
            let uv_renderer = UvPythonTableRenderer::new(true);
            uv_renderer.render(uv_checks);
        }
    }

    // 5. Print upgrade commands
    let mut commands = generate_upgrade_commands(&checks);

    // Add uv Python upgrade commands
    if let Ok(uv_checks) = &uv_python_checks {
        commands.extend(generate_uv_python_upgrade_commands(uv_checks));
    }

    if !commands.is_empty() {
        println!();
        println!("To upgrade, run:\n");
        for cmd in &commands {
            match cmd {
                UpgradeCommand::Command(c) => println!("  $ {}", c),
                UpgradeCommand::Comment(c) => println!("  # {}", c.dimmed()),
            }
        }
    }

    // 6. Print fetch errors at the end
    if !fetch_errors.is_empty() {
        println!();
        println!("{}", "Packages not found on PyPI:".dimmed());
        for error in &fetch_errors {
            println!("  {}", error.dimmed());
        }
    }

    Ok(())
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

    // 3. Query PyPI for latest versions (and Python version in parallel)
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
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
    );

    let progress_bar_clone = Arc::new(Mutex::new(progress_bar.clone()));

    // Fetch package info and Python version concurrently
    let (pypi_result, python_info) = tokio::join!(
        pypi_client.get_packages(&package_names, move |current, _total| {
            let pb = progress_bar_clone.lock().unwrap();
            pb.set_position(current as u64);
        }),
        get_python_info(true)
    );

    let pypi_result = pypi_result?;
    let package_infos = pypi_result.packages;
    let fetch_errors = pypi_result.errors;
    progress_bar.finish_and_clear();

    // Print Python version header
    if let Some(py_info) = python_info {
        let version_str = if let Some(ref latest) = py_info.latest {
            if py_info.has_update() {
                format!(
                    "Python {} ({} available)",
                    py_info.current,
                    latest.to_string().yellow()
                )
            } else {
                format!("Python {} (latest)", py_info.current)
            }
        } else {
            format!("Python {}", py_info.current)
        };
        println!("{}\n", version_str);
    }

    // Print fetch errors if any
    if !fetch_errors.is_empty() {
        println!("{}", "Packages not found on PyPI:".dimmed());
        for error in &fetch_errors {
            println!("  {}", error.dimmed());
        }
        println!();
    }

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
            println!("Updated {} file(s):", result.modified_files.len());
            for file in &result.modified_files {
                println!("  - {}", file.display());
            }
        }

        result.print_summary();
    }

    Ok(())
}
