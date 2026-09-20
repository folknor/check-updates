use anyhow::Result;
use check_updates_core::{Dependency, DependencyCheck, DependencyResolver, UpdateSeverity};
use clap::Parser;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use pcu::cli::Args;
use pcu::detector::ProjectDetector;
use pcu::global::{GlobalCheck, GlobalPackageDiscovery, UpgradeCommand, generate_upgrade_commands};
use pcu::output::{GlobalTableRenderer, TableRenderer, UvPythonTableRenderer};
use pcu::parsers::{
    CondaParser, DependencyParser, LockfileParser, PyProjectParser, RequirementsParser,
};
use pcu::pypi::{FetchError, PyPiClient};
use pcu::python::get_python_info;
use pcu::updater::FileUpdater;
use pcu::uv_python::{UvPythonCheck, UvPythonDiscovery, generate_uv_python_upgrade_commands};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

const SCHEMA_VERSION: u32 = 1;
const TOOL_NAME: &str = "pcu";

/// `kind` tag for a dependency that was found and parsed but deliberately not
/// resolved, because the registry that owns it is not one pcu can query. Today
/// that is exactly the conda-channel half of `environment.yml`.
const UNCHECKED_KIND: &str = "registry_unsupported";

/// True when a dependency came from the conda-channel half of an
/// `environment.yml` rather than from PyPI.
///
/// The entries under `dependencies:` are resolved by conda against
/// conda-forge/defaults; the nested `pip:` list genuinely is PyPI and must keep
/// resolving as it always has. Checking the *file* as well as the section keeps
/// this from catching a `requirements.txt` or PEP 621 dependency that happens to
/// carry the section name `dependencies`.
///
/// Sending conda names to PyPI is not merely noisy. `python`, `mkl`,
/// `libgcc-ng` and `cudatoolkit` come back as fetch errors, and `pytorch` comes
/// back as an abandoned 0.1.2 stub that has nothing to do with the conda package
/// of the same name - so the user is offered an "update" computed from an
/// unrelated project's version history. `reference/resolution-principles.md` is
/// explicit that inventing information is worse than silence.
fn is_conda_channel_dependency(dep: &Dependency) -> bool {
    let from_environment_file = dep
        .source_file
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "environment.yml" || n == "environment.yaml");

    from_environment_file && dep.section.as_deref() != Some("dependencies.pip")
}

/// The honest reason shown for a conda dependency, on both output paths.
fn unchecked_conda_message(dep: &Dependency) -> String {
    format!(
        "{}: conda-channel dependency, not checked (pcu queries PyPI only)",
        dep.name
    )
}

/// Report parsed dependencies that were deliberately not resolved.
///
/// Principle 3: a row that cannot be checked is still shown, with a reason.
/// Dropping these silently would leave the user believing an `environment.yml`
/// had been checked end to end.
fn print_unchecked(unchecked: &[&Dependency]) {
    if unchecked.is_empty() {
        return;
    }
    println!(
        "{}",
        format!(
            "Not checked ({}) - conda channel packages, which pcu cannot resolve:",
            unchecked.len()
        )
        .dimmed()
    );
    for dep in unchecked {
        println!(
            "  {}",
            format!("{} {}", dep.name, dep.version_spec).dimmed()
        );
    }
    println!();
}

/// The JSON `errors` array: one object per failed PyPI lookup, carrying the
/// stable `kind` tag from `FetchErrorKind::as_str` so a consumer can tell
/// `not_found` from `rate_limited` without parsing `message`, followed by one
/// `{"message"}` object per discovery tool that was present but did not
/// answer. A present-but-broken tool must never read as a clean machine.
fn errors_to_json(
    failures: &[FetchError],
    unchecked: &[&Dependency],
    tool_errors: &[String],
) -> Vec<serde_json::Value> {
    failures
        .iter()
        .map(|f| {
            serde_json::json!({
                "package": f.package,
                "kind": f.kind.as_str(),
                "message": f.detail,
            })
        })
        .chain(unchecked.iter().map(|d| {
            serde_json::json!({
                "package": d.name,
                "kind": UNCHECKED_KIND,
                "message": unchecked_conda_message(d),
                "file": d.source_file,
            })
        }))
        .chain(
            tool_errors
                .iter()
                .map(|e| serde_json::json!({"message": e})),
        )
        .collect()
}

/// Print registry failures under a header that matches what actually happened.
///
/// Only a 404 means "not on PyPI". A rate limit, a timeout or an outage says
/// nothing about whether the package exists, and printing those under a
/// "not found" header told the user their dependencies do not exist when the
/// network was down.
fn print_fetch_failures(failures: &[FetchError]) {
    let (missing, unchecked): (Vec<&FetchError>, Vec<&FetchError>) =
        failures.iter().partition(|f| f.kind.is_missing());

    if !missing.is_empty() {
        println!("{}", "Packages not found on PyPI:".dimmed());
        for failure in missing {
            println!("  {}", failure.detail.dimmed());
        }
        println!();
    }
    if !unchecked.is_empty() {
        println!("{}", "Packages that could not be checked:".dimmed());
        for failure in unchecked {
            println!("  {}", failure.detail.dimmed());
        }
        println!();
    }
}

fn emit_json_project(
    checks: &[DependencyCheck],
    failures: &[FetchError],
    unchecked: &[&Dependency],
) -> Result<()> {
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "tool": TOOL_NAME,
        "mode": "project",
        "checks": checks,
        "errors": errors_to_json(failures, unchecked, &[]),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn emit_json_global(
    checks: &[GlobalCheck],
    python_versions: &[UvPythonCheck],
    failures: &[FetchError],
    tool_errors: &[String],
) -> Result<()> {
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "tool": TOOL_NAME,
        "mode": "global",
        "checks": checks,
        "python_versions": python_versions,
        "errors": errors_to_json(failures, &[], tool_errors),
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
    // Warn if -u flag is used
    if args.update && !args.json {
        println!(
            "Note: --update flag is ignored in global mode. Commands will be shown instead.\n"
        );
    }

    // 1. Discover global packages, fetch Python info, and check uv Python
    //    versions concurrently.
    //
    //    All three of these shell out to `uv`/`pipx`/`python`. They used to sit
    //    inside `async { <blocking call> }` blocks, which runs them strictly
    //    serially on one runtime worker while claiming concurrency.
    //    They go on the blocking pool instead. `discover_and_check` is declared
    //    async but is blocking throughout, so it is driven with
    //    `Handle::block_on` from a blocking thread rather than occupying a
    //    runtime worker; tokio here has no `time`/`process` features, so there
    //    is no timeout to wrap any of it in.
    let discovery = GlobalPackageDiscovery::new(args.pre_release);
    let uv_python_discovery = UvPythonDiscovery::new();

    let python_info_task = tokio::task::spawn_blocking(|| get_python_info(true));
    let discovery_task = tokio::task::spawn_blocking(move || discovery.discover());
    let uv_python_task = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(uv_python_discovery.discover_and_check())
    });

    let python_info = python_info_task.await.unwrap_or(None);
    let discovery_outcome = discovery_task
        .await
        .map_err(|e| anyhow::anyhow!("global package discovery panicked: {e}"))?;
    let uv_python_checks = match uv_python_task.await {
        Ok(result) => result,
        Err(e) => Err(anyhow::anyhow!("uv Python discovery panicked: {e}")),
    };

    let packages = discovery_outcome.packages;
    // Errors that are not PyPI fetch failures: a discovery tool that is
    // installed but broken, and a failing `uv python list`. Without these the
    // envelope asserts "no packages, no errors" when it simply does not know.
    let mut tool_errors = discovery_outcome.errors;
    if let Err(e) = &uv_python_checks {
        tool_errors.push(format!("uv python list: {e}"));
    }

    // Print Python version header (suppress in JSON mode)
    if !args.json
        && let Some(py_info) = python_info
    {
        println!("{}\n", python_header(&py_info));
    }

    if packages.is_empty() {
        if args.json {
            let uv_checks = uv_python_checks.unwrap_or_default();
            emit_json_global(&[], &uv_checks, &[], &tool_errors)?;
        } else {
            println!("No globally installed packages found.");
            println!("Checked: uv tools, pipx, pip --user");
            print_tool_errors(&tool_errors);
        }
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
            .expect("valid progress template")
            .progress_chars("#>-"),
    );

    let pb_clone = Arc::new(Mutex::new(progress_bar.clone()));
    let result = pypi_client
        .get_packages(&package_names, move |current, _total| {
            let pb = pb_clone.lock().expect("lock poisoned");
            pb.set_position(current as u64);
        })
        .await?;

    progress_bar.finish_and_clear();

    let package_infos = result.packages;
    let fetch_failures = result.failures;

    // 3. Build check results
    let mut checks: Vec<GlobalCheck> = Vec::new();
    let mut major_filtered = 0usize;

    for package in packages {
        let Some(info) = package_infos.get(&package.name) else {
            // The PyPI lookup failed for this package. It used to be dropped
            // here, so it was absent from the table and from the JSON `checks`
            // array, surviving only as free text in `errors`. Keep
            // the row and mark it: `latest` is a placeholder, not a claim.
            let installed = package.installed_version.clone();
            checks.push(GlobalCheck {
                package,
                latest: installed,
                has_update: false,
                check_failed: true,
            });
            continue;
        };

        // Global mode always targets the absolute latest release. Nothing is
        // written to disk here, and the upgrade commands we print
        // (`uv tool upgrade --all`, `pipx upgrade-all`) have no way to aim at
        // anything but latest - so retargeting the row to the newest
        // same-major version, as `-m` used to do, printed a version the
        // printed command would not install. `-m` is now the same severity
        // filter it is in project mode: it narrows *which rows are reported*,
        // not what they are reported against. `-f` means "no filter", which is
        // the default here, and is checked first so `-g -mf` is no longer
        // silently swallowed by `-m`.
        let target = info.latest.clone();
        let has_update = target > package.installed_version;

        let check = GlobalCheck {
            package,
            latest: target,
            has_update,
            check_failed: false,
        };

        let filtered_out = !args.force
            && args.minor
            && matches!(check.update_severity(), Some(UpdateSeverity::Major));
        if filtered_out {
            major_filtered += 1;
            continue;
        }

        checks.push(check);
    }

    // 4. Display results (renderer shows "All packages up to date." per section if needed)
    if args.json {
        let uv_checks: Vec<UvPythonCheck> = match &uv_python_checks {
            Ok(v) => v.clone(),
            Err(_) => Vec::new(),
        };
        emit_json_global(&checks, &uv_checks, &fetch_failures, &tool_errors)?;
        return Ok(());
    }

    let renderer = GlobalTableRenderer::new(true);
    renderer.render(&checks);

    // 4b. Display uv Python version checks
    if let Ok(uv_checks) = &uv_python_checks
        && !uv_checks.is_empty()
    {
        println!();
        let uv_renderer = UvPythonTableRenderer::new(true);
        uv_renderer.render(uv_checks);
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
                UpgradeCommand::Command(c) => println!("  $ {c}"),
                UpgradeCommand::Comment(c) => println!("  # {}", c.dimmed()),
            }
        }
    }

    // 6. Print fetch failures at the end
    if !fetch_failures.is_empty() {
        println!();
        print_fetch_failures(&fetch_failures);
    }

    // The table only renders rows with an update, so packages whose lookup
    // failed would otherwise leave no trace on the human path either.
    let unchecked: Vec<&str> = checks
        .iter()
        .filter(|c| c.check_failed)
        .map(|c| c.package.name.as_str())
        .collect();
    if !unchecked.is_empty() {
        println!();
        println!(
            "{}",
            format!(
                "Could not check {} package(s): {}",
                unchecked.len(),
                unchecked.join(", ")
            )
            .dimmed()
        );
    }

    if major_filtered > 0 {
        println!();
        println!(
            "{major_filtered} major update(s) hidden by {}. Drop it or pass {} to see them.",
            "-m".cyan(),
            "-f".cyan()
        );
    }

    print_tool_errors(&tool_errors);

    Ok(())
}

/// The one-line Python header printed above both tables.
///
/// `PythonInfo::latest` is the newest patch *within the current series*, and
/// the header has to say so: the old text printed "(latest)" for a 3.11.14 on
/// a machine where 3.14 exists, and printed nothing at all when the lookup
/// had failed, which made "uv is broken" and "this is the newest Python"
/// look identical. Every state the struct distinguishes gets its
/// own wording here.
fn python_header(info: &pcu::python::PythonInfo) -> String {
    use pcu::python::PythonSource;

    let mut line = format!("Python {}", info.current);
    if let PythonSource::Venv(path) = &info.source {
        line.push_str(&format!(" in {}", path.display().to_string().dimmed()));
    }

    let series = format!("{}.{}", info.current.major, info.current.minor);
    match (&info.latest, &info.latest_unknown_reason) {
        (Some(latest), _) if info.has_update() => {
            line.push_str(&format!(" ({} available)", latest.to_string().yellow()));
        }
        (Some(_), _) => line.push_str(&format!(" (latest in {series} series)")),
        (None, Some(reason)) => {
            line.push_str(&format!(
                " ({})",
                format!("latest unknown: {reason}").dimmed()
            ));
        }
        (None, None) => {}
    }

    if info.newer_series_available()
        && let Some(overall) = &info.latest_overall
    {
        line.push_str(&format!(
            "; Python {} is available via {}",
            overall.to_string().yellow(),
            "uv python install".cyan()
        ));
    }

    line
}

/// Report discovery tools that are installed but did not answer
///
/// A missing `uv` or `pipx` is silent - that is a clean machine. A present one
/// that failed is not, and must not read as "nothing installed".
fn print_tool_errors(errors: &[String]) {
    if errors.is_empty() {
        return;
    }
    println!();
    println!("{}", "Some sources could not be checked:".dimmed());
    for error in errors {
        println!("  {}", error.dimmed());
    }
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

    // 1. Detect project type and find dependency files
    let detector = ProjectDetector::new(project_path.clone());
    let detected_files = detector.detect()?;

    if detected_files.is_empty() {
        if args.json {
            emit_json_project(&[], &[], &[])?;
        } else {
            println!("No dependency files found in {project_path:?}");
        }
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

    // Split off the conda-channel dependencies before anything is sent to PyPI.
    // They stay visible - `print_unchecked` and the JSON `errors` array both
    // name them - but they are never resolved against a registry that does not
    // own them. See `is_conda_channel_dependency`.
    let (conda_dependencies, all_dependencies): (Vec<Dependency>, Vec<Dependency>) =
        all_dependencies
            .into_iter()
            .partition(is_conda_channel_dependency);
    let unchecked: Vec<&Dependency> = conda_dependencies.iter().collect();

    if all_dependencies.is_empty() {
        if args.json {
            emit_json_project(&[], &[], &unchecked)?;
        } else {
            print_unchecked(&unchecked);
            if unchecked.is_empty() {
                println!("No dependencies found in any files");
            } else {
                println!("No PyPI dependencies found in any files");
            }
        }
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
            .expect("valid progress template")
            .progress_chars("#>-"),
    );

    let progress_bar_clone = Arc::new(Mutex::new(progress_bar.clone()));

    // Fetch package info and Python version
    let python_info = get_python_info(true);
    let pypi_result = pypi_client
        .get_packages(&package_names, move |current, _total| {
            let pb = progress_bar_clone.lock().expect("lock poisoned");
            pb.set_position(current as u64);
        })
        .await;

    let pypi_result = pypi_result?;
    let package_infos = pypi_result.packages;
    let fetch_failures = pypi_result.failures;
    progress_bar.finish_and_clear();

    // Print Python version header (suppress in JSON mode)
    if !args.json
        && let Some(py_info) = python_info
    {
        println!("{}\n", python_header(&py_info));
    }

    // Print fetch failures if any (suppressed in JSON mode; included in payload)
    if !args.json {
        print_fetch_failures(&fetch_failures);
        print_unchecked(&unchecked);
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

    // 5. Deduplicate for display (same package with same target)
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

    // 6. Display results table
    if args.json {
        if args.update {
            let updater = FileUpdater::new();
            let _ = updater.apply_updates(&checks, args.minor, args.force)?;
        }
        emit_json_project(&checks, &fetch_failures, &unchecked)?;
        return Ok(());
    }

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

        if skipped > 0 && !args.force {
            println!(
                "{skipped} update(s) outside the selected severity were skipped. Run {} to force upgrade all.",
                "-uf".cyan()
            );
        }
        if blocked > 0 {
            println!(
                "{blocked} update(s) cannot be written by {} even with --force; run without {} to see them and why.",
                "-u".cyan(),
                "-u".cyan()
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

#[cfg(test)]
mod tests {
    use super::*;
    use check_updates_core::VersionSpec;
    use std::path::PathBuf;

    fn dep(name: &str, file: &str, section: Option<&str>) -> Dependency {
        Dependency {
            name: name.to_string(),
            version_spec: VersionSpec::Any,
            source_file: PathBuf::from(file),
            line_number: None,
            original_line: String::new(),
            manifest_key: None,
            section: section.map(str::to_string),
        }
    }

    #[test]
    fn conda_channel_dependencies_are_not_sent_to_pypi() {
        assert!(is_conda_channel_dependency(&dep(
            "pytorch",
            "/p/environment.yml",
            Some("dependencies")
        )));
        assert!(is_conda_channel_dependency(&dep(
            "python",
            "/p/environment.yaml",
            Some("dependencies")
        )));
    }

    #[test]
    fn pip_section_of_environment_yml_stays_on_pypi() {
        assert!(!is_conda_channel_dependency(&dep(
            "requests",
            "/p/environment.yml",
            Some("dependencies.pip")
        )));
    }

    #[test]
    fn other_files_are_untouched_even_with_a_dependencies_section() {
        assert!(!is_conda_channel_dependency(&dep(
            "requests",
            "/p/requirements.txt",
            None
        )));
        assert!(!is_conda_channel_dependency(&dep(
            "requests",
            "/p/pyproject.toml",
            Some("dependencies")
        )));
    }

    #[test]
    fn unchecked_conda_dependencies_appear_in_the_json_errors_array() {
        let conda = dep("pytorch", "/p/environment.yml", Some("dependencies"));
        let errors = errors_to_json(&[], &[&conda], &[]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["package"], "pytorch");
        assert_eq!(errors[0]["kind"], UNCHECKED_KIND);
    }
}
