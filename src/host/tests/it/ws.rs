//! `GET /ws` exists, is guarded, and negotiates (#226, closing #43).
//!
//! The socket carries the same protocol as stdio because it is dispatched
//! by the same function — `rpc::command`. So what is worth testing here is
//! not that `protocol/hello` produces the right answer (`rpc.rs`'s own
//! tests cover that) but that a *WebSocket* reaches it: the upgrade
//! happens, the token rule applies to it like every other route, and a
//! frame in produces a frame out.

use futures_util::{SinkExt as _, StreamExt as _};
use jan_klod_core::Runtime;
use jan_klod_host::serve::Surface;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::Message;

use crate::common;

const TOKEN: &str = "ws-token";

/// An agent with nothing but a provider: this is about the socket.
fn booted(tag: &str) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-ws-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the temp dir");
    let guard = common::TempDir(dir.clone());
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
",
            db = dir.join("jan-klod.db").display()
        ),
    )
    .expect("writes the config");
    let runtime =
        Runtime::boot(&config, common::repo_root().join("ext")).expect("the runtime boots");
    let agent = runtime
        .build_agent(&|| common::canned_http("pong"))
        .expect("the agent boots");
    Some((guard, agent))
}

/// Connect, send `frames`, and collect what comes back — one runtime per
/// call, because the test's thread is the session's and cannot be async.
fn talk(port: u16, token: Option<&str>, frames: Vec<String>) -> Result<Vec<String>, String> {
    let token = token.map(str::to_owned);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(async move {
            let url = format!("ws://127.0.0.1:{port}/ws");
            let request = {
                let mut request = url.into_client_request().map_err(|err| err.to_string())?;
                if let Some(token) = token {
                    request.headers_mut().insert(
                        "authorization",
                        format!("Bearer {token}")
                            .parse()
                            .map_err(|_| "a header value".to_owned())?,
                    );
                }
                request
            };
            let (mut socket, _) = tokio_tungstenite::connect_async(request)
                .await
                .map_err(|err| err.to_string())?;

            let mut answers = Vec::new();
            for frame in frames {
                socket
                    .send(Message::text(frame))
                    .await
                    .map_err(|err| err.to_string())?;
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => answers.push(text.to_string()),
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err.to_string()),
                    None => break,
                }
            }
            let _ = socket.close(None).await;
            Ok(answers)
        })
}

/// A framed request, ready to send.
fn frame(id: u32, method: &str) -> String {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method }).to_string()
}

/// [`talk`], for frames that are not all text.
fn talk_mixed(port: u16, frames: Vec<Message>) -> Result<Vec<String>, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(async move {
            let url = format!("ws://127.0.0.1:{port}/ws");
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .map_err(|err| err.to_string())?;
            let mut answers = Vec::new();
            for frame in frames {
                socket.send(frame).await.map_err(|err| err.to_string())?;
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => answers.push(text.to_string()),
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err.to_string()),
                    None => break,
                }
            }
            let _ = socket.close(None).await;
            Ok(answers)
        })
}

/// Send `frames` and read until the socket closes, so a turn's
/// notifications are collected as well as its answers.
///
/// Unlike [`talk`], which reads one answer per frame: a turn emits
/// notifications *while* it runs, and counting them one-to-one against
/// what was sent would deadlock on the first one.
fn talk_until(port: u16, frames: Vec<String>, gap: std::time::Duration, done: &str) -> Vec<String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(async move {
            let url = format!("ws://127.0.0.1:{port}/ws");
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connects");
            for frame in frames {
                socket.send(Message::text(frame)).await.expect("sends");
                // Let the previous frame take effect: a `turn/cancel`
                // sent before the turn has started would arrive as an
                // ordinary command and be answered, not act on a turn.
                tokio::time::sleep(gap).await;
            }
            let mut seen = Vec::new();
            // Stops at `done` rather than on a drain timeout, so the time
            // this takes is the turn's and not the reader's — which is
            // what lets a caller assert on elapsed.
            while let Ok(Some(Ok(message))) =
                tokio::time::timeout(std::time::Duration::from_secs(30), socket.next()).await
            {
                match message {
                    Message::Text(text) => {
                        let text = text.to_string();
                        let finished = text.contains(done);
                        seen.push(text);
                        if finished {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            let _ = socket.close(None).await;
            seen
        })
}

fn hello() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "protocol/hello",
        "params": { "version": jan_klod_protocol::PROTOCOL_VERSION },
    })
    .to_string()
}

/// The socket exists, and one frame in produces one frame out.
///
/// The answer's *content* is `rpc`'s and is tested there. What this shows
/// is that a WebSocket frame reached it at all — the upgrade, the
/// dispatch, and the way back.
#[test]
fn a_client_connects_and_negotiates_the_protocol() {
    let Some((_guard, mut agent)) = booted("hello") else {
        return;
    };
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let answers = surface.serve_while(&mut agent, None, |port| {
        talk(port, None, vec![hello()]).expect("the socket answers")
    });

    assert_eq!(answers.len(), 1, "one frame in, one out: {answers:?}");
    assert!(
        answers[0].contains(jan_klod_protocol::PROTOCOL_VERSION),
        "the handshake carries the version: {}",
        answers[0]
    );
    assert!(
        !answers[0].contains("\"error\""),
        "the handshake was refused: {}",
        answers[0]
    );
}

/// The token rule reaches the socket, because the upgrade happens behind
/// the same layer every other route is behind.
///
/// Its control is the test above: without a working connection, "refused"
/// would be indistinguishable from "the socket does not exist".
#[test]
fn the_socket_is_refused_without_the_token() {
    let Some((_guard, mut agent)) = booted("token") else {
        return;
    };
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let (refused, accepted) = surface.serve_while(&mut agent, Some(TOKEN), |port| {
        let refused = talk(port, None, vec![hello()]);
        let accepted = talk(port, Some(TOKEN), vec![hello()]);
        (refused, accepted)
    });

    assert!(
        refused.is_err(),
        "an unauthenticated client upgraded: {refused:?}"
    );
    let accepted = accepted.expect("the authenticated client connects");
    assert!(
        accepted[0].contains(jan_klod_protocol::PROTOCOL_VERSION),
        "and gets the handshake: {accepted:?}"
    );
}

/// A verb that needs the session is answered over the socket, and the
/// handshake it needed first is remembered across frames.
///
/// The second half is socket-specific and is the part worth testing: the
/// negotiated flag lives in the connection loop and is handed into each
/// job and read back out, because a job cannot borrow it. If that came
/// back wrong, every command after the first would be refused — which is
/// exactly what a client would report as "it works once".
#[test]
fn a_session_verb_is_answered_and_the_handshake_is_remembered() {
    let Some((_guard, mut agent)) = booted("verbs") else {
        return;
    };
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let answers = surface.serve_while(&mut agent, None, |port| {
        talk(
            port,
            None,
            vec![
                hello(),
                frame(2, "session/create"),
                frame(3, "session/list"),
            ],
        )
        .expect("the socket answers")
    });

    assert_eq!(answers.len(), 3, "one answer per frame: {answers:?}");
    for (n, answer) in answers.iter().enumerate() {
        assert!(
            !answer.contains("\"error\""),
            "frame {n} was refused, so the handshake did not carry: {answer}"
        );
    }
    assert!(
        answers[2].contains("sessions"),
        "session/list answered with a session list: {}",
        answers[2]
    );
}

/// A command before the handshake is refused — the protocol's rule, not
/// the socket's, and it reaches the socket because the dispatch is shared.
#[test]
fn a_command_before_the_handshake_is_refused() {
    let Some((_guard, mut agent)) = booted("premature") else {
        return;
    };
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let answers = surface.serve_while(&mut agent, None, |port| {
        talk(port, None, vec![frame(1, "session/list")]).expect("the socket answers")
    });
    assert!(
        answers[0].contains("\"error\""),
        "an un-negotiated command was served: {}",
        answers[0]
    );
}

/// A frame that is not this protocol is answered, and **the connection
/// continues** — `rpc::serve`'s rule, and the reason one bad client
/// cannot take a session down.
///
/// Three kinds in one connection: not JSON, JSON that is not a request,
/// and a binary frame. The frame after them all is the assertion: it is
/// answered normally, so none of the three closed the socket.
#[test]
fn a_bad_frame_is_answered_and_the_connection_survives_it() {
    let Some((_guard, mut agent)) = booted("bad-frames") else {
        return;
    };
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let answers = surface.serve_while(&mut agent, None, |port| {
        talk_mixed(
            port,
            vec![
                Message::text(hello()),
                Message::text("not json at all".to_owned()),
                Message::text(serde_json::json!({ "hello": "world" }).to_string()),
                Message::binary(vec![0x00, 0x01, 0x02]),
                Message::text(frame(9, "session/list")),
            ],
        )
        .expect("the socket answers")
    });

    assert_eq!(answers.len(), 5, "every frame was answered: {answers:?}");
    for (n, answer) in answers.iter().enumerate().take(4).skip(1) {
        assert!(
            answer.contains("\"error\""),
            "frame {n} should have been refused: {answer}"
        );
    }
    assert!(
        !answers[4].contains("\"error\"") && answers[4].contains("sessions"),
        "the connection did not survive three bad frames: {}",
        answers[4]
    );
}

/// An agent whose first completion calls a gated tool, so a turn parks on
/// an `ask` — and whose second finishes.
fn booted_asking(tag: &str) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    let needed = [
        "provider-openai.wasm",
        "interceptor-tool-selector.wasm",
        "interceptor-permission.wasm",
    ];
    if !common::guests_staged(&needed) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-ws-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the temp dir");
    let guard = common::TempDir(dir.clone());
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
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
            db = dir.join("jan-klod.db").display()
        ),
    )
    .expect("writes the config");

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"bash","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"all done"},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).expect("serialises"),
            })
        })
    };
    let runtime =
        Runtime::boot(&config, common::repo_root().join("ext")).expect("the runtime boots");
    let agent = runtime.build_agent(&factory).expect("the agent boots");
    Some((guard, agent))
}

/// A turn over the socket: it streams, it asks in band, and the answer
/// arrives on the same connection.
///
/// This is what a WebSocket buys over SSE plus POST — #43 said so in its
/// own correction, and until now nothing demonstrated it. The answer goes
/// down the *same socket* the question came up, which over REST needs a
/// second connection and a route.
#[test]
fn a_turn_streams_and_its_ask_is_answered_in_band() {
    // Long on purpose: if the in-band answer works the turn finishes in
    // about a second, and if it does not this test takes the whole
    // timeout. The elapsed assertion below is what tells them apart —
    // "all done" alone cannot, since the mock provider says it either
    // way.
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "60");
    let started = std::time::Instant::now();
    let Some((_guard, mut agent)) = booted_asking("turn") else {
        return;
    };
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let seen = surface.serve_while(&mut agent, None, |port| {
        talk_until(
            port,
            vec![
                hello(),
                serde_json::json!({"jsonrpc":"2.0","id":2,"method":"session/message",
                    "params":{"session":"w-1","message":"use bash to clean up"}})
                .to_string(),
                serde_json::json!({"jsonrpc":"2.0","id":3,"method":"turn/answer",
                    "params":{"session":"w-1","answer":"yes"}})
                .to_string(),
            ],
            std::time::Duration::from_millis(400),
            "all done",
        )
    });

    let elapsed = started.elapsed();
    let all = seen.join("\n");
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the turn took {elapsed:?}, which is the answer timeout rather than an \
         answer: the in-band reply never reached the parked ask"
    );
    assert!(
        all.contains("notification/ask") || all.contains("\"ask\""),
        "the turn asked over the socket: {all}"
    );
    assert!(
        all.contains("all done"),
        "the turn finished after being answered in band: {all}"
    );
}

/// A `turn/cancel` is read **while the turn runs**, which is the other
/// thing a WebSocket buys over SSE plus POST.
///
/// Two assertions, and the second is the control. The cancel being
/// *accepted* (`cancelling: true`) proves the frame was read mid-turn
/// rather than queued behind it — a socket that only dispatched between
/// turns would answer this after the turn had already finished. And the
/// turn's own answer proves the cancel *did* something: it ends stopped,
/// not with the "all done" the mock provider would have produced if the
/// turn had run to completion.
#[test]
fn a_turn_is_cancelled_over_the_same_socket_while_it_runs() {
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "60");
    let Some((_guard, mut agent)) = booted_asking("cancel") else {
        return;
    };
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let seen = surface.serve_while(&mut agent, None, |port| {
        talk_until(
            port,
            vec![
                hello(),
                serde_json::json!({"jsonrpc":"2.0","id":2,"method":"session/message",
                    "params":{"session":"c-1","message":"use bash to clean up"}})
                .to_string(),
                serde_json::json!({"jsonrpc":"2.0","id":3,"method":"turn/cancel",
                    "params":{"session":"c-1"}})
                .to_string(),
            ],
            std::time::Duration::from_millis(400),
            "\"method\":\"done\"",
        )
    });

    let all = seen.join("\n");
    assert!(
        all.contains("\"cancelling\":true"),
        "the cancel was not read while the turn ran: {all}"
    );
    assert!(
        all.contains("stopped before the turn finished"),
        "the turn ran to completion anyway: {all}"
    );
    assert!(
        !all.contains("all done"),
        "the turn finished normally, so the cancel changed nothing: {all}"
    );
}
