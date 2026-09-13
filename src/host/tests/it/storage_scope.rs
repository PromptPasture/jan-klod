//! Storage scope for `interceptor-permission`: standing grants are run-scoped,
//! not persisted (durability is opt-in `persist: true`). Tests: (1) grants don't
//! survive restart, (2) `persist: true` keeps state, (3) components isolate
//! namespaces and can't read session transcripts. Skips when guests not staged.

use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::Runtime;

use crate::common;

const GUESTS: [&str; 3] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// Answer every confirmation with `always` (records standing grant).
struct AlwaysDriver {
    asked: std::rc::Rc<std::cell::RefCell<u32>>,
}
impl Driver for AlwaysDriver {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        *self.asked.borrow_mut() += 1;
        "always".to_string()
    }
}

/// Config with permission gate and optional-persist storage.
fn config_with(dir: &std::path::Path, db: &std::path::Path, persist: bool) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    // Store path makes session durable; `persist` flag lets gate reach it.
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

/// Mock provider: calls bash once, then answers.
fn tool_calling_provider() -> impl Fn() -> jan_klod_core::route::HttpFn {
    // Annotated return type needed for closure's higher-ranked lifetimes.
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

/// Run N turns that trip the gate; return ask count.
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

/// Standing grant is process-scoped: `always` silences the gate within one run,
/// but must ask again after restart.
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

    // Two turns in one process: grant from first silences second.
    let first_process = turns_asked(&config, &ext_dir, 2);
    assert_eq!(
        first_process, 1,
        "a standing grant silences the gate within a run — otherwise `always` means nothing"
    );

    // Restart: fresh Runtime over same DB. Session transcript survives; grant doesn't.
    let second_process = turns_asked(&config, &ext_dir, 1);
    assert_eq!(
        second_process, 1,
        "the gate must ask again after a restart — a click last week is not a standing \
         grant today. If this fails, `host-storage` was made durable by default; \
         durability is a grant (`persist: true`), not the default."
    );
}

/// Opt-in durability: `persist: true` keeps state across restart.
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

    // Restart: same DB, grant was persisted.
    let second_process = turns_asked(&config, &ext_dir, 1);
    assert_eq!(
        second_process, 0,
        "with `persist: true` the standing grant survives, so the gate stays quiet"
    );
}

/// One DB backs opted-in interceptors and session logs. Isolation is enforced:
/// guest namespaces prefixed (can't name other components), session logs in
/// table `host-storage` can't address.
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

    // Read DB directly (guest's view not under test).
    let store = jan_klod_core::store::Store::open(&db).expect("opens the same db");
    let namespaces = store.list_namespaces().expect("lists namespaces");

    // Gate asked for `permission`; must be prefixed to prevent namespace collisions.
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
    // Session in same DB but in `events` table (not `entries`), unreachable by `host-storage`.
    // Isolation: namespace prefix + table door.
    assert!(
        !namespaces.iter().any(|ns| ns == "s1"),
        "the session is not reachable as a namespace any more: {namespaces:?}"
    );
    assert!(
        !store
            .session_events("s1")
            .expect("the log is readable")
            .is_empty(),
        "and it really is in this database, in the event log"
    );

    // list_sessions shows real sessions, not interceptor namespaces.
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
