use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Helper to create a temporary project directory
pub struct TempProject {
    pub dir: TempDir,
}

impl TempProject {
    /// Create a new temporary project
    pub fn new() -> Self {
        let dir = TempDir::new().expect("Failed to create temp directory");
        Self { dir }
    }

    /// Get the path to the project directory
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Create a file in the project with the given content
    pub fn create_file(&self, relative_path: &str, content: &str) {
        let file_path = self.dir.path().join(relative_path);

        // Create parent directories if needed
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directories");
        }

        fs::write(&file_path, content).expect("Failed to write file");
    }

    /// Get the absolute path to a file in the project
    pub fn file_path(&self, relative_path: &str) -> PathBuf {
        self.dir.path().join(relative_path)
    }
}

impl Default for TempProject {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a sample requirements.txt content with various version specifiers
pub fn sample_requirements_txt() -> &'static str {
    r#"# Sample requirements.txt
requests>=2.28.0,<3.0.0
numpy==1.24.0
flask^2.0.0
Django~=4.0.0
pandas>=1.5.0
pytest==7.4.0
black>=23.0.0
mypy~=1.4.0
click>=8.0.0,<9.0.0
pydantic==2.0.0
"#
}

/// Create a sample requirements-dev.txt content
pub fn sample_requirements_dev_txt() -> &'static str {
    r#"# Development dependencies
pytest>=7.0.0
pytest-cov==4.1.0
black==23.7.0
mypy>=1.0.0
ruff>=0.0.280
"#
}

/// Create a sample pyproject.toml with PEP 621 dependencies
pub fn sample_pyproject_pep621() -> &'static str {
    r#"[project]
name = "test-project"
version = "0.1.0"
description = "A test project"
dependencies = [
    "requests>=2.28.0,<3.0.0",
    "numpy==1.24.0",
    "pandas>=1.5.0",
    "click>=8.0.0,<9.0.0",
]

[project.optional-dependencies]
dev = [
    "pytest>=7.0.0",
    "black==23.7.0",
    "mypy>=1.0.0",
]

[build-system]
requires = ["setuptools>=61.0"]
build-backend = "setuptools.build_meta"
"#
}

/// Create a sample pyproject.toml with Poetry dependencies
pub fn sample_pyproject_poetry() -> &'static str {
    r#"[tool.poetry]
name = "test-project"
version = "0.1.0"
description = "A test project"

[tool.poetry.dependencies]
python = "^3.9"
requests = "^2.28.0"
numpy = "1.24.0"
pandas = ">=1.5.0"
django = "~4.0.0"
flask = "^2.0.0"

[tool.poetry.group.dev.dependencies]
pytest = "^7.0.0"
black = "23.7.0"
mypy = "~1.4.0"
ruff = ">=0.0.280"

[build-system]
requires = ["poetry-core"]
build-backend = "poetry.core.masonry.api"
"#
}

/// Create a sample pyproject.toml with PDM dependencies
pub fn sample_pyproject_pdm() -> &'static str {
    r#"[project]
name = "test-project"
version = "0.1.0"
description = "A test project"
dependencies = [
    "requests>=2.28.0,<3.0.0",
    "numpy==1.24.0",
    "pandas>=1.5.0",
]

[tool.pdm.dev-dependencies]
dev = [
    "pytest>=7.0.0",
    "black==23.7.0",
]

[build-system]
requires = ["pdm-backend"]
build-backend = "pdm.backend"
"#
}

/// Create a sample environment.yml with Conda dependencies
pub fn sample_environment_yml() -> &'static str {
    r#"name: test-env
channels:
  - conda-forge
  - defaults
dependencies:
  - python=3.9
  - numpy=1.24.0
  - pandas>=1.5.0
  - requests>=2.28.0
  - pytest>=7.0.0
  - pip:
    - flask>=2.0.0
    - black==23.7.0
"#
}

/// Create a sample uv.lock file (simplified)
pub fn sample_uv_lock() -> &'static str {
    r#"version = 1

[[package]]
name = "requests"
version = "2.28.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "numpy"
version = "1.24.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "pandas"
version = "1.5.3"
source = { registry = "https://pypi.org/simple" }
"#
}

/// Create a sample poetry.lock file (simplified)
pub fn sample_poetry_lock() -> &'static str {
    r#"[[package]]
name = "requests"
version = "2.28.0"
description = "Python HTTP for Humans."
category = "main"
optional = false
python-versions = ">=3.7"

[[package]]
name = "numpy"
version = "1.24.0"
description = "Fundamental package for array computing in Python"
category = "main"
optional = false
python-versions = ">=3.8"

[metadata]
lock-version = "2.0"
python-versions = "^3.9"
content-hash = "abc123"
"#
}

/// Create a TempProject with a basic requirements.txt setup
pub fn create_temp_project_with_requirements() -> TempProject {
    let project = TempProject::new();
    project.create_file("requirements.txt", sample_requirements_txt());
    project
}

/// Create a TempProject with a PEP 621 pyproject.toml
pub fn create_temp_project_with_pep621() -> TempProject {
    let project = TempProject::new();
    project.create_file("pyproject.toml", sample_pyproject_pep621());
    project
}

/// Create a TempProject with a Poetry pyproject.toml
pub fn create_temp_project_with_poetry() -> TempProject {
    let project = TempProject::new();
    project.create_file("pyproject.toml", sample_pyproject_poetry());
    project.create_file("poetry.lock", sample_poetry_lock());
    project
}

/// Create a TempProject with a PDM pyproject.toml
pub fn create_temp_project_with_pdm() -> TempProject {
    let project = TempProject::new();
    project.create_file("pyproject.toml", sample_pyproject_pdm());
    project
}

/// Create a TempProject with a Conda environment.yml
pub fn create_temp_project_with_conda() -> TempProject {
    let project = TempProject::new();
    project.create_file("environment.yml", sample_environment_yml());
    project
}

/// Create a TempProject with multiple dependency files
pub fn create_temp_project_with_multiple_files() -> TempProject {
    let project = TempProject::new();
    project.create_file("requirements.txt", sample_requirements_txt());
    project.create_file("requirements-dev.txt", sample_requirements_dev_txt());
    project.create_file("pyproject.toml", sample_pyproject_pep621());
    project
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temp_project_creation() {
        let project = TempProject::new();
        assert!(project.path().exists());
        assert!(project.path().is_dir());
    }

    #[test]
    fn test_create_file() {
        let project = TempProject::new();
        project.create_file("test.txt", "hello world");

        let file_path = project.file_path("test.txt");
        assert!(file_path.exists());

        let content = fs::read_to_string(file_path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_create_file_with_subdirs() {
        let project = TempProject::new();
        project.create_file("subdir/test.txt", "hello");

        let file_path = project.file_path("subdir/test.txt");
        assert!(file_path.exists());
    }

    #[test]
    fn test_sample_fixtures_are_valid() {
        // Just ensure the fixtures parse as valid strings
        assert!(!sample_requirements_txt().is_empty());
        assert!(!sample_pyproject_pep621().is_empty());
        assert!(!sample_pyproject_poetry().is_empty());
        assert!(!sample_environment_yml().is_empty());
    }
}
