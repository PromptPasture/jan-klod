//! Loader for `config.yaml`.
//!
//! Extensions are grouped by category (`provider`, `store`, …); each named
//! entry under a category is one extension *instance*. The core interprets only
//! two keys per entry:
//!
//! * `enabled` — whether to load the instance (default `false`).
//! * `type`    — the wasm discriminator within the category; the component is
//!   `ext/<category>-<type>.wasm`. Defaults to the entry name, so
//!   `provider.openai` resolves to `provider-openai.wasm`. Several instances may
//!   share one `type` to reuse a single component.
//!
//! Everything else in an entry is opaque domain config: the core never
//! interprets it, only env-expands `${VAR}` references and hands it back to the
//! instance through the `host-config` interface. Likewise the top-level
//! agent-behaviour keys (`providers`, `routing`, …) are preserved verbatim for
//! the `manager-agent-loop` extension — the core holds no routing logic.

use std::path::Path;

use serde_json::{Map, Value};

/// One configured extension instance resolved from `config.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionInstance {
    /// `<category>.<name>` — unique instance id, e.g. `provider.openai`.
    pub id: String,
    /// Grouping category, e.g. `provider`.
    pub category: String,
    /// Instance name within the category, e.g. `openai`.
    pub name: String,
    /// `type` discriminator; defaults to `name`, e.g. `openai`.
    pub kind: String,
    /// Wasm component stem `<category>-<kind>`, e.g. `provider-openai`.
    pub component: String,
    /// Whether the core should load this instance.
    pub enabled: bool,
    /// Opaque config section (the entry minus `enabled`/`type`), env-expanded
    /// for enabled instances. Served back through `host-config`.
    pub config: Value,
}

impl ExtensionInstance {
    /// Wasm file name to resolve under the `ext/` directory.
    #[must_use]
    pub fn component_file(&self) -> String {
        format!("{}.wasm", self.component)
    }
}

/// Parsed `config.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Every declared instance, ordered by `(category, name)`.
    pub instances: Vec<ExtensionInstance>,
    /// Top-level keys other than `extensions` (e.g. `providers`, `routing`),
    /// preserved verbatim for the agent-loop extension. Always an object.
    pub agent: Value,
}

impl Config {
    /// Read and parse a `config.yaml` from disk.
    ///
    /// # Errors
    /// Returns [`ConfigError::Read`] if the file cannot be read, or any parse
    /// error from [`Config::from_yaml`].
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_yaml(&text)
    }

    /// Parse a `config.yaml` from a string.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] if the YAML is malformed, the structure is not
    /// the expected category→instance mapping, an enabled instance contains an
    /// unterminated `${` or references an unset `${VAR}`, or more than one store
    /// is enabled.
    pub fn from_yaml(yaml: &str) -> Result<Self, ConfigError> {
        let root: Value = serde_yaml_ng::from_str(yaml)?;
        let Value::Object(mut root) = root else {
            return Err(ConfigError::RootNotMap);
        };
        let extensions = root
            .remove("extensions")
            .unwrap_or_else(|| Value::Object(Map::new()));
        let instances = parse_instances(extensions)?;
        validate(&instances)?;
        // Whatever remains at the top level is opaque agent-behaviour config.
        Ok(Self {
            instances,
            agent: Value::Object(root),
        })
    }

    /// Iterator over the enabled instances only.
    pub fn enabled(&self) -> impl Iterator<Item = &ExtensionInstance> {
        self.instances.iter().filter(|i| i.enabled)
    }
}

fn parse_instances(extensions: Value) -> Result<Vec<ExtensionInstance>, ConfigError> {
    let Value::Object(categories) = extensions else {
        return Err(ConfigError::ExtensionsNotMap);
    };
    let mut out = Vec::new();
    for (category, entries) in categories {
        let Value::Object(entries) = entries else {
            return Err(ConfigError::CategoryNotMap { category });
        };
        for (name, entry) in entries {
            let id = format!("{category}.{name}");
            let Value::Object(mut entry) = entry else {
                return Err(ConfigError::InstanceNotMap { id });
            };
            let enabled = match entry.remove("enabled") {
                None => false,
                Some(Value::Bool(b)) => b,
                Some(_) => return Err(ConfigError::EnabledNotBool { id }),
            };
            let kind = match entry.remove("type") {
                None => name.clone(),
                Some(Value::String(s)) => s,
                Some(_) => return Err(ConfigError::TypeNotString { id }),
            };
            let component = format!("{category}-{kind}");
            let mut config = Value::Object(entry);
            // Only enabled instances are loaded, so only they need their secrets
            // resolved — a disabled provider may reference an unset ${VAR}.
            if enabled {
                expand_env(&mut config, &id)?;
            }
            out.push(ExtensionInstance {
                id,
                category: category.clone(),
                name,
                kind,
                component,
                enabled,
                config,
            });
        }
    }
    Ok(out)
}

/// Core-level invariant checks. Domain rules (e.g. routing references) belong to
/// the extensions that consume them, not the core.
fn validate(instances: &[ExtensionInstance]) -> Result<(), ConfigError> {
    let stores: Vec<&str> = instances
        .iter()
        .filter(|i| i.enabled && i.category == "store")
        .map(|i| i.id.as_str())
        .collect();
    if stores.len() > 1 {
        return Err(ConfigError::MultipleStores {
            names: stores.join(", "),
        });
    }
    Ok(())
}

/// Recursively expand `${VAR}` references in every string within `value`.
fn expand_env(value: &mut Value, id: &str) -> Result<(), ConfigError> {
    match value {
        Value::String(s) => {
            if let Some(expanded) = expand_str(s, id)? {
                *s = expanded;
            }
        }
        Value::Array(items) => {
            for item in items {
                expand_env(item, id)?;
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                expand_env(v, id)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Replace every `${NAME}` in `s` with the environment value of `NAME`.
/// Returns `None` when there is nothing to expand. An unterminated `${` is an
/// error; an unset variable is an error.
fn expand_str(s: &str, id: &str) -> Result<Option<String>, ConfigError> {
    if !s.contains("${") {
        return Ok(None);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(ConfigError::UnterminatedExpansion { id: id.to_string() });
        };
        let var = &after[..end];
        let val = std::env::var(var).map_err(|_| ConfigError::MissingEnv {
            id: id.to_string(),
            var: var.to_string(),
        })?;
        out.push_str(&val);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(Some(out))
}

/// Errors surfaced while loading `config.yaml`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read from disk.
    #[error("reading {path}: {source}")]
    Read {
        /// Path that failed to read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The YAML could not be parsed.
    #[error("parsing config: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
    /// The document root is not a mapping.
    #[error("config root must be a mapping")]
    RootNotMap,
    /// `extensions` is present but is not a mapping of categories.
    #[error("`extensions` must be a mapping of categories")]
    ExtensionsNotMap,
    /// A category (e.g. `provider`) is not a mapping of named instances.
    #[error("extensions.{category} must be a mapping of named instances")]
    CategoryNotMap {
        /// The offending category name.
        category: String,
    },
    /// An instance entry is not a mapping.
    #[error("{id} must be a mapping")]
    InstanceNotMap {
        /// The offending instance id.
        id: String,
    },
    /// An instance's `enabled` key is not a boolean.
    #[error("{id}: `enabled` must be a boolean")]
    EnabledNotBool {
        /// The offending instance id.
        id: String,
    },
    /// An instance's `type` key is not a string.
    #[error("{id}: `type` must be a string")]
    TypeNotString {
        /// The offending instance id.
        id: String,
    },
    /// An enabled instance references an environment variable that is not set.
    #[error("{id}: environment variable `{var}` is not set")]
    MissingEnv {
        /// The instance referencing the variable.
        id: String,
        /// The unset variable name.
        var: String,
    },
    /// More than one `store` instance is enabled (at most one is allowed).
    #[error("more than one store enabled ({names}); exactly one store may be active")]
    MultipleStores {
        /// Comma-separated ids of the conflicting stores.
        names: String,
    },
    /// A `${` in a config string has no matching `}`.
    #[error("{id}: unterminated `${{` in config value (missing closing `}}`)")]
    UnterminatedExpansion {
        /// The instance whose config contains the unterminated placeholder.
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn instance<'a>(cfg: &'a Config, id: &str) -> &'a ExtensionInstance {
        cfg.instances
            .iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("no instance {id}"))
    }

    #[test]
    fn resolves_type_default_and_override() {
        let cfg = Config::from_yaml(
            "
extensions:
  provider:
    openai:
      enabled: true
    lm-studio:
      enabled: false
      type: openai
",
        )
        .unwrap();

        // Default type = entry name.
        let openai = instance(&cfg, "provider.openai");
        assert_eq!(openai.kind, "openai");
        assert_eq!(openai.component, "provider-openai");
        assert_eq!(openai.component_file(), "provider-openai.wasm");
        assert_eq!(openai.category, "provider");
        assert_eq!(openai.name, "openai");

        // Explicit type reuses another component.
        let lm = instance(&cfg, "provider.lm-studio");
        assert_eq!(lm.kind, "openai");
        assert_eq!(lm.component, "provider-openai");
    }

    #[test]
    fn enabled_defaults_to_false() {
        let cfg = Config::from_yaml(
            "
extensions:
  store:
    memory: {}
",
        )
        .unwrap();
        assert!(!instance(&cfg, "store.memory").enabled);
        assert_eq!(cfg.enabled().count(), 0);
    }

    #[test]
    fn config_section_excludes_host_keys() {
        let cfg = Config::from_yaml(
            "
extensions:
  provider:
    openai:
      enabled: true
      type: openai
      model: gpt-4o-mini
      base-url: https://api.openai.com/v1
",
        )
        .unwrap();
        let openai = instance(&cfg, "provider.openai");
        assert_eq!(
            openai.config,
            json!({ "model": "gpt-4o-mini", "base-url": "https://api.openai.com/v1" })
        );
    }

    #[test]
    fn expands_env_for_enabled_instances() {
        // SAFETY: edition 2021 — set_var is safe; unique key avoids cross-test races.
        std::env::set_var("JK_TEST_OPENAI_KEY", "sk-secret");
        let cfg = Config::from_yaml(
            "
extensions:
  provider:
    openai:
      enabled: true
      api-key: ${JK_TEST_OPENAI_KEY}
      base-url: http://${JK_TEST_OPENAI_KEY}.example/v1
",
        )
        .unwrap();
        let openai = instance(&cfg, "provider.openai");
        assert_eq!(openai.config["api-key"], json!("sk-secret"));
        assert_eq!(openai.config["base-url"], json!("http://sk-secret.example/v1"));
    }

    #[test]
    fn missing_env_in_enabled_instance_errors() {
        std::env::remove_var("JK_TEST_UNSET_VAR");
        let err = Config::from_yaml(
            "
extensions:
  provider:
    openai:
      enabled: true
      api-key: ${JK_TEST_UNSET_VAR}
",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnv { .. }));
    }

    #[test]
    fn disabled_instance_keeps_unexpanded_env() {
        std::env::remove_var("JK_TEST_DISABLED_VAR");
        let cfg = Config::from_yaml(
            "
extensions:
  provider:
    openai:
      enabled: false
      api-key: ${JK_TEST_DISABLED_VAR}
",
        )
        .unwrap();
        // No error, and the raw reference is preserved untouched.
        let openai = instance(&cfg, "provider.openai");
        assert_eq!(openai.config["api-key"], json!("${JK_TEST_DISABLED_VAR}"));
    }

    #[test]
    fn one_store_is_allowed_two_is_an_error() {
        let one = Config::from_yaml(
            "
extensions:
  store:
    memory:
      enabled: true
    sqlite:
      enabled: false
",
        );
        assert!(one.is_ok());

        let two = Config::from_yaml(
            "
extensions:
  store:
    memory:
      enabled: true
    sqlite:
      enabled: true
",
        )
        .unwrap_err();
        assert!(matches!(two, ConfigError::MultipleStores { .. }));
    }

    #[test]
    fn preserves_top_level_agent_behaviour() {
        let cfg = Config::from_yaml(
            "
extensions:
  provider:
    openai:
      enabled: true
providers:
  - provider: openai
    models: [gpt-4o, gpt-4o-mini]
routing:
  chat: openai/gpt-4o-mini
",
        )
        .unwrap();
        assert_eq!(cfg.agent["routing"]["chat"], json!("openai/gpt-4o-mini"));
        assert_eq!(cfg.agent["providers"][0]["provider"], json!("openai"));
        // `extensions` must not leak into the preserved agent config.
        assert!(cfg.agent.get("extensions").is_none());
    }

    #[test]
    fn rejects_non_mapping_entry() {
        let err = Config::from_yaml(
            "
extensions:
  store:
    memory: true
",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::InstanceNotMap { .. }));
    }

    #[test]
    fn unterminated_expansion_is_an_error() {
        let err = Config::from_yaml(
            "
extensions:
  provider:
    openai:
      enabled: true
      api-key: ${MISSING_BRACE
",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::UnterminatedExpansion { .. }), "{err}");
    }
}
