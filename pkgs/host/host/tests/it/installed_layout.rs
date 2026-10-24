//! Runs an assembled installed layout (`<prefix>/bin` beside `<prefix>/share/jan-klod`) from an unrelated directory,
//! as `jan-klod` spawns it: path-resolution bugs only show up this way. Skips when the guests are not staged.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::common;

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

/// Builds the layout `install.sh` produces from the repo's build output.
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
        let name = entry.file_name();
        let carried = path.extension().is_some_and(|e| e == "wasm")
            || name.to_string_lossy().ends_with(".manifest.toml");
        if carried {
            std::fs::copy(&path, ext_dst.join(name)).expect("a guest is copied");
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

/// `GET /health` over a raw socket, so the test needs no HTTP client.
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

/// A directory that is not a jan-klod checkout, like the user's own repository.
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
        wait_until_listening(&addr, Duration::from_secs(180)),
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

/// `ask` answers on stdout from an installed layout. Stdin is closed, so a prompt
/// must take its default (denial), not read EOF as approval.
#[test]
fn ask_answers_on_stdout_from_an_installed_layout() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let prefix = std::env::temp_dir().join(format!("jk-ask-{}", std::process::id()));
    let _guard = common::TempDir(prefix.clone());
    let gateway = install_into(&prefix);
    let work = elsewhere(&prefix);

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
    assert!(
        !stdout.contains('?'),
        "no prompt text leaked into stdout: {stdout:?}"
    );
}

/// `verify --live` catches what offline checks cannot: offline verify passes with a
/// wrong API key or a dead endpoint, the usual first-run failures.
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
