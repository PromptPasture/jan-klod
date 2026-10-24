//! `send_turn` against a real socket: a canned HTTP server stands in for `jan-klod serve`.

use std::thread;

use tiny_http::{Response, Server};

#[test]
fn send_turn_round_trips_against_a_server() {
    let server = Server::http("127.0.0.1:0").expect("binds ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    let addr = format!("127.0.0.1:{port}");

    let server_thread = thread::spawn(move || {
        let mut request = server.recv().expect("receives request");
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).unwrap();
        assert!(
            body.contains("\"message\":\"how are you\""),
            "request body: {body}"
        );
        let reply = Response::from_string(r#"{"answer":"doing well","agentic":true}"#);
        request.respond(reply).unwrap();
    });

    let answer = jk_tui::send_turn(&addr, "s1", "how are you").expect("client succeeds");
    assert_eq!(answer, "doing well");

    server_thread.join().unwrap();
}
