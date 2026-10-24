//! What a component declares it needs; undeclared imports are refused, over-declaring is not (capabilities are default-deny).

use std::path::{Path, PathBuf};

/// The `jan-klod:interfaces` package version this build speaks; a constant because an installed gateway has no `wit/`.
pub const API_VERSION: &str = "0.4.0";

/// Whether `declared` can run on `host`: same major, and the same minor while major is `0`; unparseable is incompatible.
#[must_use]
pub fn api_compatible(host: &str, declared: &str) -> bool {
    let parts = |v: &str| -> Option<(u64, u64)> {
        let mut it = v.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        Some((major, minor))
    };
    match (parts(host), parts(declared)) {
        (Some((host_major, host_minor)), Some((their_major, their_minor))) => {
            host_major == their_major && (host_major != 0 || host_minor == their_minor)
        }
        _ => false,
    }
}

/// What a component's manifest declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Component name, as the file it describes is named.
    pub name: String,
    /// The component's own version.
    pub version: String,
    /// The `jan-klod:interfaces` package version it was built against.
    pub api_version: String,
    /// Category (`provider`, `tool`, `interceptor`, …).
    pub kind: String,
    /// What the component is for; empty when absent, the only optional key.
    pub description: String,
    /// Declared `host-*` interfaces, sorted; empty claims "needs nothing", distinct from an absent manifest.
    pub capabilities: Vec<String>,
}

/// Why a manifest could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The file could not be read.
    #[error("reading {path}")]
    Unreadable {
        /// Path that could not be read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not TOML.
    #[error("{path} is not valid TOML: {detail}")]
    Malformed {
        /// Path that failed to parse.
        path: String,
        /// The parser's complaint.
        detail: String,
    },
    /// A required key is missing or the wrong type.
    #[error("{path} has no {key}")]
    MissingKey {
        /// Path that is incomplete.
        path: String,
        /// The key that is missing.
        key: &'static str,
    },
}

impl Manifest {
    /// Read the manifest beside a component; `Ok(None)` means no file, distinct from one declaring nothing.
    ///
    /// # Errors
    /// [`ManifestError`] if the file exists but cannot be read, parsed or lacks a required key; never treated as absent.
    pub fn beside(component: &Path) -> Result<Option<Self>, ManifestError> {
        let path = manifest_path(component);
        if !path.exists() {
            return Ok(None);
        }
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Unreadable {
            path: shown.clone(),
            source,
        })?;
        let table: toml::Table =
            text.parse()
                .map_err(|e: toml::de::Error| ManifestError::Malformed {
                    path: shown.clone(),
                    detail: e.message().to_owned(),
                })?;

        let string = |key: &'static str| -> Result<String, ManifestError> {
            table
                .get(key)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| ManifestError::MissingKey {
                    path: shown.clone(),
                    key,
                })
        };
        let mut capabilities: Vec<String> = table
            .get("capabilities")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| ManifestError::MissingKey {
                path: shown.clone(),
                key: "capabilities",
            })?
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect();
        capabilities.sort();
        capabilities.dedup();

        Ok(Some(Self {
            name: string("name")?,
            version: string("version")?,
            api_version: string("api-version")?,
            kind: string("kind")?,
            description: table
                .get("description")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            capabilities,
        }))
    }

    /// Interfaces `imported` uses that this manifest does not declare: the refusal set.
    #[must_use]
    pub fn undeclared<'a>(&self, imported: &'a [String]) -> Vec<&'a str> {
        imported
            .iter()
            .filter(|interface| !self.capabilities.contains(*interface))
            .map(String::as_str)
            .collect()
    }
}

/// Where a component's manifest lives: beside it, same stem; public so `ext remove` deletes the file `beside` reads.
#[must_use]
pub fn manifest_path(component: &Path) -> PathBuf {
    let stem = component.file_stem().map_or_else(
        || component.to_string_lossy().into_owned(),
        |s| s.to_string_lossy().into_owned(),
    );
    component.with_file_name(format!("{stem}.manifest.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let component = dir.join(format!("{name}.wasm"));
        std::fs::write(dir.join(format!("{name}.manifest.toml")), body).unwrap();
        component
    }

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jk-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const GOOD: &str = r#"
name = "tool-fs"
version = "0.1.0"
api-version = "0.1.0"
kind = "tool"
description = "reads files"
capabilities = [
    "host-fs",
]
"#;

    #[test]
    fn a_generated_manifest_reads_back() {
        let dir = tmp();
        let component = write(&dir, "tool-fs", GOOD);
        let manifest = Manifest::beside(&component).unwrap().expect("present");
        assert_eq!(manifest.name, "tool-fs");
        assert_eq!(manifest.api_version, "0.1.0");
        assert_eq!(manifest.kind, "tool");
        assert_eq!(manifest.description, "reads files");
        assert_eq!(manifest.capabilities, vec!["host-fs"]);
    }

    #[test]
    fn a_manifest_without_a_description_still_reads() {
        let dir = tmp();
        let component = write(
            &dir,
            "tool-terse",
            "name = \"tool-terse\"\nversion = \"0.1.0\"\napi-version = \"0.1.0\"\n\
             kind = \"tool\"\ncapabilities = []\n",
        );
        let manifest = Manifest::beside(&component).unwrap().expect("present");
        assert_eq!(manifest.description, "");
    }

    #[test]
    fn no_manifest_is_none_rather_than_an_error() {
        let dir = tmp();
        let component = dir.join("tool-fs.wasm");
        assert_eq!(Manifest::beside(&component).unwrap(), None);
    }

    #[test]
    fn a_broken_manifest_is_an_error_not_an_absence() {
        let dir = tmp();
        let component = write(&dir, "tool-fs", "this is not toml = = =");
        assert!(matches!(
            Manifest::beside(&component),
            Err(ManifestError::Malformed { .. })
        ));
    }

    #[test]
    fn a_missing_key_names_itself() {
        let dir = tmp();
        let component = write(&dir, "tool-fs", "name = \"tool-fs\"\n");
        let Err(ManifestError::MissingKey { key, .. }) = Manifest::beside(&component) else {
            panic!("expected a missing-key error");
        };
        assert_eq!(key, "capabilities", "the first absent key is reported");
    }

    #[test]
    fn an_empty_capability_list_is_a_declaration() {
        let dir = tmp();
        let component = write(
            &dir,
            "quiet",
            "name = \"quiet\"\nversion = \"0.1.0\"\napi-version = \"0.1.0\"\n\
             kind = \"tool\"\ncapabilities = []\n",
        );
        let manifest = Manifest::beside(&component).unwrap().expect("present");
        assert_eq!(manifest.capabilities, [] as [std::string::String; 0]);
        assert_eq!(
            manifest.undeclared(&["host-fs".to_owned()]),
            vec!["host-fs"],
            "declaring nothing does not excuse importing something"
        );
    }

    #[test]
    fn undeclared_reports_only_what_is_missing() {
        let dir = tmp();
        let component = write(&dir, "tool-fs", GOOD);
        let manifest = Manifest::beside(&component).unwrap().unwrap();

        assert!(
            manifest.undeclared(&["host-fs".to_owned()]).is_empty(),
            "a declaration that covers the imports is not a refusal"
        );
        assert_eq!(
            manifest.undeclared(&["host-fs".to_owned(), "host-process".to_owned()]),
            vec!["host-process"],
            "only the interface that is missing is named"
        );
    }

    #[test]
    fn declaring_more_than_the_component_imports_is_not_a_problem() {
        let dir = tmp();
        let component = write(
            &dir,
            "generous",
            "name = \"generous\"\nversion = \"0.1.0\"\napi-version = \"0.1.0\"\n\
             kind = \"tool\"\ncapabilities = [\"host-fs\", \"host-http\", \"host-process\"]\n",
        );
        let manifest = Manifest::beside(&component).unwrap().unwrap();
        assert_eq!(
            manifest.undeclared(&["host-fs".to_owned()]),
            [] as [&str; 0]
        );
    }

    #[test]
    fn a_hand_written_manifest_parses() {
        let dir = tmp();
        let component = write(
            &dir,
            "byhand",
            "# written by a person\n\
             name = \"byhand\"  # trailing comment\n\
             version = \"0.2.0\"\n\
             api-version = '0.1.0'\n\
             kind = \"tool\"\n\
             capabilities = [\"host-http\", \"host-fs\"]\n",
        );
        let manifest = Manifest::beside(&component).unwrap().expect("present");
        assert_eq!(manifest.name, "byhand");
        assert_eq!(manifest.api_version, "0.1.0", "single quotes are TOML too");
        assert_eq!(
            manifest.capabilities,
            vec!["host-fs", "host-http"],
            "an inline array, sorted on read"
        );
    }
}

#[cfg(test)]
mod api_version_tests {
    use super::api_compatible;

    #[test]
    fn the_same_version_is_compatible() {
        assert!(api_compatible("0.1.0", "0.1.0"));
        assert!(api_compatible("1.4.2", "1.4.2"));
    }

    #[test]
    fn a_differing_major_is_not() {
        assert!(!api_compatible("0.4.0", "2.0.0"));
        assert!(!api_compatible("2.0.0", "1.9.9"));
        assert!(!api_compatible("0.1.0", "0.4.0"));
    }

    #[test]
    fn a_differing_minor_below_one_point_zero_is_not() {
        assert!(!api_compatible("0.1.0", "0.2.0"));
        assert!(!api_compatible("0.9.0", "0.1.0"));
    }

    #[test]
    fn a_differing_minor_from_one_point_zero_on_is_compatible() {
        assert!(api_compatible("1.3.0", "1.1.0"));
        assert!(api_compatible("1.1.0", "1.3.0"));
    }

    #[test]
    fn a_patch_difference_is_ignored() {
        assert!(api_compatible("0.1.0", "0.1.7"));
        assert!(api_compatible("2.0.4", "2.0.0"));
    }

    #[test]
    fn an_unparseable_version_is_incompatible() {
        assert!(!api_compatible("0.1.0", "dev"));
        assert!(!api_compatible("0.1.0", "0"));
        assert!(!api_compatible("", "0.1.0"));
        assert!(!api_compatible("0.1.0", "0.x.0"));
    }
}
