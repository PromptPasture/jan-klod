//! The **shipped defaults** gate: does the product a user installs actually work?
//!
//! Every other test in this tree builds its own `config.yaml` and, where it needs a
//! driver or a tool, supplies a test double. That is the right shape for testing a
//! mechanism — and it is exactly why three separate defects survived a green suite:
//!
//! - `extensions.tool` shipped empty, so a fresh install had no tools at all;
//! - `Runtime::start_all` could not instantiate any tool/registry/interceptor, so
//!   the default boot path failed on the shipped config;
//! - the REST surface answered every confirmation with `HeadlessDriver`, so the
//!   permission gate silently denied instead of asking.
//!
//! Each was invisible to a suite that never loaded the real `config.yaml` and never
//! ran the production driver. This file closes that gap: it loads the repo's own
//! `config.yaml`, boots the real `Runtime`, serves the real REST surface, and drives
//! it with the **real UI client library** (`jan_klod_client`, the `jan-klod` UI
//! crate renamed to dodge the probe example's generated bindings) — the same
//! `stream_turn` a user's TUI
//! calls. Only two things are faked, and only because a test may not do them for
//! real: the provider's HTTP (no network, no tokens) and the workspace root (a temp
//! directory, so the test cannot write into the repo it is running from).
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_client::{stream_turn, StreamEvent};
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::serve::serve_once;
use jan_klod_core::Runtime;
use tiny_http::Server;

mod common;

/// The env vars the shipped config expands. Set to placeholders: the provider's
/// HTTP is faked, so no key is ever used — but boot must not fail on a missing one.
fn stub_env() {
    // Every test in this binary sets the same values before any `Runtime::boot`,
    // so the writes are idempotent even though the tests run concurrently.
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

/// The shipped `config.yaml`, with a workspace root pointed at `dir`.
///
/// This is the *only* edit to the real file: the workspace key is commented out
/// upstream (it defaults to `$PWD`), and a test that wrote into `$PWD` would be
/// editing the repository it runs from.
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

    // Nothing the shipped config enables may be missing from a built `ext/`: a
    // default that names a component nobody builds is a broken install.
    //
    // Asserted on the resolved state, not on the rendered report — the report's
    // summary line says "0 missing", so string-matching it was always true.
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

    // And every one of them must actually instantiate and start. This is the
    // assertion `start_all`'s neutral-linker bug failed for months.
    let started = runtime.start_all().expect("every enabled extension starts");
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

/// A provider that asks to write a file, then reports done — the shape of the
/// first thing anyone tries with a coding agent.
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

/// POST an answer to a pending confirmation (a second connection, mid-turn).
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

    // The default tool set must actually reach the model, or none of the rest of
    // this can happen (the "shipped config had no tools" defect).
    let advertised = agent.tool_names();
    for expected in ["fs", "edit", "find"] {
        assert!(
            advertised.iter().any(|name| name == expected),
            "`{expected}` is advertised to the model; got {advertised:?}"
        );
    }

    let server = Server::http("127.0.0.1:0").expect("binds an ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    let addr = format!("127.0.0.1:{port}");

    // The client is the real UI library — the same call the TUI makes.
    let client = thread::spawn(move || {
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

    serve_once(&server, &mut agent).expect("serves the turn");
    let (result, asked, answer) = client.join().expect("client thread");

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

    let server = Server::http("127.0.0.1:0").expect("binds an ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    let addr = format!("127.0.0.1:{port}");

    let client = thread::spawn(move || {
        stream_turn(&addr, "shipped-2", "create hello.txt", &mut |event| {
            if matches!(event, StreamEvent::Prompt { .. }) {
                let _ = post_answer(port, "shipped-2", "no");
            }
        })
    });
    serve_once(&server, &mut agent).expect("serves the turn");
    let _ = client.join().expect("client thread");

    assert!(
        !dir.join("hello.txt").exists(),
        "a refused write must not reach the filesystem"
    );
}
