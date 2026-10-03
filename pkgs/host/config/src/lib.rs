//! Loader for `config.yaml`.
//!
//! Extensions group by category (`provider`, `store`, …); each entry is one *instance*.
//! The core interprets two keys:
//!
//! * `enabled` — load the instance (default `false`).
//! * `type` — wasm discriminator; component is `ext/<category>-<type>.wasm`.
//!   Defaults to entry name (`provider.openai` → `provider-openai.wasm`);
//!   multiple instances may share one `type`.
//!
//! Everything else is opaque domain config: expanded `${VAR}` and passed to the
//! instance via `host-config`. Top-level agent keys (`providers`, `routing`, …)
//! are preserved for `manager-agent-loop`; the core has no routing logic.

use std::path::Path;
use std::str::FromStr;

use serde_json::{Map, Value};

/// Output format for observability logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable text format.
    Human,
    /// Newline-delimited JSON format (JSONL).
    Json,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "human" => Ok(Self::Human),
            "json" => Ok(Self::Json),
            _ => Err(format!(
                "invalid output format '{s}': must be 'human' or 'json'"
            )),
        }
    }
}

impl OutputFormat {
    /// Return the string representation of this format.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
        }
    }
}

/// Observability configuration from the `observability:` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observability {
    /// Output format for logs: `human` (default) or `json`.
    pub output_format: OutputFormat,
}

impl Observability {
    /// Parse observability config from a top-level value.
    ///
    /// # Errors
    /// Returns error if:
    /// - the value is not a mapping (`ObservabilityNotMap`)
    /// - `output-format` is not a string (`ObservabilityFormatNotString`)
    /// - `output-format` has an invalid value like `plain` (`InvalidOutputFormat`)
    ///
    /// Returns `Ok` with `OutputFormat::Human` default if `output-format` is absent.
    pub fn from_value(value: &Value) -> Result<Self, ConfigError> {
        let Value::Object(obs) = value else {
            return Err(ConfigError::ObservabilityNotMap);
        };

        // Get the output-format key, defaulting to Human if absent
        match obs.get("output-format") {
            None => Ok(Self {
                output_format: OutputFormat::Human,
            }),
            Some(format_value) => {
                let Some(format_str) = format_value.as_str() else {
                    return Err(ConfigError::ObservabilityFormatNotString);
                };

                OutputFormat::from_str(format_str)
                    .map(|output_format| Self { output_format })
                    .map_err(|_| ConfigError::InvalidOutputFormat {
                        value: format_str.to_string(),
                    })
            }
        }
    }

    /// Default observability configuration (human-readable format).
    #[must_use]
    pub const fn default() -> Self {
        Self {
            output_format: OutputFormat::Human,
        }
    }
}

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
    /// Top-level keys excluding `extensions` (e.g. `providers`, `routing`),
    /// preserved for agent-loop. Always an object.
    pub agent: Value,
    /// Observability configuration (structured logging format).
    pub observability: Observability,
}

impl Config {
    /// Parse `config.yaml` from disk.
    ///
    /// # Errors
    /// [`ConfigError::Read`] (file not readable) or [`Config::from_yaml`] parse errors.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_yaml(&text)
    }

    /// Fetch a top-level key without expanding instance variables.
    ///
    /// [`Config::from_path`] expands `${VAR}` in enabled instances and fails on
    /// unset, wrong for reading unrelated keys. `ext install` needs
    /// `registry.trusted-keys` (no provider API key required). Uses the same
    /// YAML parse as [`Config::from_path`], stopping before instance expansion.
    ///
    /// # Errors
    /// [`ConfigError::Read`], [`ConfigError::Yaml`], or [`ConfigError::RootNotMap`].
    pub fn top_level(path: impl AsRef<Path>, key: &str) -> Result<Option<Value>, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let root: Value = serde_yaml_ng::from_str(&text)?;
        let Value::Object(root) = root else {
            return Err(ConfigError::RootNotMap);
        };
        Ok(root.get(key).cloned())
    }

    /// Parse `config.yaml` from a string.
    ///
    /// # Errors
    /// [`ConfigError`] if YAML is invalid, structure is not category→instance,
    /// enabled instance has unterminated `${` / unset `${VAR}`, or multiple stores enabled.
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

        // Parse observability configuration or use default.
        let observability = if let Some(obs_value) = root.remove("observability") {
            Observability::from_value(&obs_value)?
        } else {
            Observability::default()
        };

        // Whatever remains at the top level is opaque agent-behaviour config.
        Ok(Self {
            instances,
            agent: Value::Object(root),
            observability,
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
            // Only enabled instances need secrets resolved; disabled may reference unset ${VAR}.
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

/// Core-level invariants only; domain rules (e.g. routing) belong to extensions.
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

/// Replace every `${NAME}` in `s` with its environment value.
/// Returns `None` if no expansions. Errors on unterminated `${` or unset variables.
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
    /// `observability:` section is not a mapping.
    #[error("observability: must be a mapping")]
    ObservabilityNotMap,
    /// `observability.output-format` is not a string.
    #[error("observability: 'output-format' must be a string")]
    ObservabilityFormatNotString,
    /// `observability.output-format` has an invalid value.
    #[error("observability: invalid output format '{value}' (allowed: 'human', 'json')")]
    InvalidOutputFormat {
        /// The invalid format value.
        value: String,
    },
}
