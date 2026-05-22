#![allow(clippy::unwrap_used)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

/// Test that --help flag works
#[test]
fn test_help_flag() {
    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Check for outdated Python dependencies"))
        .stdout(predicate::str::contains("--update"))
        .stdout(predicate::str::contains("--minor"))
        .stdout(predicate::str::contains("--force"))
        .stdout(predicate::str::contains("--pre-release"));
}

/// Test that -h short flag works
#[test]
fn test_help_short_flag() {
    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Check for outdated Python dependencies"));
}

/// Test that --version flag works
#[test]
fn test_version_flag() {
    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("pcu"));
}

/// Test that -V short version flag works
#[test]
fn test_version_short_flag() {
    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg("-V")
        .assert()
        .success()
        .stdout(predicate::str::contains("pcu"));
}

/// Test running on a project with requirements.txt
#[test]
fn test_detect_requirements_txt() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .assert()
        .success();

    // Just verify it runs without error for now
    // Later we'll add mocking to verify output
}

/// Test running on a project with PEP 621 pyproject.toml
#[test]
fn test_detect_pep621_pyproject() {
    let project = common::create_temp_project_with_pep621();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .assert()
        .success();
}

/// Test running on a project with Poetry pyproject.toml
#[test]
fn test_detect_poetry_pyproject() {
    let project = common::create_temp_project_with_poetry();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .assert()
        .success();
}

/// Test running on a project with PDM pyproject.toml
#[test]
fn test_detect_pdm_pyproject() {
    let project = common::create_temp_project_with_pdm();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .assert()
        .success();
}

/// Test running on a project with conda environment.yml
#[test]
fn test_detect_conda_environment() {
    let project = common::create_temp_project_with_conda();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .assert()
        .success();
}

/// Test running on a project with multiple dependency files
#[test]
fn test_detect_multiple_files() {
    let project = common::create_temp_project_with_multiple_files();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .assert()
        .success();
}

/// Test running on an empty project (no dependency files)
#[test]
fn test_empty_project() {
    let project = common::TempProject::new();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    // Should still succeed but indicate no files found
    cmd.arg(project.path()).assert().success();
}

/// Test running with --update flag (dry-run for now)
#[test]
fn test_update_flag() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .arg("--update")
        .assert()
        .success();
}

/// Test running with --minor flag
#[test]
fn test_minor_flag() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .arg("--minor")
        .assert()
        .success();
}

/// Test running with --force flag
#[test]
fn test_force_flag() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .arg("--force")
        .assert()
        .success();
}

/// Test running with --pre-release flag
#[test]
fn test_pre_release_flag() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .arg("--pre-release")
        .assert()
        .success();
}

/// Test running with combined flags
#[test]
fn test_combined_flags() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path())
        .arg("--update")
        .arg("--minor")
        .arg("--pre-release")
        .assert()
        .success();
}

/// Test that files are not modified without --update flag
#[test]
fn test_no_modification_without_update() {
    let project = common::create_temp_project_with_requirements();
    let req_path = project.file_path("requirements.txt");

    let original_content = fs::read_to_string(&req_path).unwrap();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg(project.path()).assert().success();

    // File should not be modified
    let current_content = fs::read_to_string(&req_path).unwrap();
    assert_eq!(original_content, current_content);
}

/// Test running on non-existent directory
#[test]
fn test_nonexistent_directory() {
    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.arg("/nonexistent/path/to/project")
        .assert()
        .failure();
}

/// Test that current directory is used when no path is provided
#[test]
fn test_default_to_current_directory() {
    let project = common::create_temp_project_with_requirements();

    let mut cmd = Command::cargo_bin("pcu").unwrap();
    cmd.current_dir(project.path())
        .assert()
        .success();
}

/// `--json` on an empty project emits a valid JSON envelope.
#[test]
fn test_json_empty_project_envelope() {
    let project = common::TempProject::new();

    let output = Command::cargo_bin("pcu")
        .unwrap()
        .arg(project.path())
        .arg("--json")
        .output()
        .unwrap();

    assert!(output.status.success(), "pcu --json should succeed on empty project");

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["tool"], "pcu");
    assert_eq!(value["mode"], "project");
    assert!(value["checks"].is_array(), "checks should be an array");
    assert_eq!(value["checks"].as_array().unwrap().len(), 0);
    assert!(value["errors"].is_array(), "errors should be an array");
}

/// `--json` on a real project emits envelope with parseable checks.
/// Hits PyPI, so we only assert on shape - never on specific versions.
#[test]
fn test_json_project_mode_envelope() {
    let project = common::create_temp_project_with_requirements();

    let output = Command::cargo_bin("pcu")
        .unwrap()
        .arg(project.path())
        .arg("--json")
        .output()
        .unwrap();

    assert!(output.status.success(), "pcu --json should succeed");

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["tool"], "pcu");
    assert_eq!(value["mode"], "project");

    let checks = value["checks"].as_array().expect("checks should be array");
    if let Some(first) = checks.first() {
        // Every check must have at minimum a dependency object with name + spec.
        assert!(first["dependency"]["name"].is_string());
        assert!(first["dependency"]["spec"].is_string());
        assert!(first["latest"].is_string() || first["latest"].is_null());
    }
}
