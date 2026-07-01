//! Phase 3 Slice 3a exit gate — durable state survives a restart.
//!
//! Boots a `Runtime` against a `config.yaml` whose `store.sqlite` points at a
//! file, runs a turn (whose transcript is persisted host-side), then **drops the
//! whole runtime and boots a fresh one against the same DB file** and reads the
//! transcript back — proving state survives a restart, offline.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::path::PathBuf;

use jan_klod_core::conductor::RunResult;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
    })
}

#[test]
fn transcript_survives_a_runtime_restart() {
    let ext_dir = repo_root().join("ext");
    for guest in ["provider-openai.wasm", "interceptor-intent-router.wasm"] {
        if !ext_dir.join(guest).exists() {
            eprintln!("skipping: {guest} not staged — run `make ext`");
            return;
        }
    }

    let dir = std::env::temp_dir().join(format!("jk-persist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("jan-klod.db");
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  store:
    sqlite:
      enabled: true
      path: {db}
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
        let factory = || canned_http("first answer");
        let mut agent = runtime.build_agent(&factory).expect("agent boots");

        let out = agent.run("chat-1", "hello there");
        assert!(matches!(out, RunResult::Answered { .. }), "the turn completes");
        assert_eq!(agent.transcript("chat-1").len(), 1, "one turn recorded");
    } // runtime + agent (and the SQLite connection) dropped here

    assert!(db_path.exists(), "the store persisted a database file");

    // Second boot against the SAME db file: the transcript is still there.
    {
        let runtime = Runtime::boot(&config, &ext_dir).expect("runtime reboots");
        let factory = || canned_http("unused");
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
