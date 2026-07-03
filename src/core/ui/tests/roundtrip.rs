//! `send_turn` against a real socket — stands up a canned HTTP server (standing in
//! for `jan-klod serve`) and drives the client through one round-trip, asserting it
//! sends a well-formed request and reads the answer back.

use std::thread;

use tiny_http::{Response, Server};

#[test]
fn send_turn_round_trips_against_a_server() {
    let server = Server::http("127.0.0.1:0").expect("binds ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    let addr = format!("127.0.0.1:{port}");

    // Server thread: read the request body, assert it carries the message, reply.
    let server_thread = thread::spawn(move || {
        let mut request = server.recv().expect("receives request");
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).unwrap();
        assert!(body.contains("\"message\":\"how are you\""), "request body: {body}");
        let reply = Response::from_string(r#"{"answer":"doing well","agentic":true}"#);
        request.respond(reply).unwrap();
    });

    let answer = jan_klod::send_turn(&addr, "s1", "how are you").expect("client succeeds");
    assert_eq!(answer, "doing well");

    server_thread.join().unwrap();
}
