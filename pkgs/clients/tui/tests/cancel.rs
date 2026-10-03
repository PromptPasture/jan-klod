//! Acceptance #157, line 3: a cancelled turn **stops**, not just "message sent".
//! (#133 precedent: a gate that looks like it runs but does not.)
//!
//! Fake server streams SSE `delta` frames forever (mid-turn stand-in). `cancel`
//! on the `Rest` transport shuts down the client's socket. Without it,
//! `stream_turn` stays blocked when the timeout elapses. A gate that doesn't
//! run would pass silently (green despite nothing); the fake server here proves
//! it must actually stop.

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

    // The fake core: accept one connection, read past request headers,
    // then emit `delta` frames until socket shuts down. Never stops on its own.
    // Only `cancel` does.
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

    // Give the turn a moment to connect and start streaming before cancelling it.
    // Cancelling a connection that has not been made yet is a different test.
    thread::sleep(Duration::from_millis(200));
    transport.cancel("s1").expect("a turn is running to cancel");

    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stream_turn did not return after cancel — the stream is still open");
    assert!(
        result.is_ok(),
        "cancelled turn must end (error ok); hanging is the bug: {result:?}"
    );
}
