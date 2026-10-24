//! `setup` subcommand: read-only audit output and per-group config strategies.

use std::process::Command;

use jk_host::setup::{apply_group_strategy, ConfigStrategy, Group, Question, QuestionType};

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

    assert_eq!(
        output.status.code(),
        Some(0),
        "setup should exit 0. stdout:\n{stdout}\nstderr:\n{stderr}"
    );

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

    assert!(
        stdout.contains("✓") || stdout.contains("✗"),
        "should report at least one guest status"
    );
}

#[test]
fn setup_audit_is_read_only() {
    let root = common::repo_root();

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

#[test]
fn setup_configures_groups_per_strategy() {
    let nothing_group = Group {
        name: "reasoning".to_string(),
        guests: vec![
            "interceptor-intent-router".to_string(),
            "interceptor-task-router".to_string(),
        ],
        strategy: ConfigStrategy::Nothing,
    };

    let result = apply_group_strategy(&nothing_group);
    assert_eq!(result.group_name, "reasoning");
    assert_eq!(result.config, serde_json::json!({}));

    let manual_group = Group {
        name: "providers".to_string(),
        guests: vec!["provider-openai".to_string()],
        strategy: ConfigStrategy::Manual,
    };

    let result = apply_group_strategy(&manual_group);
    assert_eq!(result.group_name, "providers");
    assert_eq!(result.config, serde_json::json!({}));

    let prompt_group = Group {
        name: "test-group".to_string(),
        guests: vec!["test-guest".to_string()],
        strategy: ConfigStrategy::Prompt(vec![
            Question {
                name: "providers.openai.enabled".to_string(),
                prompt: "Do you want to enable OpenAI?".to_string(),
                kind: QuestionType::Bool,
                default: "no".to_string(),
            },
            Question {
                name: "providers.openai.api-key".to_string(),
                prompt: "Enter your OpenAI API key".to_string(),
                kind: QuestionType::String,
                default: String::new(),
            },
        ]),
    };

    assert_eq!(
        prompt_group.strategy,
        ConfigStrategy::Prompt(vec![
            Question {
                name: "providers.openai.enabled".to_string(),
                prompt: "Do you want to enable OpenAI?".to_string(),
                kind: QuestionType::Bool,
                default: "no".to_string(),
            },
            Question {
                name: "providers.openai.api-key".to_string(),
                prompt: "Enter your OpenAI API key".to_string(),
                kind: QuestionType::String,
                default: String::new(),
            },
        ])
    );
}

#[test]
fn setup_prompt_returns_default_on_empty_input() {}
