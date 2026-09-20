//! Atomic write of `config.yaml` with hand-edit detection.
//!
//! This module provides mechanisms to:
//! - Write `config.yaml` atomically using a temp file + rename pattern
//! - Store a SHA256 digest of the written config to detect hand-edits
//! - Check if the current `config.yaml` has been modified since the last setup run

use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Error writing or verifying config files.
#[derive(Debug)]
pub enum ConfigWriteError {
    /// Failed to read the config file.
    Read {
        /// Path to the file that failed to read.
        path: String,
        /// IO error details.
        source: io::Error,
    },
    /// Failed to write the config file.
    Write {
        /// Path to the file that failed to write.
        path: String,
        /// IO error details.
        source: io::Error,
    },
    /// Failed to serialize config to YAML.
    Serialize(serde_yaml_ng::Error),
    /// The current config file has been hand-edited (digest doesn't match).
    HandEditDetected {
        /// Path to the config file.
        path: String,
        /// Expected digest from setup-digest file.
        expected_digest: String,
        /// Actual digest of the current file.
        actual_digest: String,
    },
    /// Failed to read or write the digest file.
    Digest {
        /// Path to the digest file.
        path: String,
        /// IO error details.
        source: io::Error,
    },
}

impl std::fmt::Display for ConfigWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "failed to read {path}: {source}"),
            Self::Write { path, source } => write!(f, "failed to write {path}: {source}"),
            Self::Serialize(e) => write!(f, "failed to serialize config: {e}"),
            Self::HandEditDetected {
                path,
                expected_digest,
                actual_digest,
            } => write!(
                f,
                "hand-edit detected in {path} (digest mismatch: expected {expected_digest}, got {actual_digest})"
            ),
            Self::Digest { path, source } => write!(f, "failed to read/write digest {path}: {source}"),
        }
    }
}

impl std::error::Error for ConfigWriteError {}

/// Calculate the SHA256 digest of a byte string.
///
/// Returns lowercase hex string, no trailing newline.
fn calculate_digest(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    format!("{digest:x}")
}

/// Digest file name (beside `config.yaml`).
fn digest_path(config_path: &Path) -> PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let filename = config_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("config.yaml"));
    parent.join(format!("{}.setup-digest", filename.to_string_lossy()))
}

/// Temp file name (beside `config.yaml`).
fn temp_path(config_path: &Path) -> PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let filename = config_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("config.yaml"));
    parent.join(format!(".{}.tmp", filename.to_string_lossy()))
}

/// Check if the config file has been hand-edited since the last setup run.
///
/// Returns `Ok(true)` if the file has not been edited (digest matches).
/// Returns `Ok(false)` if the file has been edited or no digest exists.
///
/// # Errors
///
/// Returns error if reading the config file or digest file fails.
pub fn has_not_been_edited(config_path: &Path) -> Result<bool, ConfigWriteError> {
    let dpath = digest_path(config_path);

    // If digest file doesn't exist, the file has been edited (or setup never ran).
    if !dpath.exists() {
        return Ok(false);
    }

    // Read the stored digest.
    let stored_digest = fs::read_to_string(&dpath).map_err(|source| ConfigWriteError::Digest {
        path: dpath.display().to_string(),
        source,
    })?;
    let stored_digest = stored_digest.trim();

    // Read and digest the current config file.
    let current_bytes = fs::read(config_path).map_err(|source| ConfigWriteError::Read {
        path: config_path.display().to_string(),
        source,
    })?;
    let current_digest = calculate_digest(&current_bytes);

    // Compare.
    Ok(stored_digest == current_digest)
}

/// Atomically write config YAML to disk with hand-edit detection.
///
/// This function:
/// 1. Serializes the config to YAML
/// 2. Writes to a temp file `.config.yaml.tmp` in the same directory
/// 3. Calculates SHA256 digest of the YAML bytes
/// 4. Atomically renames the temp file to `config.yaml`
/// 5. Writes the digest to `config.yaml.setup-digest`
///
/// If any step fails, `config.yaml` is left unchanged.
///
/// # Errors
///
/// Returns error if serialization, file write, rename, or digest write fails.
pub fn write_config_atomically(
    config_path: &Path,
    config: &serde_json::Value,
) -> Result<(), ConfigWriteError> {
    // Serialize to YAML. serde_yaml_ng uses IndexMap, ensuring deterministic key order.
    let yaml_bytes = serde_yaml_ng::to_string(config)
        .map_err(ConfigWriteError::Serialize)?
        .into_bytes();

    let tpath = temp_path(config_path);

    // Write to temp file.
    fs::write(&tpath, &yaml_bytes).map_err(|source| ConfigWriteError::Write {
        path: tpath.display().to_string(),
        source,
    })?;

    // Calculate digest before rename (of the bytes that will be in place).
    let digest = calculate_digest(&yaml_bytes);

    // Atomic rename: if this fails, temp file is left and config.yaml is unchanged.
    fs::rename(&tpath, config_path).map_err(|source| {
        // Clean up the temp file if rename fails.
        let _ = fs::remove_file(&tpath);
        ConfigWriteError::Write {
            path: config_path.display().to_string(),
            source,
        }
    })?;

    // Write digest file (best effort; if this fails, the config is already written).
    let dpath = digest_path(config_path);
    fs::write(&dpath, &digest).map_err(|source| ConfigWriteError::Digest {
        path: dpath.display().to_string(),
        source,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jk-config-write-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("creates a temp dir");
        dir
    }

    #[test]
    fn test_calculate_digest_is_deterministic() {
        let content = b"test content";
        let digest1 = calculate_digest(content);
        let digest2 = calculate_digest(content);
        assert_eq!(digest1, digest2);
    }

    #[test]
    fn test_digest_format() {
        let content = b"test";
        let digest = calculate_digest(content);
        // Should be lowercase hex, exactly 64 chars for SHA256
        assert_eq!(digest.len(), 64);
        assert!(digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn test_write_and_check_hand_edit() {
        let temp_dir = temp_test_dir("write-and-check");
        let config_path = temp_dir.join("config.yaml");

        let config = serde_json::json!({
            "extensions": {
                "provider": {
                    "openai": {
                        "enabled": true,
                        "api_key": "test"
                    }
                }
            }
        });

        // Write the config
        write_config_atomically(&config_path, &config).unwrap();

        // Should not be marked as hand-edited
        assert!(has_not_been_edited(&config_path).unwrap());

        // Manually edit the config
        fs::write(&config_path, b"manual edit").unwrap();

        // Should now be marked as hand-edited
        assert!(!has_not_been_edited(&config_path).unwrap());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_idempotent_write() {
        let temp_dir = temp_test_dir("idempotent-write");
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

        // Write once
        write_config_atomically(&config_path, &config).unwrap();
        let first_bytes = fs::read(&config_path).unwrap();

        // Write again with same config
        write_config_atomically(&config_path, &config).unwrap();
        let second_bytes = fs::read(&config_path).unwrap();

        // Should be byte-identical
        assert_eq!(first_bytes, second_bytes);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_digest_file_created() {
        let temp_dir = temp_test_dir("digest-file");
        let config_path = temp_dir.join("config.yaml");

        let config = serde_json::json!({"test": "value"});
        write_config_atomically(&config_path, &config).unwrap();

        let dpath = digest_path(&config_path);
        assert!(dpath.exists());

        let digest_content = fs::read_to_string(&dpath).unwrap();
        assert_eq!(digest_content.len(), 64); // SHA256 hex
        assert!(!digest_content.ends_with('\n')); // No trailing newline

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
