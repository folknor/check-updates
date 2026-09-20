use std::fs;
use std::path::{Path, PathBuf};

/// Detected package manager type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Pip,
    Uv,
    Poetry,
    Pdm,
    Conda,
}

impl std::fmt::Display for PackageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageManager::Pip => write!(f, "pip"),
            PackageManager::Uv => write!(f, "uv"),
            PackageManager::Poetry => write!(f, "poetry"),
            PackageManager::Pdm => write!(f, "pdm"),
            PackageManager::Conda => write!(f, "conda"),
        }
    }
}

/// Information about detected dependency files
#[derive(Debug, Clone)]
pub struct DetectedFile {
    pub path: PathBuf,
    pub package_manager: PackageManager,
}

/// Detects package managers and dependency files in a project
pub struct ProjectDetector {
    project_path: PathBuf,
}

impl ProjectDetector {
    pub fn new(project_path: PathBuf) -> Self {
        Self { project_path }
    }

    /// Detect all dependency files in the project
    ///
    /// Discovery is deliberately non-recursive: it looks only at the top level
    /// of `project_path`. See the note on `detect` in the module tail for why a
    /// recursive walk is not a local change.
    pub fn detect(&self) -> anyhow::Result<Vec<DetectedFile>> {
        let mut detected_files = Vec::new();

        // pyproject.toml is always parsed when it exists. The package manager
        // is only a *label* used for post-update sync advice; it must never
        // decide whether the file's dependencies get read at all. A file with
        // only `[dependency-groups]`, or a plain setuptools project, still has
        // dependencies worth reporting.
        let pyproject_path = self.project_path.join("pyproject.toml");
        if pyproject_path.is_file() {
            match classify_pyproject(&pyproject_path) {
                Ok(pm) => detected_files.push(DetectedFile {
                    path: pyproject_path,
                    package_manager: pm,
                }),
                // Failure policy: an unreadable individual file is a warning,
                // not an abort. Previously the `?` here killed the whole run
                // while `read_dir` errors below were swallowed silently.
                Err(err) => {
                    eprintln!(
                        "warning: could not read {}: {err}",
                        pyproject_path.display()
                    );
                }
            }
        }

        // requirements*.txt (pip). `read_dir` yields entries in unspecified
        // order, so collect and sort: the output table order and which
        // duplicate definition wins downstream must not vary between runs.
        match fs::read_dir(&self.project_path) {
            Ok(entries) => {
                let mut requirement_files: Vec<PathBuf> = Vec::new();
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Some(filename) = path.file_name() else {
                        continue;
                    };
                    let filename_str = filename.to_string_lossy();
                    if filename_str.starts_with("requirements")
                        && filename_str.ends_with(".txt")
                        && path.is_file()
                    {
                        requirement_files.push(path);
                    }
                }
                requirement_files.sort();
                for path in requirement_files {
                    detected_files.push(DetectedFile {
                        path,
                        package_manager: PackageManager::Pip,
                    });
                }
            }
            Err(err) => {
                eprintln!(
                    "warning: could not list {}: {err}",
                    self.project_path.display()
                );
            }
        }

        // Conda environment files.
        for filename in &["environment.yml", "environment.yaml"] {
            let conda_path = self.project_path.join(filename);
            if conda_path.is_file() {
                detected_files.push(DetectedFile {
                    path: conda_path,
                    package_manager: PackageManager::Conda,
                });
            }
        }

        Ok(detected_files)
    }

    /// Get the sync command to run after updating
    pub fn get_sync_command(&self, pm: &PackageManager) -> &'static str {
        match pm {
            PackageManager::Pip => "pip install -r requirements.txt",
            PackageManager::Uv => "uv lock",
            PackageManager::Poetry => "poetry lock",
            PackageManager::Pdm => "pdm lock",
            PackageManager::Conda => "conda env update",
        }
    }
}

/// Label the package manager that owns a `pyproject.toml`.
///
/// This is a *label*, never a gate: every `pyproject.toml` is parsed for
/// dependencies regardless of what this returns. The label drives the
/// post-update sync advice ("Run `poetry lock` ..."), so it must be right for
/// that purpose and nothing more.
///
/// Tool tables are read through a real TOML parse rather than substring search,
/// so `[tool.poetry.dependencies]` with no bare `[tool.poetry]` header counts,
/// `[tool.pdm.dev-dependencies]` counts, and the words `[tool.poetry]` inside a
/// comment or a string do not.
///
/// Precedence: an explicit tool table beats a lock file, because the lock file
/// may be a leftover from a manager the project has since migrated away from.
///
/// `Err` is returned only when the file cannot be read; a *syntactically*
/// invalid file still gets a label, since the dependency parser reports the
/// syntax error with better context than the detector could.
pub fn classify_pyproject(pyproject_path: &Path) -> anyhow::Result<PackageManager> {
    let contents = fs::read_to_string(pyproject_path)?;
    let dir = pyproject_path.parent().unwrap_or(Path::new("."));

    // Note: `contents.parse::<toml::Value>()` is NOT the same thing as parsing a
    // document - toml 1.x implements `FromStr for Value` in terms of the *value*
    // deserializer, so a whole manifest parses as a bare value (and
    // `[tool.poetry]` comes back as an array). Deserialize a `toml::Table`.
    let tool_table = toml::from_str::<toml::Table>(&contents)
        .ok()
        .and_then(|doc| doc.get("tool").and_then(toml::Value::as_table).cloned());

    if let Some(tool) = &tool_table {
        if tool.contains_key("poetry") {
            return Ok(PackageManager::Poetry);
        }
        if tool.contains_key("pdm") {
            return Ok(PackageManager::Pdm);
        }
        if tool.contains_key("uv") {
            return Ok(PackageManager::Uv);
        }
    }

    if dir.join("poetry.lock").is_file() {
        return Ok(PackageManager::Poetry);
    }
    if dir.join("pdm.lock").is_file() {
        return Ok(PackageManager::Pdm);
    }
    if dir.join("uv.lock").is_file() {
        return Ok(PackageManager::Uv);
    }

    // Nothing declares an owner. PEP 621 projects are most commonly driven by
    // uv today, and `uv lock` is the least destructive of the candidate
    // suggestions, so it stays the fallback. A dedicated "unknown / PEP 621"
    // label would be more honest, but adding an enum variant means revisiting
    // every exhaustive match on `PackageManager`, including the sync-advice
    // tables in `pcu/src/updater.rs`.
    Ok(PackageManager::Uv)
}

// Recursive discovery: deliberately NOT implemented here.
//
// Making `detect` walk subdirectories is not a detector-local change. pcu
// resolves installed versions from a single project-root lock file
// (`LockfileParser::find_and_parse(&project_path)` in `main.rs`) and prints one
// set of sync commands for the whole run; nested `pyproject.toml` files belong
// to sibling distributions with their own lock files and their own managers, so
// discovering them would merge unrelated dependency sets into one table and
// resolve them against the wrong lock. A recursive walk also needs an exclusion
// policy (`.venv`, `site-packages`, `node_modules`, `.git`, build trees) and a
// depth bound, or a single run over a repo with vendored environments detects
// hundreds of files. This
// one needs a design decision about what "the project" means for pcu, not a
// patch.

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_detect_pip_requirements() {
        let temp_dir = TempDir::new().unwrap();
        let req_path = temp_dir.path().join("requirements.txt");
        fs::write(&req_path, "requests==2.28.0\n").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Pip);
        assert_eq!(detected[0].path, req_path);
    }

    #[test]
    fn test_detect_multiple_requirements() {
        let temp_dir = TempDir::new().unwrap();
        let req_path = temp_dir.path().join("requirements.txt");
        let req_dev_path = temp_dir.path().join("requirements-dev.txt");
        fs::write(&req_path, "requests==2.28.0\n").unwrap();
        fs::write(&req_dev_path, "pytest==7.0.0\n").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 2);
        assert!(
            detected
                .iter()
                .all(|d| d.package_manager == PackageManager::Pip)
        );
    }

    #[test]
    fn test_detect_poetry() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        let poetry_lock_path = temp_dir.path().join("poetry.lock");

        fs::write(&pyproject_path, "[tool.poetry]\nname = \"test\"\n").unwrap();
        fs::write(&poetry_lock_path, "").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Poetry);
        assert_eq!(detected[0].path, pyproject_path);
    }

    #[test]
    fn test_detect_poetry_without_lock() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");

        fs::write(&pyproject_path, "[tool.poetry]\nname = \"test\"\n").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Poetry);
    }

    #[test]
    fn test_detect_pdm() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        let pdm_lock_path = temp_dir.path().join("pdm.lock");

        fs::write(&pyproject_path, "[tool.pdm]\n").unwrap();
        fs::write(&pdm_lock_path, "").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Pdm);
    }

    #[test]
    fn test_detect_uv() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        let uv_lock_path = temp_dir.path().join("uv.lock");

        fs::write(
            &pyproject_path,
            "[project]\nname = \"test\"\ndependencies = []\n",
        )
        .unwrap();
        fs::write(&uv_lock_path, "").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Uv);
    }

    #[test]
    fn test_detect_uv_without_lock() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");

        fs::write(
            &pyproject_path,
            "[project]\nname = \"test\"\ndependencies = [\"requests\"]\n",
        )
        .unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Uv);
    }

    #[test]
    fn test_detect_conda_yml() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join("environment.yml");

        fs::write(&env_path, "name: test\n").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Conda);
        assert_eq!(detected[0].path, env_path);
    }

    #[test]
    fn test_detect_conda_yaml() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join("environment.yaml");

        fs::write(&env_path, "name: test\n").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Conda);
    }

    #[test]
    fn test_detect_mixed_project() {
        let temp_dir = TempDir::new().unwrap();
        let req_path = temp_dir.path().join("requirements.txt");
        let env_path = temp_dir.path().join("environment.yml");

        fs::write(&req_path, "requests==2.28.0\n").unwrap();
        fs::write(&env_path, "name: test\n").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 2);
        assert!(
            detected
                .iter()
                .any(|d| d.package_manager == PackageManager::Pip)
        );
        assert!(
            detected
                .iter()
                .any(|d| d.package_manager == PackageManager::Conda)
        );
    }

    #[test]
    fn test_priority_poetry_over_others() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        let poetry_lock_path = temp_dir.path().join("poetry.lock");
        let uv_lock_path = temp_dir.path().join("uv.lock");

        // Even if both locks exist, poetry.lock takes priority if [tool.poetry] exists
        fs::write(&pyproject_path, "[tool.poetry]\nname = \"test\"\n").unwrap();
        fs::write(&poetry_lock_path, "").unwrap();
        fs::write(&uv_lock_path, "").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].package_manager, PackageManager::Poetry);
    }

    #[test]
    fn test_pyproject_without_recognizable_manager_is_still_detected() {
        // A dependency-groups-only file has no [project] and no tool
        // table. It must still reach the parser.
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_path, "[dependency-groups]\ndev = [\"pytest\"]\n").unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let detected = detector.detect().unwrap();

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].path, pyproject_path);
    }

    #[test]
    fn test_poetry_detected_from_subtable_only() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_path,
            "[tool.poetry.dependencies]\nrequests = \"^2.28\"\n",
        )
        .unwrap();

        assert_eq!(
            classify_pyproject(&pyproject_path).unwrap(),
            PackageManager::Poetry
        );
    }

    #[test]
    fn test_pdm_detected_from_dev_dependencies_only() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_path,
            "[tool.pdm.dev-dependencies]\ntest = [\"pytest\"]\n",
        )
        .unwrap();

        assert_eq!(
            classify_pyproject(&pyproject_path).unwrap(),
            PackageManager::Pdm
        );
    }

    #[test]
    fn test_poetry_mention_in_comment_is_not_poetry() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_path,
            "# migrated away from [tool.poetry]\n[project]\nname = \"x\"\ndependencies = []\n",
        )
        .unwrap();

        assert_eq!(
            classify_pyproject(&pyproject_path).unwrap(),
            PackageManager::Uv
        );
    }

    #[test]
    fn test_requirements_order_is_deterministic() {
        let temp_dir = TempDir::new().unwrap();
        for name in [
            "requirements-dev.txt",
            "requirements.txt",
            "requirements-a.txt",
        ] {
            fs::write(temp_dir.path().join(name), "requests\n").unwrap();
        }

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        let names: Vec<String> = detector
            .detect()
            .unwrap()
            .iter()
            .map(|d| d.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            names,
            vec![
                "requirements-a.txt".to_string(),
                "requirements-dev.txt".to_string(),
                "requirements.txt".to_string(),
            ]
        );
    }

    #[test]
    fn test_directory_named_like_requirements_is_ignored() {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir(temp_dir.path().join("requirements-x.txt")).unwrap();

        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());
        assert!(detector.detect().unwrap().is_empty());
    }

    #[test]
    fn test_tool_table_beats_stale_lock_file() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_path, "[tool.uv]\n[project]\nname = \"x\"\n").unwrap();
        fs::write(temp_dir.path().join("poetry.lock"), "").unwrap();

        assert_eq!(
            classify_pyproject(&pyproject_path).unwrap(),
            PackageManager::Uv
        );
    }

    #[test]
    fn test_get_sync_command() {
        let temp_dir = TempDir::new().unwrap();
        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());

        assert_eq!(
            detector.get_sync_command(&PackageManager::Pip),
            "pip install -r requirements.txt"
        );
        assert_eq!(detector.get_sync_command(&PackageManager::Uv), "uv lock");
        assert_eq!(
            detector.get_sync_command(&PackageManager::Poetry),
            "poetry lock"
        );
        assert_eq!(detector.get_sync_command(&PackageManager::Pdm), "pdm lock");
        assert_eq!(
            detector.get_sync_command(&PackageManager::Conda),
            "conda env update"
        );
    }
}
