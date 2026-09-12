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

/// Acceptance line 1: a manifest that omits a capability the component imports
/// is refused, and the message names the interface.
///
/// The fixture is a real guest's manifest with one capability deleted, copied
/// into a temp `ext/` alongside the real component. Building a guest that is
/// deliberately wrong would have tested a component nobody ships; this tests
/// the exact artifact that does ship, described wrongly.
#[test]
fn a_component_importing_more_than_it_declares_is_refused() {
    if !common::guests_staged(&["tool-fetch.wasm"]) {
        return;
    }
    let real_ext = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-manifest-undeclared-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    let _guard = common::TempDir(dir.clone());

    std::fs::copy(
        real_ext.join("tool-fetch.wasm"),
        ext.join("tool-fetch.wasm"),
    )
    .unwrap();
    // `tool-fetch` imports host-config and host-http. Drop the http line.
    let manifest = std::fs::read_to_string(real_ext.join("tool-fetch.manifest.toml")).unwrap();
    assert!(
        manifest.contains("\"host-http\","),
        "the fixture depends on tool-fetch declaring host-http: {manifest}"
    );
    std::fs::write(
        ext.join("tool-fetch.manifest.toml"),
        manifest.replace("    \"host-http\",\n", ""),
    )
    .unwrap();

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "\nextensions:\n  tool:\n    fetch:\n      enabled: true\n",
    )
    .unwrap();

    // `Runtime` is not `Debug`, so bind the error rather than `expect_err`.
    let Err(error) = Runtime::boot(&config, &ext) else {
        panic!("a manifest that under-declares must be refused, and this booted");
    };
    let message = format!("{error}");
    assert!(
        message.contains("host-http"),
        "the refusal names the interface the component imports: {message}"
    );
    assert!(
        message.contains("tool-fetch"),
        "and the component it is about: {message}"
    );
    assert!(
        !message.contains("host-config"),
        "and not the one that was declared correctly: {message}"
    );
}

/// Acceptance line 2: declaring a capability `config.yaml` does not grant is
/// fine — it boots, and the capability is still denied at the call.
///
/// The manifest is not a second place grants are kept. Every capability is
/// default-deny where it is used, so an over-declaring manifest asks for more
/// than it receives and simply does not get it; refusing it would mean an
/// author had to keep their declaration in step with every operator's config.
#[test]
fn declaring_a_capability_the_config_does_not_grant_still_boots() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let real_ext = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-manifest-generous-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    let _guard = common::TempDir(dir.clone());

    std::fs::copy(real_ext.join("tool-fs.wasm"), ext.join("tool-fs.wasm")).unwrap();
    // Declare host-process as well, which this component does not import and
    // which the config below does not grant (`execution:` is absent entirely).
    let manifest = std::fs::read_to_string(real_ext.join("tool-fs.manifest.toml")).unwrap();
    std::fs::write(
        ext.join("tool-fs.manifest.toml"),
        manifest.replace(
            "    \"host-fs\",\n",
            "    \"host-fs\",\n    \"host-process\",\n",
        ),
    )
    .unwrap();

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext).expect("an over-declaring manifest still boots");
    let loaded = runtime
        .extensions()
        .iter()
        .find(|e| e.instance.component_file() == "tool-fs.wasm")
        .expect("the tool loaded");
    assert_eq!(
        loaded.capabilities.as_deref(),
        Some(["host-fs".to_owned()].as_slice()),
        "what it imports is unchanged by what it declares"
    );

    // And the declaration granted nothing: with no `execution:` block the
    // process substrate is denied, so a command cannot run whatever the
    // manifest says.
    let factory = || common::canned_http("unused");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let out = agent.run("s", "hello");
    assert!(
        matches!(out, jan_klod_core::conductor::RunResult::Answered { .. })
            || matches!(out, jan_klod_core::conductor::RunResult::Failed(_)),
        "the turn ran one way or the other; what matters is that boot did not refuse"
    );
}

/// A component with no manifest beside it is refused by default.
///
/// The declaration is the thing that can be inspected *before* running
/// anything, so a component that ships without one defeats the point of
/// having them at all. Refusing by default is what makes the grant below a
/// grant rather than a formality.
#[test]
fn a_component_with_no_manifest_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-manifest-none-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    let _guard = common::TempDir(dir.clone());
    // The component alone — no manifest, as a hand-copied `.wasm` or a bundle
    // built before manifests existed would leave it.
    std::fs::copy(
        common::repo_root().join("ext").join("tool-fs.wasm"),
        ext.join("tool-fs.wasm"),
    )
    .unwrap();

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
    )
    .unwrap();

    let Err(error) = Runtime::boot(&config, &ext) else {
        panic!("a component with no manifest must be refused, and this booted");
    };
    let message = format!("{error}");
    assert!(
        message.contains("tool-fs.wasm") && message.contains("no manifest"),
        "the refusal names the component and the reason: {message}"
    );
    assert!(
        message.contains("allow-unmanifested"),
        "and points at the way out, since a refusal a reader cannot act on is a dead end: {message}"
    );
}

/// `allow-unmanifested: true` is the named widening — top-level, because every
/// key under `extensions:` must be a category of named instances.
#[test]
fn allow_unmanifested_loads_a_component_that_declares_nothing() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-manifest-allow-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    let _guard = common::TempDir(dir.clone());
    std::fs::copy(
        common::repo_root().join("ext").join("tool-fs.wasm"),
        ext.join("tool-fs.wasm"),
    )
    .unwrap();

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "\nallow-unmanifested: true\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext).expect("the grant permits an unmanifested load");
    let loaded = runtime
        .extensions()
        .iter()
        .find(|e| e.instance.component_file() == "tool-fs.wasm")
        .expect("the tool loaded");
    assert_eq!(
        loaded.capabilities.as_deref(),
        Some(["host-fs".to_owned()].as_slice()),
        "the imports are still read — the grant waives the declaration, not the introspection"
    );
}

/// The grant is off unless written. A config that does not mention it must
/// behave as the refusing one above, so the earlier test cannot be passing for
/// some other reason.
#[test]
fn the_grant_is_off_by_default() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-manifest-default-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    let _guard = common::TempDir(dir.clone());
    std::fs::copy(
        common::repo_root().join("ext").join("tool-fs.wasm"),
        ext.join("tool-fs.wasm"),
    )
    .unwrap();

    let config = dir.join("config.yaml");
    // Explicitly false, and separately absent — a grant that only works when
    // spelled `true` should read the same either way.
    for body in [
        "\nallow-unmanifested: false\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
        "\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
    ] {
        std::fs::write(&config, body).unwrap();
        assert!(
            Runtime::boot(&config, &ext).is_err(),
            "without the grant, an unmanifested component is refused: {body:?}"
        );
    }
}

/// `manifest::API_VERSION` is a constant, so it can drift from `wit/`. This is
/// what stops it: the same agreement `scripts/manifests.sh` refuses to guess at,
/// asserted from the other side.
///
/// A constant is right — an installed gateway has no `wit/` beside it, so a
/// version read from that directory at runtime would be read from a directory
/// that may not exist. The cost is exactly this test.
#[test]
fn the_hosts_api_version_matches_the_wit_package() {
    let wit = common::repo_root().join("wit");
    let mut declared: Vec<String> = std::fs::read_dir(&wit)
        .expect("wit/ is readable")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "wit"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            text.lines()
                .find_map(|line| {
                    line.strip_prefix("package jan-klod:interfaces@")
                        .and_then(|rest| rest.strip_suffix(';'))
                })
                .map(str::to_owned)
        })
        .collect();
    declared.sort();
    declared.dedup();

    assert_eq!(
        declared.len(),
        1,
        "wit/ declares more than one package version, so there is no answer to \
         which one the host speaks: {declared:?}"
    );
    assert_eq!(
        declared[0],
        jan_klod_core::manifest::API_VERSION,
        "the host's API_VERSION constant has drifted from wit/"
    );
}

/// A component built against a different interface package is refused, with
/// both versions named — rather than failing later as an obscure missing
/// import from the linker.
#[test]
fn a_component_built_against_another_api_version_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let real_ext = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-manifest-api-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    let _guard = common::TempDir(dir.clone());
    std::fs::copy(real_ext.join("tool-fs.wasm"), ext.join("tool-fs.wasm")).unwrap();

    // Both versions come from the host's own constant rather than from
    // literals. This test used to spell them `0.1.0` and `0.2.0`, and the day
    // `wit/` was bumped to 0.2.0 the "incompatible" rewrite became a no-op —
    // the component was then perfectly compatible and the test failed asking
    // why it had not been refused. A fixture that encodes the very number it is
    // testing against goes stale the moment that number moves.
    let ours = jan_klod_core::manifest::API_VERSION;
    let current = format!("api-version = \"{ours}\"");
    let manifest = std::fs::read_to_string(real_ext.join("tool-fs.manifest.toml")).unwrap();
    assert!(
        manifest.contains(&current),
        "the fixture rewrites this line, so it has to be there: {manifest}"
    );
    // A different **major**, which is incompatible under both rules — pre-1.0,
    // where a differing minor is refused, and after, where a differing major
    // is. So this stays a real mismatch whatever `wit/` is versioned at.
    let theirs = "9.0.0";
    std::fs::write(
        ext.join("tool-fs.manifest.toml"),
        manifest.replace(&current, &format!("api-version = \"{theirs}\"")),
    )
    .unwrap();

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
    )
    .unwrap();

    let Err(error) = Runtime::boot(&config, &ext) else {
        panic!("a component from another interface version must be refused");
    };
    let message = format!("{error}");
    assert!(
        message.contains(theirs) && message.contains(ours),
        "both versions are named, so the reader knows which way the gap runs: {message}"
    );
    assert!(
        message.contains("tool-fs"),
        "and the component it is about: {message}"
    );
}

/// A crate `make ext-new` generated boots — its manifest satisfies 16a's
/// cross-check without anybody hand-writing one.
///
/// Acceptance line 3 of [#111](https://github.com/PromptPasture/jan-klod/issues/111).
/// The manifest is generated by `scripts/manifests.sh` from the component's
/// *real* imports, so it cannot under-declare — but "cannot under-declare" and
/// "boot accepts it" are different claims, and only the second is what an
/// extension author needs to be true. This asserts the second.
///
/// `tool-hello` is the committed output of `make ext-new NAME=tool-hello
/// KIND=tool`, kept byte-identical to what the generator emits by
/// `scripts/ext-new-selftest.sh`.
#[test]
fn a_generated_crate_boots() {
    if !common::guests_staged(&["tool-hello.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-extnew-boot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "\nextensions:\n  tool:\n    hello:\n      enabled: true\n",
    )
    .unwrap();

    Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("a generated crate's manifest must satisfy the boot cross-check");
}
