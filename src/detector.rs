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
    pub fn detect(&self) -> anyhow::Result<Vec<DetectedFile>> {
        let mut detected_files = Vec::new();

        // Check for pyproject.toml and determine which package manager
        let pyproject_path = self.project_path.join("pyproject.toml");
        if pyproject_path.exists() {
            if let Some(pm) = self.detect_pyproject_manager(&pyproject_path)? {
                detected_files.push(DetectedFile {
                    path: pyproject_path,
                    package_manager: pm,
                });
            }
        }

        // Check for requirements*.txt files (pip)
        if let Ok(entries) = fs::read_dir(&self.project_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(filename) = path.file_name() {
                    let filename_str = filename.to_string_lossy();
                    if filename_str.starts_with("requirements") && filename_str.ends_with(".txt") {
                        detected_files.push(DetectedFile {
                            path: path.clone(),
                            package_manager: PackageManager::Pip,
                        });
                    }
                }
            }
        }

        // Check for conda environment files
        for filename in &["environment.yml", "environment.yaml"] {
            let conda_path = self.project_path.join(filename);
            if conda_path.exists() {
                detected_files.push(DetectedFile {
                    path: conda_path,
                    package_manager: PackageManager::Conda,
                });
            }
        }

        Ok(detected_files)
    }

    /// Detect which package manager uses pyproject.toml
    fn detect_pyproject_manager(&self, pyproject_path: &Path) -> anyhow::Result<Option<PackageManager>> {
        let contents = fs::read_to_string(pyproject_path)?;

        // Check for lock files to disambiguate
        let uv_lock = self.project_path.join("uv.lock");
        let poetry_lock = self.project_path.join("poetry.lock");
        let pdm_lock = self.project_path.join("pdm.lock");

        // Check for tool sections in pyproject.toml
        let has_poetry_section = contents.contains("[tool.poetry]");
        let has_pdm_section = contents.contains("[tool.pdm]");

        // Determine package manager based on lock files and tool sections
        if poetry_lock.exists() || has_poetry_section {
            Ok(Some(PackageManager::Poetry))
        } else if pdm_lock.exists() || has_pdm_section {
            Ok(Some(PackageManager::Pdm))
        } else if uv_lock.exists() {
            Ok(Some(PackageManager::Uv))
        } else {
            // If no lock file exists but pyproject.toml has dependencies,
            // default to uv (PEP 621 standard)
            if contents.contains("[project]") &&
               (contents.contains("dependencies") || contents.contains("[project.dependencies]")) {
                Ok(Some(PackageManager::Uv))
            } else {
                // No recognizable package manager
                Ok(None)
            }
        }
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

#[cfg(test)]
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
        assert!(detected.iter().all(|d| d.package_manager == PackageManager::Pip));
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

        fs::write(&pyproject_path, "[project]\nname = \"test\"\ndependencies = []\n").unwrap();
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

        fs::write(&pyproject_path, "[project]\nname = \"test\"\ndependencies = [\"requests\"]\n").unwrap();

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
        assert!(detected.iter().any(|d| d.package_manager == PackageManager::Pip));
        assert!(detected.iter().any(|d| d.package_manager == PackageManager::Conda));
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
    fn test_get_sync_command() {
        let temp_dir = TempDir::new().unwrap();
        let detector = ProjectDetector::new(temp_dir.path().to_path_buf());

        assert_eq!(detector.get_sync_command(&PackageManager::Pip), "pip install -r requirements.txt");
        assert_eq!(detector.get_sync_command(&PackageManager::Uv), "uv lock");
        assert_eq!(detector.get_sync_command(&PackageManager::Poetry), "poetry lock");
        assert_eq!(detector.get_sync_command(&PackageManager::Pdm), "pdm lock");
        assert_eq!(detector.get_sync_command(&PackageManager::Conda), "conda env update");
    }
}
