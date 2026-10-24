//! A keyless self-hosted model over a real loopback socket: it boots, egress permits a loopback `base-url`, and `type:` names a real component.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use jk_agent::conductor::RunResult;
use jk_agent::Runtime;

use crate::common;

const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

struct FakeOllama {
    port: u16,
    requests: Arc<AtomicU32>,
    saw_authorization: Arc<AtomicU32>,
    stop: Arc<AtomicU32>,
    faults: Arc<Mutex<Vec<String>>>,
}

impl FakeOllama {
    fn start(answer: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        let requests = Arc::new(AtomicU32::new(0));
        let saw_authorization = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicU32::new(0));
        let faults = Arc::new(Mutex::new(Vec::new()));
        let (counted, authed, stopped, faulted) = (
            Arc::clone(&requests),
            Arc::clone(&saw_authorization),
            Arc::clone(&stop),
            Arc::clone(&faults),
        );
        let fault = move |what: &str, e: &dyn std::fmt::Debug| {
            if let Ok(mut log) = faulted.lock() {
                log.push(format!("{what}: {e:?}"));
            }
        };
        thread::spawn(move || loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    if stopped.load(Ordering::Relaxed) != 0 {
                        break;
                    }
                    counted.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = socket.set_read_timeout(Some(REQUEST_DEADLINE)) {
                        fault("set_read_timeout", &e);
                    }
                    let request = match common::read_request(&mut socket) {
                        Ok(request) => request.to_lowercase(),
                        Err(e) => {
                            fault("read_request", &e);
                            continue;
                        }
                    };
                    if request.contains("authorization:") {
                        authed.fetch_add(1, Ordering::Relaxed);
                    }
                    let body = serde_json::json!({
                        "choices": [{
                            "message": { "role": "assistant", "content": answer },
                            "finish_reason": "stop"
                        }]
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    if let Err(e) = socket.write_all(response.as_bytes()) {
                        fault("write_all", &e);
                    }
                }
                Err(e) => {
                    fault("accept", &e);
                    break;
                }
            }
        });
        Self {
            port,
            requests,
            saw_authorization,
            stop,
            faults,
        }
    }
}

impl Drop for FakeOllama {
    fn drop(&mut self) {
        self.stop.store(1, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

#[test]
fn a_keyless_local_model_completes_a_turn_over_a_real_socket() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-local-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let ollama = FakeOllama::start("the local model answered");
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  provider:
    ollama:
      enabled: true
      type: openai
      base-url: http://127.0.0.1:{}/v1
      model: qwen2.5:7b
",
            ollama.port
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let policy = runtime.egress_policy();
    let factory = move || -> jk_wasm::route::HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = runtime
        .build_agent(&factory)
        .expect("agent boots without an api-key");

    let result = agent.run("local", "hello");
    match result {
        RunResult::Answered { text, .. } => {
            assert!(
                text.contains("the local model answered"),
                "the answer came from the local endpoint: {text}"
            );
        }
        other @ RunResult::Failed(_) => panic!("a local model must complete a turn, got {other:?}"),
    }

    assert!(
        ollama.requests.load(Ordering::Relaxed) >= 1,
        "the request actually crossed a socket"
    );
    let faults = ollama
        .faults
        .lock()
        .expect("the fault log is not poisoned")
        .clone();
    assert!(
        faults.is_empty(),
        "the fake endpoint could not complete its own I/O, so it saw less than \
         the request it answered: {faults:?}"
    );

    assert_eq!(
        ollama.saw_authorization.load(Ordering::Relaxed),
        0,
        "no Authorization header is sent when no api-key is configured"
    );
}

#[test]
fn an_unreachable_provider_says_so_and_names_the_endpoint() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-unreach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
        listener.local_addr().expect("addr").port()
    };
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
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

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("boots");
    let policy = runtime.egress_policy();
    let factory = move || -> jk_wasm::route::HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let RunResult::Failed(message) = agent.run("s", "hello") else {
        panic!("an unreachable provider cannot answer");
    };
    assert!(
        message.contains(&format!("127.0.0.1:{port}")),
        "the failure names the endpoint to look at: {message}"
    );
    assert!(
        message.contains("could not be reached"),
        "and says it was unreachable rather than transient: {message}"
    );
    assert!(
        !message.contains("ProviderError {"),
        "no generated-binding Debug reaches the user: {message}"
    );
    assert!(
        message.contains("egress"),
        "the message mentions egress: {message}"
    );
}
