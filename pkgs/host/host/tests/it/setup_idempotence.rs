//! Idempotence test for config.yaml atomic write and hand-edit detection.
//!
//! Verifies that:
//! - Running setup twice with identical input produces byte-identical output.
//! - No file is re-written if the content is identical.
//! - The setup-digest file is created and allows hand-edit detection.

use jan_klod_host::config_write::{has_not_been_edited, write_config_atomically};
use std::fs;
use std::path::PathBuf;

fn temp_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jk-setup-idempotence-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("creates a temp dir");
    dir
}

#[test]
fn setup_is_idempotent() {
    let temp_dir = temp_test_dir("idempotent");
    let config_path = temp_dir.join("config.yaml");

    let config = serde_json::json!({
        "extensions": {
            "provider": {
                "openai": {
                    "enabled": true,
                    "api_key": "test-key"
                },
                "anthropic": {
                    "enabled": false,
                    "api_key": "another-key"
                }
            },
            "store": {
                "sqlite": {
                    "enabled": true,
                    "path": "/tmp/db"
                }
            }
        },
        "routing": {
            "primary": "openai",
            "fallback": null
        }
    });

    // First write
    write_config_atomically(&config_path, &config).expect("first write succeeds");
    let first_bytes = fs::read(&config_path).expect("can read first write");

    // Verify digest file exists
    let digest_path = temp_dir.join("config.yaml.setup-digest");
    assert!(
        digest_path.exists(),
        "digest file should exist after first write"
    );

    // File should not be marked as hand-edited
    assert!(
        has_not_been_edited(&config_path).expect("can check edit status"),
        "file should not be marked as hand-edited after setup"
    );

    // Second write with identical config
    write_config_atomically(&config_path, &config).expect("second write succeeds");
    let second_bytes = fs::read(&config_path).expect("can read second write");

    // Bytes should be identical
    assert_eq!(
        first_bytes, second_bytes,
        "setup should produce byte-identical output on identical input"
    );

    // File should still not be marked as hand-edited
    assert!(
        has_not_been_edited(&config_path).expect("can check edit status"),
        "file should still not be marked as hand-edited after second setup"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn setup_digest_detects_hand_edits() {
    let temp_dir = temp_test_dir("hand-edit-detection");
    let config_path = temp_dir.join("config.yaml");

    let config = serde_json::json!({
        "extensions": {
            "provider": {
                "openai": {
                    "enabled": true
                }
            }
        }
    });

    // First setup run
    write_config_atomically(&config_path, &config).expect("first write succeeds");

    // File should not be marked as hand-edited
    assert!(
        has_not_been_edited(&config_path).expect("can check edit status"),
        "file should not be marked as edited"
    );

    // Manually edit the config (simulate a hand-edit)
    fs::write(&config_path, b"manual: edit\n").expect("can write manual edit");

    // File should now be marked as hand-edited
    assert!(
        !has_not_been_edited(&config_path).expect("can check edit status"),
        "file should be marked as hand-edited after manual modification"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn setup_digest_file_format() {
    let temp_dir = temp_test_dir("digest-format");
    let config_path = temp_dir.join("config.yaml");

    let config = serde_json::json!({"test": "value"});

    write_config_atomically(&config_path, &config).expect("write succeeds");

    let digest_path = temp_dir.join("config.yaml.setup-digest");
    assert!(digest_path.exists(), "digest file should exist");

    let digest_content = fs::read_to_string(&digest_path).expect("can read digest file");

    // Digest should be exactly 64 hex characters (SHA256)
    assert_eq!(
        digest_content.len(),
        64,
        "digest should be 64 hex characters"
    );

    // All characters should be lowercase hex
    assert!(
        digest_content
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "digest should be lowercase hex"
    );

    // No trailing newline
    assert!(
        !digest_content.ends_with('\n'),
        "digest should have no trailing newline"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}
