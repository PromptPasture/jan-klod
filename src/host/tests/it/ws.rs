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
