//! `scripts/manifests.sh` and `Runtime::boot` share no code, so agreeing on each guest's capabilities means both are right.

use jk_agent::Runtime;
use jk_wasm::LoadState;

use crate::common;

const GUESTS: [&str; 6] = [
    "provider-openai.wasm",
    "tool-fs.wasm",
    "tool-shell.wasm",
    "tool-fetch.wasm",
    "tool-escape-probe.wasm",
    "interceptor-permission.wasm",
];

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
    for guest in GUESTS {
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

    assert_eq!(of("tool-fs.wasm"), vec!["host-fs"]);
    assert_eq!(
        of("provider-openai.wasm"),
        vec!["host-config", "host-http", "host-log", "host-secrets"]
    );
    assert_eq!(of("tool-escape-probe.wasm"), vec!["host-log"]);
    assert_eq!(of("tool-shell.wasm"), vec!["host-process"]);
    assert!(
        !of("tool-fs.wasm").contains(&"host-process".to_string()),
        "a file tool cannot run a command, and the import set says so"
    );
}

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

    let factory = || common::canned_http("unused");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let out = agent.run("s", "hello");
    assert!(
        matches!(out, jk_agent::conductor::RunResult::Answered { .. })
            || matches!(out, jk_agent::conductor::RunResult::Failed(_)),
        "the turn ran one way or the other; what matters is that boot did not refuse"
    );
}

#[test]
fn a_component_with_no_manifest_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-manifest-none-{}", std::process::id()));
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
        jk_wasm::manifest::API_VERSION,
        "the host's API_VERSION constant has drifted from wit/"
    );
}

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

    let ours = jk_wasm::manifest::API_VERSION;
    let current = format!("api-version = \"{ours}\"");
    let manifest = std::fs::read_to_string(real_ext.join("tool-fs.manifest.toml")).unwrap();
    assert!(
        manifest.contains(&current),
        "the fixture rewrites this line, so it has to be there: {manifest}"
    );
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
