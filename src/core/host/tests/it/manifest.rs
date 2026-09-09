//! Two independent readers of the same components must agree.
//!
//! `scripts/manifests.sh` derives each guest's capabilities from the
//! `wasm-tools` CLI at build time; `Runtime::boot` derives them from wasmtime's
//! component-type API at load time. Nothing forces those to agree — different
//! tool, different library, different moment — so this asserts they do, for
//! every staged guest.
//!
//! That is worth more than either check alone. A single reader can only be
//! wrong consistently: if the filter widened to include a world's *exports*, a
//! test comparing that reader against itself would pass while every manifest
//! silently listed `tool-callable` as a capability. Two readers can only agree
//! by both being right, or by being wrong in the same way for the same reason —
//! and they share no code.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use jan_klod_core::{LoadState, Runtime};

use crate::common;

/// The guests whose manifests and imports are compared. Every one that is
/// staged, rather than a chosen few: a mismatch on the eighteenth is as much a
/// bug as one on the first.
const GUESTS: [&str; 6] = [
    "provider-openai.wasm",
    "tool-fs.wasm",
    "tool-shell.wasm",
    "tool-fetch.wasm",
    "tool-escape-probe.wasm",
    "interceptor-permission.wasm",
];

/// `capabilities` from a generated manifest, sorted.
fn declared(ext_dir: &std::path::Path, guest: &str) -> Vec<String> {
    let path = ext_dir.join(format!("{guest}.manifest.toml"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is unreadable: {e}", path.display()));
    let mut found: Vec<String> = text
        .lines()
        .skip_while(|line| !line.starts_with("capabilities = ["))
        .skip(1)
        .take_while(|line| !line.starts_with(']'))
        .filter_map(|line| {
            let trimmed = line.trim().trim_end_matches(',');
            trimmed
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_owned)
        })
        .collect();
    found.sort();
    found
}

/// A config enabling one instance per guest under test, so `boot` compiles each
/// and records what it imports.
fn config_for(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        "
execution:
  enabled: false
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    permission:
      enabled: true
  tool:
    fs:
      enabled: true
    shell:
      enabled: true
    fetch:
      enabled: true
    escape-probe:
      enabled: true
",
    )
    .unwrap();
    path
}

#[test]
fn what_a_component_imports_matches_what_its_manifest_declares() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    // A manifest per guest is the other half of the comparison; without it there
    // is nothing to compare against, and silently passing would be the point of
    // this file defeated.
    for guest in GUESTS {
        // `GUESTS` names components (`tool-fs.wasm`) because that is what
        // `guests_staged` checks; a manifest is named for the same stem.
        let manifest = ext_dir.join(format!("{}.manifest.toml", guest.trim_end_matches(".wasm")));
        assert!(
            manifest.exists(),
            "{} is missing — run `make ext`, which generates manifests beside the components",
            manifest.display()
        );
    }

    let dir = std::env::temp_dir().join(format!("jk-manifest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = config_for(&dir);

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");

    let mut compared = 0;
    for extension in runtime.extensions() {
        // `tool.escape-probe` resolves to `tool-escape-probe.wasm`, and the
        // manifest is named for the component, not the instance.
        let component = extension.instance.component_file();
        let Some(observed) = &extension.capabilities else {
            panic!(
                "{} compiled nothing — capabilities are only absent for a missing component",
                extension.instance.id
            );
        };
        assert!(
            matches!(extension.state, LoadState::Compiled(_)),
            "{} did not compile",
            extension.instance.id
        );

        let stem = component.trim_end_matches(".wasm");
        let expected = declared(&ext_dir, stem);
        assert_eq!(
            observed, &expected,
            "`{component}` imports {observed:?} but its manifest declares {expected:?} — \
             one of the two readers is wrong, and they share no code"
        );
        compared += 1;
    }

    assert_eq!(
        compared,
        GUESTS.len(),
        "every enabled instance was compared; the config and GUESTS have drifted apart"
    );
}

/// The specific values, so a change that made both readers agree on something
/// wrong still fails. Chosen for what each proves rather than for coverage.
#[test]
fn the_capability_sets_are_the_ones_expected() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-manifest-values-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let runtime = Runtime::boot(config_for(&dir), &ext_dir).expect("runtime boots");

    let of = |component: &str| -> Vec<String> {
        runtime
            .extensions()
            .iter()
            .find(|e| e.instance.component_file() == component)
            .and_then(|e| e.capabilities.clone())
            .unwrap_or_else(|| panic!("{component} was not loaded"))
    };

    // Exports excluded: this one also exports `tool-callable` and
    // `extension-lifecycle`, which are what it implements, not what it needs.
    assert_eq!(of("tool-fs.wasm"), vec!["host-fs"]);
    // Type-only imports excluded: this one also imports `llm-types`, which
    // grants nothing.
    assert_eq!(
        of("provider-openai.wasm"),
        vec!["host-config", "host-http", "host-log"]
    );
    // The component written to attempt escapes with plain `std` rather than
    // typed imports asks for nothing but logging — which is why
    // `sandbox_boundary.rs` can prove it gets nothing.
    assert_eq!(of("tool-escape-probe.wasm"), vec!["host-log"]);
    // And the fact #46's deferred per-turn sandbox warning is waiting on: a
    // tool that can run a command is distinguishable from one that cannot.
    assert_eq!(of("tool-shell.wasm"), vec!["host-process"]);
    assert!(
        !of("tool-fs.wasm").contains(&"host-process".to_string()),
        "a file tool cannot run a command, and the import set says so"
    );
}
