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
