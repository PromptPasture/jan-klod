//! Does the shipped product actually work? Most tests here build custom
//! `config.yaml` and test doubles. That let boot defects slip through: empty
//! `extensions.tool`, `start_all` failing, REST surface silently denying
//! instead of asking. This file loads the real `config.yaml`, boots the real
//! `Runtime` and REST surface, drives it with the real client library
//! (`jan_klod_client`, what the TUI calls). Only provider HTTP and workspace
//! root (temp dir) are faked.
//!
//! Skips when guests not staged in `ext/`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jan_klod_client::{stream_turn, StreamEvent};
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;
use jan_klod_host::serve::Surface;

use crate::common;

/// Stub env vars the shipped config expands (placeholder values; no provider HTTP uses them).
fn stub_env() {
    // Idempotent (all tests set these concurrently).
    #[allow(unsafe_code)]
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GROQ_API_KEY",
        "SEARCH_API_KEY",
    ] {
        unsafe { std::env::set_var(key, "test-placeholder") };
    }
}

/// Shipped `config.yaml` with workspace root pointing at `dir` (test-only edit to avoid repo mutations).
fn shipped_config_with_workspace(dir: &std::path::Path) -> String {
    let shipped = std::fs::read_to_string(common::repo_root().join("config.yaml"))
        .expect("the shipped config.yaml is readable");
    format!("{shipped}\nworkspace: {}\n", dir.display())
}

#[test]
fn every_extension_the_shipped_config_enables_boots_and_starts() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    stub_env();

    let dir = std::env::temp_dir().join(format!("jk-shipped-boot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(&config, shipped_config_with_workspace(&dir)).unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("the shipped config boots");

    // All enabled defaults must be staged (check state, not report string which always says "0 missing").
    let absent: Vec<&str> = runtime
        .extensions()
        .iter()
        .filter(|ext| matches!(ext.state, jan_klod_core::LoadState::Missing(_)))
        .map(|ext| ext.instance.id.as_str())
        .collect();
    assert!(
        absent.is_empty(),
        "every enabled default must be staged; missing: {absent:?}\n{}",
        runtime.report()
    );

    // Every default must instantiate and start (the neutral-linker bug failed this for months).
    // Use `start_all_eager` not `start_all`: since #59 plain `start_all` is lazy for `tool-*`,
    // but shipped tools must truly instantiate. Explicit dependency rather than dropped.
    let started = runtime
        .start_all_eager()
        .expect("every enabled extension starts");
    for expected in [
        "tool.fs",
        "tool.edit",
        "tool.find",
        "interceptor.permission",
    ] {
        assert!(
            started.iter().any(|id| id == expected),
            "{expected} started; got {started:?}"
        );
    }
}

/// #59's boot-plan claim against real config: `start_all` (default) starts
/// providers/interceptors but leaves `tool.*` lazy, while `start_all_eager`
/// starts all. Same runtime, same config, different methods. Paired with the
/// test above so regressions (eager `start_all` or lazy `start_all_eager`) fail.
#[test]
fn the_shipped_configs_tools_are_lazy_under_start_all_but_not_start_all_eager() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    stub_env();

    let dir = std::env::temp_dir().join(format!("jk-shipped-lazy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(&config, shipped_config_with_workspace(&dir)).unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("the shipped config boots");

    let lazy = runtime
        .start_all()
        .expect("providers and interceptors start");
    assert!(
        lazy.iter().any(|id| id == "provider.openai"),
        "the eager categories still start under `start_all`: {lazy:?}"
    );
    assert!(
        lazy.iter().any(|id| id == "interceptor.permission"),
        "the eager categories still start under `start_all`: {lazy:?}"
    );
    for lazy_id in ["tool.fs", "tool.edit", "tool.find"] {
        assert!(
            !lazy.iter().any(|id| id == lazy_id),
            "{lazy_id} must not be instantiated by `start_all`: {lazy:?}"
        );
    }

    // Same runtime forced eager proves the difference is `start_all`'s laziness, not config.
    let eager = runtime
        .start_all_eager()
        .expect("every enabled extension starts");
    for expected in ["tool.fs", "tool.edit", "tool.find"] {
        assert!(
            eager.iter().any(|id| id == expected),
            "{expected} started under `start_all_eager`; got {eager:?}"
        );
    }
}

/// Mock provider: request file write on first call, report done on second.
fn write_then_answer_http() -> HttpFn {
    let calls = Arc::new(AtomicU32::new(0));
    Box::new(move |_m, _u, _h, _b, _t| {
        let n = calls.fetch_add(1, Ordering::Relaxed);
        let body = if n == 0 {
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call-1",
                            "function": {
                                "name": "fs",
                                "arguments": r#"{"op":"write","path":"hello.txt","contents":"hi"}"#
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        } else {
            serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "wrote hello.txt" },
                    "finish_reason": "stop"
                }]
            })
        };
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

/// POST answer to a pending confirmation (second connection, mid-turn).
fn post_answer(port: u16, session: &str, answer: &str) -> String {
    let body = serde_json::json!({ "answer": answer }).to_string();
    let request = format!(
        "POST /session/{session}/answer HTTP/1.1\r\nHost: localhost\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn a_fresh_install_asks_before_writing_and_writes_once_allowed() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    stub_env();

    let dir = std::env::temp_dir().join(format!("jk-shipped-turn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(&config, shipped_config_with_workspace(&dir)).unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("the shipped config boots");
    let factory = write_then_answer_http;
    let mut agent = runtime
        .build_agent(&factory)
        .expect("the shipped config builds an agent");

    // Default tools must reach the model (or the "no tools" defect hides).
    let advertised = agent.tool_names();
    for expected in ["fs", "edit", "find"] {
        assert!(
            advertised.iter().any(|name| name == expected),
            "`{expected}` is advertised to the model; got {advertised:?}"
        );
    }

    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();
    let addr = format!("127.0.0.1:{port}");

    // Real UI library (same as TUI).
    let (result, asked, answer) = surface.serve_while(&mut agent, None, move |_| {
        let mut asked = None;
        let mut answer = String::new();
        let result = stream_turn(
            &addr,
            "shipped-1",
            "create hello.txt",
            &mut |event| match event {
                StreamEvent::Prompt {
                    question, options, ..
                } => {
                    asked = Some((question, options));
                    let _ = post_answer(port, "shipped-1", "yes");
                }
                StreamEvent::Done(text) => answer = text,
                _ => {}
            },
        );
        (result, asked, answer)
    });

    assert!(result.is_ok(), "the streamed turn completed: {result:?}");
    let (question, options) = asked.expect("the user was asked before the write happened");
    assert!(
        question.contains("fs"),
        "the question names the tool: {question}"
    );
    assert!(
        options.iter().any(|o| o == "always"),
        "the standing choices are offered: {options:?}"
    );
    assert_eq!(
        answer, "wrote hello.txt",
        "the turn finished after approval"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("hello.txt")).expect("the approved write happened"),
        "hi"
    );
}

#[test]
fn a_refused_confirmation_leaves_the_workspace_untouched() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    stub_env();

    let dir = std::env::temp_dir().join(format!("jk-shipped-deny-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(&config, shipped_config_with_workspace(&dir)).unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("the shipped config boots");
    let factory = write_then_answer_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();
    let addr = format!("127.0.0.1:{port}");

    let _ = surface.serve_while(&mut agent, None, move |_| {
        stream_turn(&addr, "shipped-2", "create hello.txt", &mut |event| {
            if matches!(event, StreamEvent::Prompt { .. }) {
                let _ = post_answer(port, "shipped-2", "no");
            }
        })
    });

    assert!(
        !dir.join("hello.txt").exists(),
        "a refused write must not reach the filesystem"
    );
}
