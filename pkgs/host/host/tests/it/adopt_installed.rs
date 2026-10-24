//! `adopt_installed` admits components installed after boot (#214), through
//! the same compile path as boot. The refusals matter more than the success.

use jk_agent::Runtime;

use crate::common;

/// A runtime over a temp `ext/` holding only what the config enables.
fn runtime_over(tag: &str, staged: &[&str]) -> Option<(common::TempDir, Runtime)> {
    if !common::guests_staged(&["provider-openai.wasm", "tool-hello.wasm"]) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-adopt-{tag}-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).expect("creates the temp ext dir");
    let from = common::repo_root().join("ext");
    for name in staged {
        for suffix in [".wasm", ".manifest.toml"] {
            let file = format!("{name}{suffix}");
            if from.join(&file).exists() {
                std::fs::copy(from.join(&file), ext.join(&file)).expect("copies a guest");
            }
        }
    }
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
",
    )
    .expect("writes the config");
    let runtime = Runtime::boot(&config, &ext).expect("the runtime boots");
    Some((common::TempDir(dir), runtime))
}

/// A component absent from `config.yaml` at boot becomes part of the runtime.
#[test]
fn a_component_that_arrived_after_boot_is_adopted() {
    let Some((guard, mut runtime)) = runtime_over("adopts", &["provider-openai"]) else {
        return;
    };
    let ext = guard.0.join("ext");
    for suffix in [".wasm", ".manifest.toml"] {
        let name = format!("tool-hello{suffix}");
        std::fs::copy(common::repo_root().join("ext").join(&name), ext.join(&name))
            .expect("the install lands");
    }

    let id = runtime
        .adopt_installed("tool-hello")
        .expect("a real, manifested component is adopted");
    assert_eq!(id, "tool.hello", "the naming rule is the one boot uses");
}

/// Adopting something that is not there changes nothing.
#[test]
fn adopting_a_component_that_is_not_there_is_refused() {
    let Some((_guard, mut runtime)) = runtime_over("absent", &["provider-openai"]) else {
        return;
    };
    let before = runtime.extension_ids();
    let err = runtime
        .adopt_installed("tool-nothing")
        .expect_err("nothing is at that path");
    assert!(
        err.to_string().contains("nothing at"),
        "the refusal should name the missing file: {err}"
    );
    assert_eq!(
        runtime.extension_ids(),
        before,
        "a refused adoption still changed the runtime"
    );
}

/// A stem that is not `<category>-<kind>` is refused before reading anything.
#[test]
fn a_stem_without_a_category_is_refused() {
    let Some((_guard, mut runtime)) = runtime_over("stem", &["provider-openai"]) else {
        return;
    };
    let err = runtime
        .adopt_installed("nocategory")
        .expect_err("a bare name is not a component stem");
    assert!(err.to_string().contains("<category>-<kind>"), "{err}");
}

/// A stem naming an undispatched category is refused with the valid ones listed (#222).
#[test]
fn a_stem_naming_a_category_nothing_dispatches_is_refused() {
    let Some((_guard, mut runtime)) = runtime_over("category", &["provider-openai"]) else {
        return;
    };
    let err = runtime
        .adopt_installed("self-built")
        .expect_err("`self` is not a category this runtime dispatches");
    let text = err.to_string();
    assert!(
        text.contains("tool") && text.contains("provider"),
        "the refusal must name the categories that exist: {text}"
    );
}

/// Adopting something already loaded is refused; two instances of one id would make dispatch order-dependent.
#[test]
fn adopting_what_is_already_loaded_is_refused() {
    let Some((_guard, mut runtime)) = runtime_over("dup", &["provider-openai"]) else {
        return;
    };
    let err = runtime
        .adopt_installed("provider-openai")
        .expect_err("already loaded");
    assert!(err.to_string().contains("already loaded"), "{err}");
}

/// A component rewritten on disk is not reloaded (#214). This holds by absence
/// (no watcher, no rescan), so the test guards against a later feature adding one.
#[test]
fn a_component_rewritten_on_disk_is_not_picked_up() {
    let Some((guard, mut runtime)) = runtime_over("rewritten", &["provider-openai"]) else {
        return;
    };
    let ext = guard.0.join("ext");
    let loaded = ext.join("provider-openai.wasm");
    let before = std::fs::metadata(&loaded)
        .expect("the loaded component is there")
        .len();

    std::fs::copy(
        common::repo_root().join("ext").join("tool-hello.wasm"),
        &loaded,
    )
    .expect("overwrites the loaded component");
    let after = std::fs::metadata(&loaded).expect("still there").len();
    assert_ne!(
        before, after,
        "the fixture did not actually change the file"
    );

    let err = runtime
        .adopt_installed("provider-openai")
        .expect_err("a loaded id is not re-adopted");
    assert!(err.to_string().contains("already loaded"), "{err}");
    assert_eq!(
        runtime.extension_ids(),
        vec!["provider.openai".to_string()],
        "the runtime's view of itself changed when only the disk did"
    );
}

/// Adoption is not a back door: an under-declared component is refused as at boot.
#[test]
fn a_component_the_boot_path_would_refuse_is_refused_here_too() {
    let Some((guard, mut runtime)) = runtime_over("under", &["provider-openai"]) else {
        return;
    };
    let ext = guard.0.join("ext");
    std::fs::copy(
        common::repo_root().join("ext").join("tool-hello.wasm"),
        ext.join("tool-hello.wasm"),
    )
    .expect("the component lands");
    // Its manifest claims it needs nothing, which is a lie: `tool-hello`
    // imports `host-log`.
    std::fs::write(
        ext.join("tool-hello.manifest.toml"),
        format!(
            "name = \"tool-hello\"\nversion = \"0.1.0\"\napi-version = \"{}\"\n\
             kind = \"tool\"\ndescription = \"x\"\ncapabilities = []\n",
            jk_wasm::manifest::API_VERSION
        ),
    )
    .expect("writes the under-declaring manifest");

    let before = runtime.extension_ids();
    let err = runtime
        .adopt_installed("tool-hello")
        .expect_err("an under-declared component must not be adopted");
    assert!(
        err.to_string().contains("host-log"),
        "the refusal should name what was not declared: {err}"
    );
    assert_eq!(
        runtime.extension_ids(),
        before,
        "a refused adoption left the component behind anyway"
    );
}
