//! A component with no grants gets no ambient authority. `tool-escape-probe`

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jk_wasm::host_process::ProcessRunner;
use jk_wasm::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

const PROBE: &str = "tool-escape-probe.wasm";

struct Sentinel {
    port: u16,
    hits: Arc<AtomicU32>,
    stop: Arc<AtomicU32>,
}

impl Sentinel {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        listener.set_nonblocking(true).expect("nonblocking");
        let hits = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&hits);
        let stopped = Arc::clone(&stop);
        thread::spawn(move || {
            while stopped.load(Ordering::Relaxed) == 0 {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        counted.fetch_add(1, Ordering::Relaxed);
                        let mut buf = [0_u8; 512];
                        let _ = socket.read(&mut buf);
                        let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                    }
                    Err(_) => thread::sleep(std::time::Duration::from_millis(20)),
                }
            }
        });
        Self { port, hits, stop }
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.stop.store(1, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn probe(engine: &Engine) -> Option<ToolExtension> {
    if !common::guests_staged(&[PROBE]) {
        return None;
    }
    let component = Component::from_file(engine, common::repo_root().join("ext").join(PROBE))
        .expect("the probe compiles");
    Some(
        ToolExtension::instantiate_with_http(
            engine,
            "tool.escape-probe",
            &component,
            None,
            ProcessRunner::disabled(),
            None,
            None,
            false,
        )
        .expect("the probe instantiates"),
    )
}

#[test]
fn a_guest_cannot_open_its_own_socket() {
    let engine = Engine::default();
    let Some(mut tool) = probe(&engine) else {
        return;
    };
    let sentinel = Sentinel::start();

    let args = serde_json::json!({
        "op": "socket",
        "target": format!("127.0.0.1:{}", sentinel.port)
    });
    let report = tool.invoke(&args.to_string()).unwrap_or_else(|err| err);

    assert_eq!(
        sentinel.hits.load(Ordering::Relaxed),
        0,
        "a guest reached no socket directly: {report}. `host-http`'s egress \
         policy bounds only guests using `host-http`; this one did not."
    );
    assert!(
        report.starts_with("refused"),
        "and the attempt is refused, not silently hanging: {report}"
    );
}

#[test]
fn a_guest_cannot_open_a_socket_to_a_public_address() {
    let engine = Engine::default();
    let Some(mut tool) = probe(&engine) else {
        return;
    };
    let args = serde_json::json!({ "op": "socket", "target": "93.184.216.34:80" });
    let report = tool.invoke(&args.to_string()).unwrap_or_else(|err| err);
    assert!(
        report.starts_with("refused"),
        "outbound sockets are denied wholesale: {report}"
    );
}

#[test]
fn a_guest_cannot_read_the_hosts_filesystem() {
    let engine = Engine::default();
    let Some(mut tool) = probe(&engine) else {
        return;
    };
    for target in ["/etc/passwd", "/etc/hosts", "config.yaml", "."] {
        let args = serde_json::json!({ "op": "fs", "target": target });
        let report = tool.invoke(&args.to_string()).unwrap_or_else(|err| err);
        assert!(
            report.starts_with("refused"),
            "the ambient filesystem is empty — {target} must not open: {report}"
        );
    }
}

#[test]
fn a_guest_cannot_read_the_hosts_environment() {
    std::env::set_var("OPENAI_API_KEY", "sk-guest-env-must-not-leak");
    std::env::set_var("JAN_KLOD_TOKEN", "bearer-guest-env-must-not-leak");
    let engine = Engine::default();
    let Some(mut tool) = probe(&engine) else {
        return;
    };

    let report = tool.invoke(r#"{"op":"env"}"#).unwrap_or_else(|err| err);
    assert!(
        !report.contains("must-not-leak"),
        "a guest read the host's credentials straight out of the environment: {report}"
    );
    assert!(
        report.starts_with("refused"),
        "and gets no environment at all: {report}"
    );

    let args = tool.invoke(r#"{"op":"args"}"#).unwrap_or_else(|err| err);
    for ours in ["--bind", "config.yaml", "serve", "--live"] {
        assert!(
            !args.contains(ours),
            "the host's arguments reached a guest: {args}"
        );
    }
}

const SECRET: &str = "TOPSECRET-USER-KEYSTROKES";
const CHILD: &str = "JK_STDIN_PROBE_CHILD";
const TEST_NAME: &str = "a_guest_gets_no_standard_input";

#[test]
fn a_guest_gets_no_standard_input() {
    if std::env::var(CHILD).is_ok() {
        let engine = Engine::default();
        let Some(mut tool) = probe(&engine) else {
            return;
        };
        let report = tool.invoke(r#"{"op":"stdin"}"#).unwrap_or_else(|err| err);
        println!("PROBE-REPORT {report}");
        return;
    }
    if !common::guests_staged(&[PROBE]) {
        return;
    }

    let test_path = module_path!().split_once("::").map_or_else(
        || TEST_NAME.to_owned(),
        |(_, module)| format!("{module}::{TEST_NAME}"),
    );
    let mut child = std::process::Command::new(std::env::current_exe().expect("own path"))
        .args(["--exact", test_path.as_str(), "--nocapture"])
        .env(CHILD, "1")
        .env("JK_REQUIRE_GUESTS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("re-executes itself");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(SECRET.as_bytes())
        .expect("the pipe carries real bytes");
    let out = child.wait_with_output().expect("the child finishes");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let report = stdout
        .lines()
        .find_map(|line| line.strip_prefix("PROBE-REPORT "))
        .unwrap_or_else(|| panic!("the child reported nothing — it did not run:\n{stdout}"));

    assert!(
        !report.contains(SECRET),
        "a guest cannot read the host's stdin: {report}"
    );
    assert!(
        report.starts_with("refused") || report.starts_with("READ 0 bytes"),
        "and gets nothing at all: {report}"
    );
}
