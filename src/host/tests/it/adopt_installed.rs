//! A component installed after boot can be adopted, and a bad one cannot
//! (#214).
//!
//! `Runtime::boot` compiles the instances `config.yaml` enables and never
//! adds to them, so a component that arrives during a session is inert. This
//! is the other half: `adopt_installed` compiles it through the **same**
//! path boot uses, so a component adopted at runtime is not a weaker class
//! than one present at boot.
//!
//! The refusals matter more than the success here. Adoption is how code
//! enters a running runtime, so a test that only showed it working would be
//! the wrong half to have.

use jan_klod_core::Runtime;

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

/// The point of the whole slice: a component that was not in `config.yaml`
/// at boot becomes part of the runtime.
#[test]
fn a_component_that_arrived_after_boot_is_adopted() {
    let Some((guard, mut runtime)) = runtime_over("adopts", &["provider-openai"]) else {
        return;
    };
    let ext = guard.0.join("ext");
    // It arrives the way an install leaves it: the pair, in `ext/`.
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

/// A stem that is not `<category>-<kind>` is refused before anything is
/// read, because guessing a category would invent an instance nobody
/// configured.
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

/// Adopting something already loaded is refused rather than duplicated.
///
/// Two instances of one id would each be dispatched, and which one answered
/// would depend on ordering — the kind of thing that works until it does
/// not.
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

/// A component that changed on disk is not reloaded (#214).
///
/// The "no silent reload" the issue requires, and it holds **by
/// construction rather than by a guard**: nothing watches the filesystem,
/// and the only way a component enters a running runtime is
/// `adopt_installed`, which refuses an id it already has. So overwriting a
/// loaded component's bytes changes nothing until the process restarts.
///
/// Worth a test anyway. The property is currently an absence — no watcher,
/// no rescan — and absences are what a later convenience feature removes
/// without noticing.
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

    // Replace it with something else entirely — a different component.
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

    // Nothing has rescanned, and asking would be refused.
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

/// A component whose manifest under-declares is refused at adoption, the
/// same as at boot.
///
/// This is the test that says adoption is not a back door. An install that
/// landed a component the boot path would have refused, adopted anyway,
/// would make "installed at runtime" the way to get an unchecked component
/// into the runtime.
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
        "name = \"tool-hello\"\nversion = \"0.1.0\"\napi-version = \"0.3.0\"\n\
         kind = \"tool\"\ndescription = \"x\"\ncapabilities = []\n",
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
