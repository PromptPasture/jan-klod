//! What a component gets when nobody grants it anything.
//!
//! Every guest's linker is built with `wasmtime_wasi::p2::add_to_linker_sync`,
//! which wires `wasi:filesystem`, `wasi:sockets/tcp` and `wasi:sockets/udp` in
//! full. What keeps those from being an escape hatch is not the linker but the
//! `WasiCtx`: no preopens, and a `SocketAddrCheck` whose default refuses every
//! address. Both are **defaults in a dependency we upgrade**. If a future
//! wasmtime flips either one, `host-fs`'s path jail and `core::egress`'s
//! destination policy become decoration — a guest would just open its own socket
//! or its own file, and every existing test would keep passing, because every
//! other guest is cooperative and asks politely through a typed import.
//!
//! So `tool-escape-probe` does not ask. It calls `std::net::TcpStream::connect`,
//! `std::io::stdin`, and `std::fs::read_to_string` — ordinary Rust, no bespoke
//! bindings, which is the realistic shape of the problem. These tests assert each
//! attempt is refused, and the socket case asserts it against a real listening
//! server's own request count rather than the error the guest reports.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

const PROBE: &str = "tool-escape-probe.wasm";

/// A loopback server that counts connections, so "did the packet leave?" is
/// answered by the listener and not by the sandbox's error message.
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

/// Instantiate the probe with **nothing** granted: no workspace, no process
/// runner, no `host-http`. Whatever it manages is ambient authority.
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
        "a guest reached a socket directly: {report}. `host-http`'s egress policy \
         only bounds the guests that use `host-http`; this one did not."
    );
    assert!(
        report.starts_with("refused"),
        "and the attempt is refused rather than silently hanging: {report}"
    );
}

/// The same, aimed off-box. A guest that cannot reach loopback but *can* reach
/// the internet has no sandbox, only a firewall rule.
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

/// The credentials are in the environment, so the guest must not have one.
///
/// The sibling of the subprocess leak fixed on 2026-08-15: a command run through
/// `host-process` inherited `OPENAI_API_KEY` and `JAN_KLOD_TOKEN` because nothing
/// cleared the environment. A guest reading them *directly* would be the shorter
/// path, and it is closed only because `WasiCtxBuilder::inherit_env` is not called
/// — a default in a crate we upgrade, exactly like the deny-all socket check. If it
/// flips, every component reads the operator's provider key with one line of `std`,
/// and nothing else in the suite would notice.
///
/// The secrets are set in *this* process before the guest runs, so the check is not
/// measuring an empty environment: they are demonstrably there to be inherited.
#[test]
fn a_guest_cannot_read_the_hosts_environment() {
    // Set *before* the guest is instantiated. `inherit_env` snapshots the
    // environment when the `WasiCtx` is built, so setting these afterwards left the
    // credential assertion passing even with inheritance switched on — the check
    // that matters was the one measuring nothing.
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

    // The host's command line names its config and its bind address. A guest sees
    // its own argv[0] and nothing of ours.
    let args = tool.invoke(r#"{"op":"args"}"#).unwrap_or_else(|err| err);
    for ours in ["--bind", "config.yaml", "serve", "--live"] {
        assert!(
            !args.contains(ours),
            "the host's arguments reached a guest: {args}"
        );
    }
}

/// stdin is the one the host was actually giving away.
///
/// The gateway inherited the parent's stdio and handed it to every guest, so a
/// component could read the terminal `jan-klod-gateway ask` runs in — including a
/// permission answer being typed at the prompt. `tool-escape-probe` read 26 bytes
/// of it.
///
/// **This test feeds itself the input.** The first version simply asked the probe
/// to read stdin and accepted "0 bytes" as success — which is what a test harness
/// with no stdin returns whether or not the guest was granted any. It passed
/// against the hole. So the parent re-executes this binary with a secret piped in
/// and reads what the child reports: a vacuum cannot be mistaken for a boundary
/// when the pipe demonstrably has bytes in it.
const SECRET: &str = "TOPSECRET-USER-KEYSTROKES";
/// Set on the re-executed child so it runs the probe instead of the parent half.
const CHILD: &str = "JK_STDIN_PROBE_CHILD";
/// This test's own function name, for the `--exact` filter it re-executes with.
const TEST_NAME: &str = "a_guest_gets_no_standard_input";

#[test]
fn a_guest_gets_no_standard_input() {
    if std::env::var(CHILD).is_ok() {
        // Child half: read stdin through the guest and report.
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

    // libtest's `--exact` matches the *full* test path. While each file here was
    // its own test binary that was the bare function name; now that they are
    // modules of one `it` target it is module-qualified, and the old literal
    // matched nothing — the child ran zero tests and this test failed with "the
    // child reported nothing". Derived rather than rewritten by hand, so a
    // rename or another move cannot silently desynchronise it: `module_path!()`
    // is `it::sandbox_boundary`, and libtest's name drops the crate root.
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
        "a guest read the host's standard input: {report}. `jan-klod-gateway ask` \
         runs in the user's terminal, so this is the keystrokes — including the \
         answer to a permission prompt."
    );
    assert!(
        report.starts_with("refused") || report.starts_with("READ 0 bytes"),
        "and gets nothing at all: {report}"
    );
}
