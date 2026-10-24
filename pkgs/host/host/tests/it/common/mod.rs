#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jk_wasm::http::WireResponse;
use jk_wasm::route::HttpFn;

pub mod minisig;

pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."]
        .iter()
        .collect()
}

/// Set to fail instead of skip when guests not staged. `make gate` exports it.
const REQUIRE: &str = "JK_REQUIRE_GUESTS";

pub fn guests_staged(guests: &[&str]) -> bool {
    let ext_dir = repo_root().join("ext");
    let absent: Vec<&str> = guests
        .iter()
        .copied()
        .filter(|g| !ext_dir.join(g).exists())
        .collect();
    if absent.is_empty() {
        return true;
    }
    assert!(
        std::env::var(REQUIRE).is_err(),
        "{REQUIRE} is set, so this test must not be skipped, but {absent:?} \
         are not staged in {} — run `make ext`",
        ext_dir.display()
    );
    eprintln!("skipping: {absent:?} not staged — run `make ext`");
    false
}

fn runnable(program: &str) -> bool {
    ["--version", "version"].iter().any(|flag| {
        std::process::Command::new(program)
            .arg(flag)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

pub fn optional_tool(program: &str) -> bool {
    if runnable(program) {
        return true;
    }
    eprintln!("skipping: optional toolchain `{program}` is not installed");
    false
}

pub fn tool_available(program: &str) -> bool {
    if runnable(program) {
        return true;
    }
    assert!(
        std::env::var(REQUIRE).is_err(),
        "{REQUIRE} is set, so this test must not be skipped, but `{program}` is not \
         on PATH"
    );
    eprintln!("skipping: `{program}` is not available");
    false
}

pub fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

pub fn read_request(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = socket.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
        let body_len: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0);
        if raw.len() >= head_end + 4 + body_len {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

pub struct Countable {
    pub port: u16,
    hits: Arc<AtomicU32>,
}

impl Countable {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        let hits = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&hits);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else { continue };
                counted.fetch_add(1, Ordering::SeqCst);
                let mut seen = Vec::new();
                let mut byte = [0_u8; 1];
                while socket.read(&mut byte).unwrap_or(0) == 1 {
                    seen.push(byte[0]);
                    if seen.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = socket.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret",
                );
            }
        });
        Self { port, hits }
    }

    pub fn only_hit(&self) {
        let before = self.hits.load(Ordering::SeqCst);
        let mut probe =
            TcpStream::connect(("127.0.0.1", self.port)).expect("the sentinel is listening");
        probe
            .write_all(b"GET /probe HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .expect("writes the probe");
        let mut sink = Vec::new();
        let _ = probe.read_to_end(&mut sink);
        assert_eq!(
            self.hits.load(Ordering::SeqCst),
            before + 1,
            "counter live (fail → assertion below proves nothing)"
        );
        assert_eq!(
            self.hits.load(Ordering::SeqCst),
            1,
            "only connection is this test's probe — more = code reached it"
        );
    }
}
