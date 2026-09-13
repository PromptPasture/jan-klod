//! Tool wiring: enabled `tool.*` extensions flow into the fleet.
//! Tests config -> capability -> fleet (offline). Skips when guests not staged.

use jan_klod_core::Runtime;

use crate::common;

#[test]
fn build_agent_wires_enabled_tools_into_the_fleet() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&[
        "provider-openai.wasm",
        "interceptor-intent-router.wasm",
        "tool-fs.wasm",
    ]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-toolwire-{}", std::process::id()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
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
  tool:
    fs:
      enabled: true
workspace: {ws}
",
            ws = workspace.display()
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("ok");
    let mut agent = runtime
        .build_agent(&factory)
        .expect("agent boots with tools");

    assert!(
        agent.tool_names().contains(&"fs".to_string()),
        "the enabled tool.fs should be in the fleet: {:?}",
        agent.tool_names()
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// #59 end-to-end: tools lazy without interceptors to advertise them.
/// `build_agent` leaves tool uninstantiated unless an interceptor asks
/// (via `instantiate_interceptors`). No interceptor on purpose: with
/// `tool-selector`, `build_agent` computes tool advertisement up front
/// (unavoidable eagerness in current design). Dropping interceptors isolates
/// the tool fleet's own laziness.
#[test]
fn a_tool_with_no_interceptor_to_advertise_it_stays_uninstantiated_until_asked() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "tool-fs.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-toollazy-{}", std::process::id()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  tool:
    fs:
      enabled: true
workspace: {ws}
",
            ws = workspace.display()
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("ok");
    let mut agent = runtime
        .build_agent(&factory)
        .expect("agent boots with no interceptors enabled");

    assert!(
        !agent.tools_instantiated(),
        "no interceptor asked for advertisement, so no `tool.fs` instantiation"
    );

    assert!(
        agent.tool_names().contains(&"fs".to_string()),
        "the tool is still there once actually asked for"
    );
    assert!(
        agent.tools_instantiated(),
        "asking for tool names is what instantiates it"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Edit is confirmed before writing. Previously a denylist failed to catch
/// `view`/`replace`/`insert`, letting edits run unasked. Tests on-disk bytes
/// (refused = unchanged, approved = changed), with real anchor from `view`.
#[test]
fn an_edit_is_confirmed_before_it_touches_the_file() {
    if !common::guests_staged(&[
        "provider-openai.wasm",
        "tool-edit.wasm",
        "interceptor-tool-selector.wasm",
        "interceptor-permission.wasm",
    ]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-edit-gate-{}", std::process::id()));
    let _guard = common::TempDir(dir.clone());
    let original = "fn main() {}\n";

    let refused = run_edit_turn(&dir, "refuse", "no");
    assert_eq!(
        refused.contents, original,
        "a refused edit leaves the file untouched"
    );
    // Print the prompt (UI copy review).
    eprintln!("PROMPT: {:?}", refused.asked);
    // Must name the file; "approve tool `edit`" alone tells nothing.
    assert!(
        refused
            .asked
            .iter()
            .any(|q| q.contains("main.rs") && q.contains("// edited")),
        "the prompt names the file and shows the change: {:?}",
        refused.asked
    );
    assert!(
        refused.asked.iter().any(|q| q.contains("edit")),
        "the gate asked about the edit: {:?}",
        refused.asked
    );
    assert!(
        !refused.asked.iter().any(|q| q.contains("view")),
        "viewing is read-only and must not prompt: {:?}",
        refused.asked
    );

    let approved = run_edit_turn(&dir, "approve", "yes");
    assert_ne!(
        approved.contents, original,
        "approved, the same edit goes through (proves refusal wasn't stale anchor)"
    );
    assert!(
        approved.contents.contains("// edited"),
        "and applies: {:?}",
        approved.contents
    );
}

/// Result of one gated-edit turn.
struct EditTurn {
    /// The file's contents afterwards.
    contents: String,
    /// Every question the gate put to the driver.
    asked: Vec<String>,
}

/// One turn: view `main.rs`, replace first line, answer all confirmations.
fn run_edit_turn(root: &std::path::Path, tag: &str, answer: &'static str) -> EditTurn {
    struct Answering {
        answer: &'static str,
        asked: Vec<String>,
    }
    impl jan_klod_core::intercept::Driver for Answering {
        fn ask(&mut self, prompt: &jan_klod_core::intercept::UserPrompt) -> String {
            self.asked.push(prompt.question.clone());
            self.answer.to_string()
        }
    }

    let work = root.join(tag);
    std::fs::create_dir_all(&work).unwrap();
    let target = work.join("main.rs");
    std::fs::write(&target, "fn main() {}\n").unwrap();

    let config = work.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
workspace: {}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  tool:
    edit:
      enabled: true
  interceptor:
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
            work.display()
        ),
    )
    .unwrap();

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        Box::new(move |_m, _u, _h, body, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = match n {
                0 => serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                        {"id":"c1","function":{"name":"edit",
                         "arguments":"{\"op\":\"view\",\"path\":\"main.rs\"}"}}]},
                        "finish_reason":"tool_calls"}]}),
                1 => {
                    // Pull real anchor from `view` (made-up one proves nothing).
                    let anchor = first_anchor(body.unwrap_or_default())
                        .expect("the view result carries an `anchor|lineno|text` line");
                    let args = format!(
                        "{{\"op\":\"replace\",\"path\":\"main.rs\",\"start\":\"{anchor}\",\
                         \"contents\":\"// edited\"}}"
                    );
                    serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                        {"id":"c2","function":{"name":"edit","arguments":args}}]},
                        "finish_reason":"tool_calls"}]})
                }
                _ => serde_json::json!({"choices":[{"message":{"role":"assistant",
                        "content":"done"},"finish_reason":"stop"}]}),
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let mut driver = Answering {
        answer,
        asked: Vec::new(),
    };
    let _ = agent.run_with_driver(&mut driver, "s1", "rewrite main.rs");

    EditTurn {
        contents: std::fs::read_to_string(&target).unwrap(),
        asked: driver.asked,
    }
}

/// Extract first `anchor` from `anchor|lineno|text` in request body.
fn first_anchor(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    // View output JSON-encoded in request; `|` survives, newlines escaped. Scan for `<hex>|<digits>|`.
    let bytes: Vec<char> = text.chars().collect();
    for (i, window) in bytes.windows(12).enumerate() {
        let candidate: String = window.iter().collect();
        let Some((anchor, rest)) = candidate.split_once('|') else {
            continue;
        };
        if anchor.len() >= 6
            && anchor.chars().all(|c| c.is_ascii_hexdigit())
            && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            // Verify whole token (not partial).
            let start = text[..].char_indices().nth(i).map(|(b, _)| b)?;
            let before = text[..start].chars().next_back();
            if before.is_some_and(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            return Some(anchor.to_string());
        }
    }
    None
}
