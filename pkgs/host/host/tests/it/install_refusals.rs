//! What must not be possible around `ext-install` (#213).
//!
//! `ext_install.rs` covers `ext::install` as a function — what lands, and
//! which malformed offers are refused. This covers the *tool*: installing is
//! the one action that adds code to the runtime, so the interesting
//! assertions are refusals reached the way a model would reach them.
//!
//! Two are existing behaviour that nothing named this path — `ext/` is
//! outside every jail, and an unsigned artefact already needs its digest —
//! and a test that does not name the path stops covering it the moment the
//! path moves. The third, an operator declining, is new.

use std::cell::RefCell;
use std::rc::Rc;

use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::tool_host::ToolExtension;
use jan_klod_core::Runtime;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

const PROBE: &str = "tool-escape-probe.wasm";

/// A driver that refuses whatever it is asked, and counts the asking.
struct DecliningDriver {
    asked: Rc<RefCell<u32>>,
}

impl Driver for DecliningDriver {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        *self.asked.borrow_mut() += 1;
        "no".to_string()
    }
}

/// `ext/` is not somewhere a guest can write, and this names it.
///
/// `sandbox_boundary.rs` proves a guest has no ambient filesystem at all,
/// which already covers this — but by accident of generality, among
/// `/etc/passwd` and `.`. The whole install design rests on "no guest can
/// write `ext/`", and that claim deserves a test that fails if `ext/`
/// specifically ever becomes reachable.
#[test]
fn a_guest_cannot_write_the_extension_directory() {
    if !common::guests_staged(&[PROBE]) {
        return;
    }
    let engine = Engine::default();
    let ext_dir = common::repo_root().join("ext");
    let component = Component::from_file(&engine, ext_dir.join(PROBE)).expect("the probe compiles");
    let mut probe = ToolExtension::instantiate_with_http(
        &engine,
        "tool.escape-probe",
        &component,
        None,
        ProcessRunner::disabled(),
        None,
        None,
        false,
    )
    .expect("the probe instantiates");

    let target = ext_dir.join("tool-implanted-by-a-guest.wasm");
    let args = serde_json::json!({ "op": "fs", "target": target.display().to_string() });
    let report = probe.invoke(&args.to_string()).unwrap_or_else(|err| err);
    assert!(
        report.starts_with("refused"),
        "a guest reached the extension directory, which is where the install \
         trust boundary lives: {report}"
    );
    assert!(
        !target.exists(),
        "and nothing was written: {}",
        target.display()
    );
}

/// Boot a runtime whose model asks to install `path`, with the permission
/// gate on, and return the directory it would have installed into.
fn install_turn(tag: &str, driver: &mut dyn Driver) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("jk-instref-{tag}-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).expect("creates the temp ext dir");
    // A populated `ext/` of its own: booting against the repo's would make an
    // install write into the working tree, and booting against an empty one
    // would load no guests at all — the turn would never reach a tool, and
    // "nothing was installed" would be true for the wrong reason.
    let staged = common::repo_root().join("ext");
    for guest in [
        "provider-openai",
        "interceptor-tool-selector",
        "interceptor-permission",
    ] {
        for suffix in [".wasm", ".manifest.toml"] {
            let name = format!("{guest}{suffix}");
            let from = staged.join(&name);
            if from.exists() {
                std::fs::copy(&from, ext.join(&name)).expect("copies a staged guest");
            }
        }
    }
    // A **signed** offer whose key this config trusts. Without it the
    // install is refused for want of provenance whatever the operator
    // answers — which is what made the first version of this test pass
    // with a driver that said yes.
    let incoming = dir.join("incoming");
    std::fs::create_dir_all(&incoming).expect("creates the incoming dir");
    let signer = common::minisig::Signer::new();
    for suffix in [".wasm", ".manifest.toml"] {
        let name = format!("tool-hello{suffix}");
        std::fs::copy(staged.join(&name), incoming.join(&name)).expect("copies the offer");
        signer.sign(&incoming.join(&name));
    }
    let source = incoming.join("tool-hello.wasm");

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
registry:
  install-tool: true
  trusted-keys:
    - {key}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
            key = signer.public_key_base64()
        ),
    )
    .expect("writes the config");

    let arguments = serde_json::json!({ "path": source.display().to_string() }).to_string();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        let arguments = arguments.clone();
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"ext-install","arguments":arguments}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"done"},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, &ext).expect("the runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("the agent boots");
    agent.run_with_driver(driver, "s1", "install it");
    ext
}

/// The operator says no, and nothing is installed.
///
/// The new refusal, and it works without anything built for it:
/// `ext-install` is not on `interceptor-permission`'s read-only allowlist,
/// so the gate confirms it like any other write. The approval an install
/// needs is the one the operator already has.
#[test]
fn an_install_the_operator_declines_does_not_happen() {
    if !common::guests_staged(&[
        "provider-openai.wasm",
        "interceptor-tool-selector.wasm",
        "interceptor-permission.wasm",
        "tool-hello.wasm",
    ]) {
        return;
    }
    let asked = Rc::new(RefCell::new(0));
    let mut driver = DecliningDriver {
        asked: Rc::clone(&asked),
    };
    let ext = install_turn("declined", &mut driver);
    let _guard = common::TempDir(ext.parent().expect("has a parent").to_path_buf());

    assert!(
        *asked.borrow() > 0,
        "the operator was never asked, so the refusal below proves nothing \
         about the gate"
    );
    assert!(
        !ext.join("tool-hello.wasm").exists(),
        "a declined install still wrote the component into {}",
        ext.display()
    );
}
