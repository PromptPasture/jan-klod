//! Acceptance line 3 of #157: a cancelled turn actually **stops**, not merely
//! "a message was sent" — #133 is the local precedent for a gate that looks
//! like it runs and does not.
//!
//! A fake server that streams SSE `delta` frames forever stands in for a core
//! that is still mid-turn. `Transport::cancel` on the `Rest` transport does not
//! send anything to that server at all — it shuts down the client's own
//! socket, the same way dropping the connection would. If that shutdown did
//! nothing, `stream_turn` would still be blocked reading frames when the
//! timeout below elapses and the test would fail exactly the way a gate that
//! does not run looks like one that passes: green because nothing challenged
//! it. Here something does — the server never stops on its own.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jan_klod::transport::{Rest, Transport};

#[test]
fn cancel_stops_a_turn_streaming_forever_over_rest() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    // The fake core: accept one connection, read past the request headers,
    // then emit `delta` frames on a short interval until the socket is shut
    // down out from under it. It never decides to stop — only `cancel` does.
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        {
            let mut reader = BufReader::new(stream.try_clone().expect("clone for reading"));
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) if line.trim().is_empty() => break,
                    Ok(_) => {}
                }
            }
        }
        let headers =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        if stream.write_all(headers.as_bytes()).is_err() {
            return;
        }
        loop {
            let frame = "event: delta\ndata: {\"text\":\"still going\"}\n\n";
            if stream.write_all(frame.as_bytes()).is_err() {
                // `cancel` shut the socket down: this is the fake server
                // discovering it, exactly as a real one would.
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });

    let transport: Rest = Rest::new(addr.to_string());
    let transport = std::sync::Arc::new(transport);

    let (tx, rx) = mpsc::channel();
    let turn_transport = transport.clone();
    thread::spawn(move || {
        let result = turn_transport.stream_turn("s1", "keep talking", &mut |_event| {});
        let _ = tx.send(result);
    });

    // Give the turn a moment to connect and start streaming before cancelling
    // it — cancelling a connection that has not been made yet is a different
    // test.
    thread::sleep(Duration::from_millis(200));
    transport.cancel("s1").expect("a turn is running to cancel");

    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stream_turn did not return after cancel — the stream is still open");
    assert!(
        result.is_ok(),
        "a cancelled turn ending in an error is fine; not ending at all is the bug: {result:?}"
    );
}
