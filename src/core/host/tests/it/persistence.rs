//! Durable state survives a restart.
//!
//! Boots a `Runtime` against a `config.yaml` whose `store.sqlite` points at a
//! file, runs a turn (persisted host-side), then **drops the whole runtime and
//! boots a fresh one against the same DB file** and reads the transcript back —
//! offline.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use jan_klod_core::conductor::RunResult;
use jan_klod_core::Runtime;

use crate::common;

#[test]
fn transcript_survives_a_runtime_restart() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-persist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("jan-klod.db");
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
storage:
  path: {db}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    intent-router:
      enabled: true
",
            db = db_path.display()
        ),
    )
    .unwrap();

    // First boot: run a turn; its transcript is persisted to the SQLite file.
    {
        let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
        let factory = || common::canned_http("first answer");
        let mut agent = runtime.build_agent(&factory).expect("agent boots");

        let out = agent.run("chat-1", "hello there");
        assert!(
            matches!(out, RunResult::Answered { .. }),
            "the turn completes"
        );
        assert_eq!(agent.transcript("chat-1").len(), 1, "one turn recorded");
    } // runtime + agent (and the SQLite connection) dropped here

    assert!(db_path.exists(), "the store persisted a database file");

    // Second boot against the SAME db file: the transcript is still there.
    {
        let runtime = Runtime::boot(&config, &ext_dir).expect("runtime reboots");
        let factory = || common::canned_http("unused");
        let agent = runtime.build_agent(&factory).expect("agent reboots");

        let transcript = agent.transcript("chat-1");
        assert_eq!(transcript.len(), 1, "the turn survived the restart");
        assert!(
            transcript[0].value.contains("first answer"),
            "the persisted answer is intact: {}",
            transcript[0].value
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// A relative `storage.path` follows the deployment, not the shell.
///
/// An installed jan-klod runs from whatever directory the user happens to be
/// in. Resolved against cwd, the default `jan-klod.db` would scatter across
/// every directory with a different history each time; resolved against the
/// config, there is one store per deployment. (Invisible in a checkout, where
/// both paths coincide.)
#[test]
fn a_relative_storage_path_resolves_against_the_config_not_the_cwd() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-relpath-{}", std::process::id()));
    let elsewhere = dir.join("some-users-repo");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
storage:
  path: ./jan-klod.db
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("boots");
    let factory = || common::canned_http("hello");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let _ = agent.run("s1", "hi");

    assert!(
        dir.join("jan-klod.db").exists(),
        "the store lands beside config.yaml, where the deployment is"
    );
    // The assertion above carries this test; cwd is process-wide and tests run
    // in parallel, so we can't chdir to `elsewhere` to check more directly.
    assert!(
        !elsewhere.join("jan-klod.db").exists(),
        "and not in a sibling working directory"
    );
}
