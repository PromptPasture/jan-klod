#![allow(missing_docs)]
use jan_klod_config::{Config, ConfigError, ExtensionInstance};
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

    let openai = instance(&cfg, "provider.openai");
    assert_eq!(openai.kind, "openai");
    assert_eq!(openai.component, "provider-openai");
    assert_eq!(openai.component_file(), "provider-openai.wasm");
    assert_eq!(openai.category, "provider");
    assert_eq!(openai.name, "openai");

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
    assert_eq!(
        openai.config["base-url"],
        json!("http://sk-secret.example/v1")
    );
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
    assert!(
        matches!(err, ConfigError::UnterminatedExpansion { .. }),
        "{err}"
    );
}

#[test]
fn from_path_reads_a_yaml_file() {
    let dir = std::env::temp_dir().join(format!("jk-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        "extensions:\n  provider:\n    openai:\n      enabled: true\n",
    )
    .unwrap();
    let cfg = Config::from_path(&path).unwrap();
    assert!(instance(&cfg, "provider.openai").enabled);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn from_path_surfaces_missing_file_error() {
    let err = Config::from_path("/nonexistent/path/config.yaml").unwrap_err();
    assert!(matches!(err, ConfigError::Read { .. }), "{err}");
}
