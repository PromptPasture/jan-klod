//! State persists across runtime restarts.
//!
//! Boots a runtime, runs a turn (persisted to SQLite), drops the runtime,
//! boots a fresh one against the same DB, and verifies the transcript survives.
//!
//! Skips if guests are not staged in `ext/`.

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

    // First boot: run a turn, persisted to SQLite.
    {
        let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
        let factory = || common::canned_http("first answer");
        let mut agent = runtime.build_agent(&factory).expect("agent boots");

        let out = agent.run("chat-1", "hello there");
        assert!(
            matches!(out, RunResult::Answered { .. }),
            "the turn completes"
        );
        // Turn is two messages (question + answer) because transcript projects
        // from the event log, not one `{user, answer}` row per turn.
        assert_eq!(
            agent.transcript("chat-1").len(),
            2,
            "the turn's question and answer are recorded"
        );
    } // runtime + agent (and the SQLite connection) dropped here

    assert!(db_path.exists(), "the store persisted a database file");

    // Second boot against the same DB: transcript survives.
    {
        let runtime = Runtime::boot(&config, &ext_dir).expect("runtime reboots");
        let factory = || common::canned_http("unused");
        let agent = runtime.build_agent(&factory).expect("agent reboots");

        let transcript = agent.transcript("chat-1");
        assert_eq!(transcript.len(), 2, "the turn survived the restart");
        assert_eq!(
            transcript
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["hello there", "first answer"],
            "the question and the answer are both intact, in order"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// Relative `storage.path` resolves against config, not cwd.
///
/// Installed jan-klod runs from any user directory. Against cwd, the default
/// `jan-klod.db` would scatter across directories; against config, there is
/// one store per deployment (invisible in checkout where both paths coincide).
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
        "store lands beside config.yaml"
    );
    // cwd is process-wide; parallel tests can't chdir to `elsewhere` safely.
    assert!(
        !elsewhere.join("jan-klod.db").exists(),
        "not in a sibling directory"
    );
}
