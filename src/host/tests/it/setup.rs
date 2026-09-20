//! `setup` subcommand: audit the environment and distribution without writing files.
//!
//! Tests the audit output for the coding distribution, verifying that it reports
//! on guests, config keys, providers, sandbox, MCP servers, and registry trust.

use std::process::Command;

use crate::common;

#[test]
fn setup_audit_for_coding_distribution_shows_all_sections() {
    let root = common::repo_root();

    let output = Command::new(env!("CARGO_BIN_EXE_jan-klod-gateway"))
        .arg("setup")
        .arg("coding")
        .current_dir(&root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .expect("setup command runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Exit code should be 0 (success: audit completed)
    assert_eq!(
        output.status.code(),
        Some(0),
        "setup should exit 0. stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Check that all expected sections are present in the output
    assert!(
        stdout.contains("Setup Audit for Distribution: coding"),
        "should show distribution name"
    );
    assert!(stdout.contains("Guests:"), "should have Guests section");
    assert!(
        stdout.contains("Configuration Keys:"),
        "should have Configuration Keys section"
    );
    assert!(
        stdout.contains("Provider API Keys:"),
        "should have Provider API Keys section"
    );
    assert!(
        stdout.contains("Sandbox Capability:"),
        "should have Sandbox Capability section"
    );
    assert!(
        stdout.contains("Registry Trust:"),
        "should have Registry Trust section"
    );
    assert!(
        stdout.contains("MCP Servers:"),
        "should have MCP Servers section"
    );
}

#[test]
fn setup_audit_with_no_arguments_defaults_to_coding() {
    let root = common::repo_root();

    // Create a temp config.yaml in the repo to avoid the default resolution
    let temp_config = root.join("config.yaml");
    let _exists_before = temp_config.exists();

    let output = Command::new(env!("CARGO_BIN_EXE_jan-klod-gateway"))
        .arg("setup")
        .current_dir(&root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .expect("setup command runs");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should default to coding distribution
    assert!(
        stdout.contains("coding"),
        "should default to coding distribution"
    );
}

#[test]
fn setup_audit_reports_missing_guests() {
    let root = common::repo_root();

    let output = Command::new(env!("CARGO_BIN_EXE_jan-klod-gateway"))
        .arg("setup")
        .arg("coding")
        .current_dir(&root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .expect("setup command runs");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The coding distribution lists guests; at minimum some should be reported
    // (either present with ✓ or missing with ✗)
    assert!(
        stdout.contains("✓") || stdout.contains("✗"),
        "should report at least one guest status"
    );
}

#[test]
fn setup_audit_is_read_only() {
    let root = common::repo_root();

    // Create a temp config to check it's not modified
    let temp_config = root.join("config.yaml");
    let config_mtime_before = if temp_config.exists() {
        std::fs::metadata(&temp_config)
            .ok()
            .and_then(|m| m.modified().ok())
    } else {
        None
    };

    let output = Command::new(env!("CARGO_BIN_EXE_jan-klod-gateway"))
        .arg("setup")
        .arg("coding")
        .current_dir(&root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .expect("setup command runs");

    assert_eq!(output.status.code(), Some(0), "setup should exit 0");

    // Verify config.yaml wasn't modified
    let config_mtime_after = if temp_config.exists() {
        std::fs::metadata(&temp_config)
            .ok()
            .and_then(|m| m.modified().ok())
    } else {
        None
    };

    assert_eq!(
        config_mtime_before, config_mtime_after,
        "setup should not modify config.yaml"
    );
}
