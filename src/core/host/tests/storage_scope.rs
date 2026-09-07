//! What an interceptor's `host-storage` may outlive, and what it may see.
//!
//! `interceptor-permission` records standing grants — "always allow `fs:write`"
//! — through `host-storage`, and its own documentation states the property that
//! makes them safe: **run-scoped, never persisted**, because "a permission
//! boundary should not quietly become permanently open because of a click last
//! week."
//!
//! Nothing enforced that. It held because the host happened to back
//! `host-storage` with a private `HashMap`, three fields away from an open
//! `SQLite` connection holding session transcripts. Wiring the two together is a
//! one-line change that reads like a bug fix — the store is *right there*, and an
//! interceptor that wants to remember a context summary has nowhere else to put
//! it — and it would have repealed the security property with nothing failing.
//!
//! So durability is now a grant, off by default, and this file is the thing that
//! notices if that default flips:
//!
//! 1. A standing grant does not survive a restart. (The documented property.)
//! 2. An instance that asks for `persist: true` does keep its state.
//! 3. Neither instance can read the other's namespace, or a session transcript.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::Runtime;

mod common;

const GUESTS: [&str; 3] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// Answers every confirmation with `always`, so the gate records a standing grant.
struct AlwaysDriver {
    asked: std::rc::Rc<std::cell::RefCell<u32>>,
}
impl Driver for AlwaysDriver {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        *self.asked.borrow_mut() += 1;
        "always".to_string()
    }
}

/// A config whose permission gate is enabled, with a durable store on disk.
fn config_with(dir: &std::path::Path, db: &std::path::Path, persist: bool) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    // `store.sqlite.path` is what makes the session store durable; `persist` is
    // what decides whether the permission gate may reach it.
    let persist_line = if persist { "\n      persist: true" } else { "" };
    std::fs::write(
        &path,
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
    tool-selector:
      enabled: true
    permission:
      enabled: true{persist_line}
",
            db = db.display()
        ),
    )
    .unwrap();
    path
}

/// A provider that calls a dangerous tool once, then answers.
fn tool_calling_provider() -> impl Fn() -> jan_klod_core::route::HttpFn {
    // The return type is annotated rather than cast: without it the closure's
    // higher-ranked lifetimes do not unify with `HttpFn`.
    move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::atomic::AtomicU32::new(0);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"bash","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"done"},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    }
}

/// Run one turn that trips the gate; return how many times it asked.
fn turns_asked(config: &std::path::Path, ext_dir: &std::path::Path, turns: u32) -> u32 {
    let runtime = Runtime::boot(config, ext_dir).expect("runtime boots");
    let factory = tool_calling_provider();
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let asked = std::rc::Rc::new(std::cell::RefCell::new(0));
    for _ in 0..turns {
        let mut driver = AlwaysDriver {
            asked: std::rc::Rc::clone(&asked),
        };
        let _ = agent.run_with_driver(&mut driver, "s1", "use bash");
    }
    let count = *asked.borrow();
    count
}

/// The property `interceptor-permission` documents, now enforced.
///
/// Within one process, answering `always` silences the gate — that is the whole
/// point of a standing grant. Across a restart it must ask again.
#[test]
fn a_standing_grant_does_not_survive_a_restart() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-scope-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let db = dir.join("jan-klod.db");
    let ext_dir = common::repo_root().join("ext");
    let config = config_with(&dir, &db, false);

    // Two turns in one process: the grant from the first silences the second.
    let first_process = turns_asked(&config, &ext_dir, 2);
    assert_eq!(
        first_process, 1,
        "a standing grant silences the gate within a run — otherwise `always` means nothing"
    );

    // A second `Runtime` against the *same database* is a restart. The session
    // transcript survives it; the grant must not.
    let second_process = turns_asked(&config, &ext_dir, 1);
    assert_eq!(
        second_process, 1,
        "the gate must ask again after a restart — a click last week is not a standing \
         grant today. If this fails, `host-storage` was made durable by default; \
         durability is a grant (`persist: true`), not the default."
    );
}

/// The other half: an interceptor that *asks* for durability gets it, so the
/// default-off is a policy and not a missing feature.
#[test]
fn an_instance_that_opts_in_keeps_its_state_across_a_restart() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-scope-persist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let db = dir.join("jan-klod.db");
    let ext_dir = common::repo_root().join("ext");
    let config = config_with(&dir, &db, true);

    let first_process = turns_asked(&config, &ext_dir, 1);
    assert_eq!(first_process, 1, "the first run asks once");

    // Same database, new process — the grant was written through to it.
    let second_process = turns_asked(&config, &ext_dir, 1);
    assert_eq!(
        second_process, 0,
        "with `persist: true` the standing grant survives, so the gate stays quiet"
    );
}

/// One database now backs every opted-in interceptor *and* the session
/// transcripts. Isolation therefore has to be enforced rather than assumed.
#[test]
fn a_namespace_a_guest_can_name_never_reaches_another_components_data() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-scope-iso-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let db = dir.join("jan-klod.db");
    let ext_dir = common::repo_root().join("ext");
    let config = config_with(&dir, &db, true);

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = tool_calling_provider();
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let asked = std::rc::Rc::new(std::cell::RefCell::new(0));
    let mut driver = AlwaysDriver { asked };
    let _ = agent.run_with_driver(&mut driver, "s1", "use bash");

    // Read the database directly — the guest's view is not the one under test.
    let store = jan_klod_core::store::Store::open(&db).expect("opens the same db");
    let namespaces = store.list_namespaces().expect("lists namespaces");

    // The gate asked for namespace `permission`; it must not have got it.
    assert!(
        !namespaces.iter().any(|ns| ns == "permission"),
        "a guest's namespace must be prefixed with its component id, not taken \
         verbatim — otherwise two interceptors that both pick `state` share one: {namespaces:?}"
    );
    assert!(
        namespaces
            .iter()
            .any(|ns| ns.starts_with("ext/interceptor.permission/")),
        "the grant landed under the component's own subtree: {namespaces:?}"
    );
    // And the session transcript is a namespace the guest could have *named*
    // (`s1`) but cannot reach, because it never writes the prefix.
    assert!(
        namespaces.iter().any(|ns| ns == "s1"),
        "the transcript is in the same database: {namespaces:?}"
    );

    // `list_sessions` must not offer interceptor storage as a conversation.
    let sessions = agent.list_sessions();
    assert!(
        sessions.iter().any(|s| s == "s1"),
        "the real session lists: {sessions:?}"
    );
    assert!(
        !sessions.iter().any(|s| s.contains("ext/")),
        "interceptor namespaces are not sessions: {sessions:?}"
    );
}
