//! The way a user actually starts it.
//!
//! Two consecutive fixes missed this path. The first taught the gateway to
//! resolve `config.yaml`/`ext/` against its installed data directory; the UI kept
//! naming them explicitly, so the resolution never ran. The second fixed the UI's
//! spawn; nothing automated would have noticed if it had not.
//!
//! Both were *mechanism* corrections verified against the mechanism. What was
//! missing is the shape every other durable test here has: exercise the entry
//! point a user touches. So this assembles a real installed layout —
//! `<prefix>/bin/jan-klod-gateway` beside `<prefix>/share/jan-klod/` — runs it
//! **from an unrelated working directory** exactly as `jan-klod` spawns it, and
//! asks whether it came up.
//!
//! The binary is the one this crate builds (`CARGO_BIN_EXE_…`), so the test moves
//! with the code rather than against a stale artefact.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod common;

const GUESTS: [&str; 3] = [
    "provider-openai.wasm",
    "tool-fs.wasm",
    "interceptor-permission.wasm",
];

/// Kills the gateway on drop, so a failing assertion cannot leave one running.
struct Gateway(Child);
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Build `<prefix>/bin` + `<prefix>/share/jan-klod` from the repo's own build
/// output — the layout `install.sh` produces.
fn install_into(prefix: &Path) -> PathBuf {
    let bin = prefix.join("bin");
    let data = prefix.join("share/jan-klod");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&data).unwrap();

    let built = PathBuf::from(env!("CARGO_BIN_EXE_jan-klod-gateway"));
    let installed = bin.join("jan-klod-gateway");
    std::fs::copy(&built, &installed).expect("the gateway binary is copied");

    std::fs::copy(
        common::repo_root().join("config.yaml"),
        data.join("config.yaml"),
    )
    .expect("the shipped config is copied");
    let ext_src = common::repo_root().join("ext");
    let ext_dst = data.join("ext");
    std::fs::create_dir_all(&ext_dst).unwrap();
    for entry in std::fs::read_dir(&ext_src)
        .expect("ext/ is readable")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "wasm") {
            std::fs::copy(&path, ext_dst.join(entry.file_name())).expect("a guest is copied");
        }
    }
    installed
}

/// A port nobody is using, released immediately before the gateway takes it.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds an ephemeral port");
    listener.local_addr().expect("has an address").port()
}

fn wait_until_listening(addr: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// `GET /health` over a raw socket — no HTTP client, so the test depends on
/// nothing the product does not already ship.
fn get_health(addr: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connects to the gateway");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response
}

/// A directory that is emphatically not a jan-klod checkout — the user's own
/// repository, which is where this gets run.
fn elsewhere(prefix: &Path) -> PathBuf {
    let dir = prefix.join("some-users-repo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
    dir
}

#[test]
fn an_installed_gateway_serves_from_a_directory_that_is_not_a_checkout() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let prefix = std::env::temp_dir().join(format!("jk-installed-{}", std::process::id()));
    let _guard = common::TempDir(prefix.clone());
    let gateway = install_into(&prefix);
    let work = elsewhere(&prefix);

    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    // Exactly what `jan-klod` spawns. Naming config.yaml/ext here instead — as it
    // used to — is what broke: from `work` those paths do not exist.
    let child = Command::new(&gateway)
        .args(["serve", "--bind", &addr])
        .current_dir(&work)
        .env("OPENAI_API_KEY", "test-placeholder")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the installed gateway starts");
    let _gateway = Gateway(child);

    assert!(
        wait_until_listening(&addr, Duration::from_secs(20)),
        "the gateway never came up at {addr} — an installed jan-klod could not start \
         from {}",
        work.display()
    );

    let health = get_health(&addr);
    assert!(health.contains("200 OK"), "health responds: {health}");
    assert!(
        health.contains("\"status\":\"ok\""),
        "and reports ok: {health}"
    );
}

#[test]
fn an_installed_gateway_verifies_its_own_components_from_anywhere() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let prefix = std::env::temp_dir().join(format!("jk-installed-v-{}", std::process::id()));
    let _guard = common::TempDir(prefix.clone());
    let gateway = install_into(&prefix);
    let work = elsewhere(&prefix);

    // `install.sh` runs exactly this after installing, so a regression here also
    // means every future install reports itself broken.
    let output = Command::new(&gateway)
        .arg("verify")
        .current_dir(&work)
        .env("OPENAI_API_KEY", "test-placeholder")
        .output()
        .expect("verify runs");

    assert!(
        output.status.success(),
        "verify failed from {}:\n{}\n{}",
        work.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("start cleanly"),
        "every component starts: {stdout}"
    );
    // `0 missing`, not `!contains("missing")` — the report's summary line always
    // carries the word, so the negative form is vacuously false. That exact
    // mistake shipped once already in `shipped_defaults`, and I wrote it again
    // here; the difference is that this suite now runs for real, so it failed
    // immediately instead of sitting green.
    assert!(stdout.contains("0 missing"), "nothing is missing: {stdout}");
}

/// The negative case, so the tests above cannot pass for an unrelated reason.
#[test]
fn a_gateway_with_no_data_directory_beside_it_fails_and_says_why() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let prefix = std::env::temp_dir().join(format!("jk-installed-bare-{}", std::process::id()));
    let _guard = common::TempDir(prefix.clone());
    let bin = prefix.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // Binary alone — no `../share/jan-klod`. This is what the installer produced
    // before it learned to copy the components.
    let lone = bin.join("jan-klod-gateway");
    std::fs::copy(env!("CARGO_BIN_EXE_jan-klod-gateway"), &lone).unwrap();
    let work = elsewhere(&prefix);

    let output = Command::new(&lone)
        .arg("verify")
        .current_dir(&work)
        .env("OPENAI_API_KEY", "test-placeholder")
        .output()
        .expect("verify runs");

    assert!(
        !output.status.success(),
        "a gateway with no components must not report success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("config.yaml"),
        "the failure names what it could not find: {stderr}"
    );
}

/// `ask` answers on stdout, from an installed layout, in a directory that is not
/// a checkout.
///
/// Documented in two places before it existed — sentences I wrote to justify
/// withholding stdin from guests, describing a subcommand that fell through to the
/// boot plan. This drives the real binary the way the docs say to, with stdin
/// closed so a confirmation cannot be answered: EOF must take the prompt's default,
/// which is a denial, rather than reading as approval.
#[test]
fn ask_answers_on_stdout_from_an_installed_layout() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let prefix = std::env::temp_dir().join(format!("jk-ask-{}", std::process::id()));
    let _guard = common::TempDir(prefix.clone());
    let gateway = install_into(&prefix);
    let work = elsewhere(&prefix);

    // A local endpoint the installed config does not name, so the run needs no
    // network and no key: point the provider at it via an override config beside
    // the binary's data directory.
    let data = prefix.join("share/jan-klod");
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
    let port = listener.local_addr().expect("addr").port();
    let answer = "the installed gateway answered";
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { break };
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf);
            let body = serde_json::json!({
                "choices": [{ "message": { "role": "assistant", "content": answer },
                              "finish_reason": "stop" }]
            })
            .to_string();
            let _ = socket.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    std::fs::write(
        data.join("config.yaml"),
        format!(
            "
extensions:
  provider:
    local:
      enabled: true
      type: openai
      base-url: http://127.0.0.1:{port}/v1
      model: local
"
        ),
    )
    .unwrap();

    let output = Command::new(&gateway)
        .args(["ask", "what", "does", "this", "repo", "do?"])
        .current_dir(&work)
        .stdin(Stdio::null())
        .output()
        .expect("ask runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "ask succeeded: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(answer),
        "the answer is on stdout: {stdout:?}"
    );
    // stdout is the answer and nothing else, so a script can pipe it.
    assert!(
        !stdout.contains('?'),
        "no prompt text leaked into stdout: {stdout:?}"
    );
}

/// `verify --live` catches what the offline checks cannot.
///
/// Everything `verify` did before was offline — components resolve, instantiate
/// and start — and all of it passes with a wrong API key, an endpoint that is not
/// running, a model that does not exist, and a `base-url` egress will refuse.
/// That is the whole list of things that actually go wrong on a first run, so
/// "verified" was a claim about the parts nobody has trouble with.
#[test]
fn verify_live_reports_a_provider_that_does_not_answer() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let prefix = std::env::temp_dir().join(format!("jk-vlive-{}", std::process::id()));
    let _guard = common::TempDir(prefix.clone());
    let gateway = install_into(&prefix);
    let work = elsewhere(&prefix);
    let data = prefix.join("share/jan-klod");

    // A port that is real and closed, which is what a stopped local model looks
    // like — the most common first-run failure there is.
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
        listener.local_addr().expect("addr").port()
    };
    std::fs::write(
        data.join("config.yaml"),
        format!(
            "
extensions:
  provider:
    local:
      enabled: true
      type: openai
      base-url: http://127.0.0.1:{dead}/v1
      model: local
"
        ),
    )
    .unwrap();

    // Offline verify passes: the component is present and starts.
    let offline = Command::new(&gateway)
        .arg("verify")
        .current_dir(&work)
        .output()
        .expect("verify runs");
    assert!(
        offline.status.success(),
        "the offline checks pass even though nothing answers — which is the point: {}",
        String::from_utf8_lossy(&offline.stderr)
    );

    let live = Command::new(&gateway)
        .args(["verify", "--live"])
        .current_dir(&work)
        .output()
        .expect("verify --live runs");
    let stderr = String::from_utf8_lossy(&live.stderr);
    assert!(
        !live.status.success(),
        "--live fails when the model does not answer"
    );
    assert!(
        stderr.contains(&dead.to_string()) && stderr.contains("could not be reached"),
        "and names the endpoint and the cause: {stderr}"
    );
}
