# Test Suite

This directory contains the test infrastructure for `python-check-updates`.

## Structure

```
tests/
├── README.md           # This file
├── common/             # Shared test utilities
│   └── mod.rs          # Helper functions and fixture generators
├── fixtures/           # Sample dependency files for testing
│   ├── requirements.txt
│   ├── requirements-dev.txt
│   ├── pyproject-pep621.toml
│   ├── pyproject-poetry.toml
│   ├── pyproject-pdm.toml
│   ├── environment.yml
│   ├── uv.lock
│   └── poetry.lock
└── integration.rs      # Integration tests using assert_cmd
```

## Test Categories

### Integration Tests (`integration.rs`)

End-to-end tests that run the actual CLI binary:
- CLI flag tests (`--help`, `--version`, etc.)
- Project detection tests (requirements.txt, pyproject.toml, environment.yml)
- Update behavior tests
- Error handling tests

These tests use `assert_cmd` to invoke the binary and `predicates` to verify output.

### Common Module (`common/mod.rs`)

Helper utilities for all tests:

#### TempProject
A helper struct to create temporary test projects:
```rust
let project = TempProject::new();
project.create_file("requirements.txt", content);
```

#### Fixture Generators
Functions that return sample file contents:
- `sample_requirements_txt()` - Basic requirements.txt
- `sample_requirements_dev_txt()` - Dev dependencies
- `sample_pyproject_pep621()` - PEP 621 pyproject.toml
- `sample_pyproject_poetry()` - Poetry pyproject.toml
- `sample_pyproject_pdm()` - PDM pyproject.toml
- `sample_environment_yml()` - Conda environment.yml
- `sample_uv_lock()` - UV lock file
- `sample_poetry_lock()` - Poetry lock file

#### Project Builders
Convenience functions to create complete test projects:
- `create_temp_project_with_requirements()` - Project with requirements.txt
- `create_temp_project_with_pep621()` - Project with PEP 621 pyproject.toml
- `create_temp_project_with_poetry()` - Project with Poetry setup
- `create_temp_project_with_pdm()` - Project with PDM setup
- `create_temp_project_with_conda()` - Project with Conda environment
- `create_temp_project_with_multiple_files()` - Project with multiple dependency files

### Fixtures Directory

Static sample files that can be used directly in tests:
- `requirements.txt` - Sample pip requirements with various version specifiers
- `requirements-dev.txt` - Development dependencies
- `pyproject-pep621.toml` - PEP 621 style pyproject.toml
- `pyproject-poetry.toml` - Poetry style pyproject.toml
- `pyproject-pdm.toml` - PDM style pyproject.toml
- `environment.yml` - Conda environment specification
- `uv.lock` - UV lock file
- `poetry.lock` - Poetry lock file

## Running Tests

Run all tests:
```bash
cargo test
```

Run only integration tests:
```bash
cargo test --test integration
```

Run a specific test:
```bash
cargo test test_help_flag
```

Run tests with output:
```bash
cargo test -- --nocapture
```

## Current Test Status

The current test suite focuses on:
1. **CLI interface validation** - Ensures all flags work correctly
2. **Project detection** - Verifies the tool can find and identify dependency files
3. **Basic functionality** - Tests that the tool runs without errors

### Note on PyPI Testing

The current tests do **not** mock PyPI responses. Tests that run against real projects will:
- Skip verification of actual version checking
- Focus on file detection and parsing
- Verify the tool runs without crashing

Future improvements will add:
- PyPI mocking using `wiremock`
- Verification of version comparison logic
- Output format validation
- File update verification

## Adding New Tests

### Adding an Integration Test

1. Import the common module: `use crate::common;`
2. Create a test project using helpers
3. Run the CLI with `Command::cargo_bin("python-check-updates")`
4. Use `assert_cmd` assertions to verify behavior

Example:
```rust
#[test]
fn test_my_feature() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("python-check-updates").unwrap();
    cmd.arg(project.path())
        .arg("--my-flag")
        .assert()
        .success();
}
```

### Adding a Fixture Generator

Add a new function to `common/mod.rs`:
```rust
pub fn sample_my_file() -> &'static str {
    r#"content here"#
}
```

### Adding a Static Fixture

Add a new file to `tests/fixtures/` with your sample content.

## Dependencies

Test dependencies (from `Cargo.toml`):
- `tempfile` - Create temporary directories for test projects
- `assert_cmd` - Test CLI applications
- `predicates` - Assertions for command output
- `tokio-test` - Async testing utilities
- `wiremock` - HTTP mocking (for future PyPI mocking)
