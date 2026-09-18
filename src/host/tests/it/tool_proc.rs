//! The model drives a long-lived child, across turns (#220).
//!
//! `execution_config.rs` proves the *host* holds a child open for a guest.
//! This is the model-facing half: three turns of one session start a child,
//! read what it printed, and stop it — and the turn boundary is the point,
//! because a handle that did not survive it would make the tool useless for
//! the thing it exists for.
//!
//! Everything is driven through a booted `Runtime` with a canned provider
//! emitting tool calls, so the path is config → fleet → `tool-proc` →
//! `host-process` and back into the model's context.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_core::Runtime;

use crate::common;
use crate::execution_config::pid_alive;

/// The guests a turn through `tool-proc` needs.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "tool-proc.wasm"];

/// A child that prints once and then stays up.
///
/// It prints *without being written to*, unlike the `cat` the host-side
/// tests use: `tool-proc` deliberately exposes no way to write to a child,
/// so a child that only echoes would have nothing to read. `exec` replaces
/// the shell so the pid that is printed is the pid that stays.
const TICKER: &str = "execution:\n  enabled: true\n  long-lived:\n    - name: ticker\n      \
                      command: sh\n      args: [\"-c\", \"echo hello-from-child; exec sleep 30\"]\n";

/// What the model saw, one entry per turn.
struct Turns {
    /// The tool result carried into each turn's follow-up completion.
    results: Vec<String>,
}

impl Turns {
    fn result(&self, turn: usize) -> &str {
        self.results
            .get(turn)
            .map_or("<no such turn>", String::as_str)
    }
}

/// Run one turn per entry in `calls`, in a single session, with the same
/// fleet — which is what makes this a test of the turn boundary rather than
/// of three separate runs.
fn run_turns(execution: &str, calls: &[serde_json::Value]) -> Turns {
    let dir = std::env::temp_dir().join(format!(
        "jk-toolproc-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
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
    proc:
      enabled: true
workspace: {ws}
{execution}
",
            ws = workspace.display()
        ),
    )
    .unwrap();

    // Two completions per turn: the even one asks for the tool, the odd one
    // carries its result and ends the turn. So the odd bodies are what the
    // model was told, which is the only thing worth asserting on.
    let arguments: Vec<String> = calls.iter().map(serde_json::Value::to_string).collect();
    let nth = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let (counter, recorded) = (Arc::clone(&nth), Arc::clone(&seen));
    let factory = move || -> jan_klod_core::route::HttpFn {
        let counter = Arc::clone(&counter);
        let recorded = Arc::clone(&recorded);
        let arguments = arguments.clone();
        Box::new(move |_m, _u, _h, body, _t| {
            let n = counter.fetch_add(1, Ordering::Relaxed) as usize;
            let response = if n.is_multiple_of(2) {
                let call = arguments.get(n / 2).cloned().unwrap_or_default();
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"proc","arguments":call}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                if let Ok(mut log) = recorded.lock() {
                    log.push(String::from_utf8_lossy(body.unwrap_or_default()).into_owned());
                }
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"done"},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("runtime boots")
        .with_sandbox_wrapper(env!("CARGO_BIN_EXE_jan-klod-gateway"));
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    for turn in 0..calls.len() {
        let _ = agent.run("s1", &format!("turn {turn}"));
    }

    let results = seen.lock().expect("not poisoned").clone();
    Turns { results }
}

/// The acceptance: started in one turn, read in the next, stopped in a third.
///
/// The turn boundary is the whole assertion. A handle kept anywhere that did
/// not outlive a turn would pass a single-turn test and fail this one, and
/// "start a dev server, then look at it" is the use this tool exists for.
#[test]
fn a_child_starts_in_one_turn_is_read_in_the_next_and_stops_in_a_third() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turns = run_turns(
        TICKER,
        &[
            serde_json::json!({ "op": "start", "name": "ticker" }),
            serde_json::json!({ "op": "output", "name": "ticker" }),
            serde_json::json!({ "op": "stop", "name": "ticker" }),
        ],
    );

    assert!(
        turns.result(0).contains("\\\"started\\\":true"),
        "the child did not start: {}",
        turns.result(0)
    );
    assert!(
        turns.result(1).contains("hello-from-child"),
        "a later turn could not read the child it started: {}",
        turns.result(1)
    );
    assert!(
        turns.result(1).contains("\\\"running\\\":true"),
        "output must say whether the child is still alive: {}",
        turns.result(1)
    );
    assert!(
        turns.result(2).contains("\\\"stopped\\\":true"),
        "the child could not be stopped: {}",
        turns.result(2)
    );
}

/// A name the operator never wrote down is refused — and the refusal says
/// what *is* available, because a model that cannot see the alternatives
/// cannot correct itself.
#[test]
fn an_ungranted_name_is_refused_and_the_refusal_names_what_is_granted() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turns = run_turns(
        TICKER,
        &[serde_json::json!({ "op": "start", "name": "not-in-the-config" })],
    );
    assert!(
        turns.result(0).contains("refused"),
        "an ungranted name was not refused: {}",
        turns.result(0)
    );
    assert!(
        turns.result(0).contains("ticker"),
        "the refusal must name what is available: {}",
        turns.result(0)
    );
}

/// The control the refusal needs: with no grant at all there is nothing to
/// list, so the test above passes against a tool that refuses everything
/// unless a granted name really does start.
///
/// Also the `list` op's own coverage: the model can see the fleet of
/// children before naming one.
#[test]
fn a_granted_name_is_listed_before_the_model_asks_for_it() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turns = run_turns(TICKER, &[serde_json::json!({ "op": "list" })]);
    assert!(
        turns.result(0).contains("ticker"),
        "the granted child was not listed: {}",
        turns.result(0)
    );
    assert!(
        turns.result(0).contains("\\\"running\\\":false"),
        "a child nobody started must not be reported as running: {}",
        turns.result(0)
    );
}

/// The existing guarantee, re-asserted from the model's side: a child the
/// *model* started and never stopped is still the host's to reap.
///
/// `execution_config.rs` proves this for a guest that leaks a handle. The
/// difference here is who forgot — and a model forgetting is the ordinary
/// case rather than the exotic one, so this must hold without anything in
/// the guest cooperating.
#[test]
fn a_child_the_model_forgot_does_not_outlive_the_runtime() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    // Prints its own pid, then stays up. `sleep` ignores stdin, so nothing
    // but a kill ends it inside thirty seconds.
    let grant = "execution:\n  enabled: true\n  long-lived:\n    - name: reporter\n      \
                 command: sh\n      args: [\"-c\", \"echo $$; exec sleep 30\"]\n";
    let turns = run_turns(
        grant,
        &[
            serde_json::json!({ "op": "start", "name": "reporter" }),
            serde_json::json!({ "op": "output", "name": "reporter" }),
        ],
    );

    let pid: u32 = turns
        .result(1)
        .split("\\\"output\\\":\\\"")
        .nth(1)
        .and_then(|tail| {
            tail.split(|c: char| !c.is_ascii_digit())
                .find(|digits| !digits.is_empty())
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("the child must report its pid: {}", turns.result(1)));

    assert!(
        !pid_alive(pid),
        "pid {pid} is still running after the runtime was dropped — the model \
         never stopped it, so the host had to"
    );
}
