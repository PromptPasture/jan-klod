//! Component test harness — load a staged guest, wire host capabilities, and
//! verify its WIT interface end-to-end through the Component Model.
//!
//! This is the generalisation of the `provider_probe` example: where the probe
//! drives one provider against a *live* endpoint, the harness drives both
//! first-party guests **offline and deterministically**. It instantiates each
//! category world (`store-world`, `provider-world`), backs the imports with a
//! reusable [`TestHost`] (config section, captured logs, a **canned** `host-http`
//! so the provider needs no network), then runs lifecycle plus the guest's own
//! interface.
//!
//! Each test skips with a note when its component is not staged in `ext/`, so a
//! bare `cargo test` (no guests built) stays green. Build the guests first
//! (`make extensions`) or run the bundled target (`make harness`) to exercise
//! them.

// Dominated by `bindgen!`-generated code; exempt from the workspace lints, as the
// sibling `provider_probe` example is.
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::PathBuf;

use jan_klod_core::ConfigSection;
use serde_json::json;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

/// `store-world`: lifecycle + `memory-store`, imports `host-log` + `host-config`.
mod store_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "store-world",
    });
}

/// `provider-world`: lifecycle + `llm-provider`, also imports `host-http`.
mod provider_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "provider-world",
    });
}

/// A canned `host-http` reply. The harness mirrors the host boundary in
/// [`jan_klod_core::http`]: 5xx -> `server-error`, 4xx -> `client-error`,
/// everything else -> a successful response. One mock serves both the happy path
/// and error-mapping tests.
#[derive(Clone)]
struct MockHttp {
    status: u16,
    body: Vec<u8>,
}

/// Reusable host backing every guest's imports in one struct: its config section,
/// a log buffer the tests can assert against, and the canned HTTP reply.
struct TestHost {
    wasi: WasiCtx,
    table: ResourceTable,
    section: ConfigSection,
    logs: Vec<String>,
    http: MockHttp,
}

impl TestHost {
    fn new(section: serde_json::Value, http: MockHttp) -> Self {
        Self {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            section: ConfigSection::new(section),
            logs: Vec::new(),
            http,
        }
    }
}

impl WasiView for TestHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// --- host-log: capture lines for assertions (one impl per generated world) ---

impl store_bind::jan_klod::interfaces::host_log::Host for TestHost {
    fn log(
        &mut self,
        _level: store_bind::jan_klod::interfaces::host_log::LogLevel,
        component: String,
        message: String,
        _fields: Vec<store_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
        self.logs.push(format!("{component}: {message}"));
    }
}

impl provider_bind::jan_klod::interfaces::host_log::Host for TestHost {
    fn log(
        &mut self,
        _level: provider_bind::jan_klod::interfaces::host_log::LogLevel,
        component: String,
        message: String,
        _fields: Vec<provider_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
        self.logs.push(format!("{component}: {message}"));
    }
}

// --- host-config: serve the instance section (one impl per generated world) ---

impl store_bind::jan_klod::interfaces::host_config::Host for TestHost {
    fn get(
        &mut self,
        key: String,
    ) -> std::result::Result<String, store_bind::jan_klod::interfaces::host_config::ConfigError> {
        self.section
            .get(&key)
            .ok_or(store_bind::jan_klod::interfaces::host_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }
    fn all(
        &mut self,
    ) -> std::result::Result<String, store_bind::jan_klod::interfaces::host_config::ConfigError> {
        Ok(self.section.all())
    }
}

impl provider_bind::jan_klod::interfaces::host_config::Host for TestHost {
    fn get(
        &mut self,
        key: String,
    ) -> std::result::Result<String, provider_bind::jan_klod::interfaces::host_config::ConfigError>
    {
        self.section
            .get(&key)
            .ok_or(provider_bind::jan_klod::interfaces::host_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }
    fn all(
        &mut self,
    ) -> std::result::Result<String, provider_bind::jan_klod::interfaces::host_config::ConfigError>
    {
        Ok(self.section.all())
    }
}

// --- host-http: canned reply, split into ok/err the way the real host does ---

impl provider_bind::jan_klod::interfaces::host_http::Host for TestHost {
    fn fetch(
        &mut self,
        _request: provider_bind::jan_klod::interfaces::host_http::HttpRequest,
    ) -> std::result::Result<
        provider_bind::jan_klod::interfaces::host_http::HttpResponse,
        provider_bind::jan_klod::interfaces::host_http::HttpError,
    > {
        use provider_bind::jan_klod::interfaces::host_http::{HttpError, HttpResponse};
        let MockHttp { status, body } = self.http.clone();
        if status >= 500 {
            Err(HttpError::ServerError(status))
        } else if status >= 400 {
            Err(HttpError::ClientError(status))
        } else {
            Ok(HttpResponse {
                status,
                headers: vec![],
                body,
            })
        }
    }
}

/// Resolve a staged guest at `<repo>/ext/<file>`, relative to this crate. Returns
/// `None` (with a skip note) when the component is absent, so an unbuilt tree
/// still passes.
fn staged_component(engine: &Engine, file: &str) -> Option<Component> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "..", "ext", file]
        .iter()
        .collect();
    if !path.exists() {
        eprintln!(
            "skipping: {} not staged — run `make extensions` (or `make harness`)",
            path.display()
        );
        return None;
    }
    Some(Component::from_file(engine, &path).expect("staged component should compile"))
}

#[test]
fn store_memory_lifecycle_and_roundtrip() -> Result<()> {
    use store_bind::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, HealthStatus,
    };
    use store_bind::exports::jan_klod::interfaces::memory_store::StoreError;
    use store_bind::jan_klod::interfaces::{host_config, host_log};
    use store_bind::StoreWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "store-memory.wasm") else {
        return Ok(());
    };

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

    let host = TestHost::new(
        json!({ "enabled": true }),
        MockHttp { status: 200, body: vec![] },
    );
    let mut store = Store::new(&engine, host);
    let world = StoreWorld::instantiate(&mut store, &component, &linker)?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let mem = world.jan_klod_interfaces_memory_store();

    // Lifecycle: init -> start -> healthy.
    let ctx = ExtensionContext {
        id: "store.memory".to_string(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)?
        .map_err(wasmtime::Error::msg)?;
    lifecycle.call_start(&mut store)?.map_err(wasmtime::Error::msg)?;
    assert!(matches!(
        lifecycle.call_health(&mut store)?,
        HealthStatus::Up
    ));

    let ns = "notes";

    // set -> get round-trips the value and assigns a stable id.
    let first = mem
        .call_set(&mut store, ns, "k1", "\"hello\"")?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    let got = mem
        .call_get(&mut store, ns, "k1")?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert_eq!(got.value, "\"hello\"");
    assert_eq!(got.id, first.id);

    // Re-set updates the value in place, keeping the same id.
    let updated = mem
        .call_set(&mut store, ns, "k1", "\"world\"")?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert_eq!(updated.id, first.id, "update keeps the row id");
    assert_eq!(updated.value, "\"world\"");

    // A second key, then list/recent/search across the namespace.
    mem.call_set(&mut store, ns, "k2", "\"second\"")?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;

    let keys = mem
        .call_list_keys(&mut store, ns)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|e| e.value.is_empty()), "list omits payloads");

    let recent = mem
        .call_recent(&mut store, ns, 10)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert_eq!(recent.len(), 2);
    assert!(recent.iter().any(|e| e.value == "\"world\""));

    let limited = mem
        .call_recent(&mut store, ns, 1)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert_eq!(limited.len(), 1, "recent honours the limit");

    let found = mem
        .call_search(&mut store, ns, "second", 10)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key, "k2");

    // delete -> get reports the key gone; purge clears the namespace.
    mem.call_delete(&mut store, ns, "k1")?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert!(matches!(
        mem.call_get(&mut store, ns, "k1")?,
        Err(StoreError::NotFound)
    ));

    mem.call_purge_namespace(&mut store, ns)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    let after = mem
        .call_list_keys(&mut store, ns)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;
    assert!(after.is_empty(), "purge clears the namespace");

    lifecycle.call_stop(&mut store)?;
    assert!(
        store.data().logs.iter().any(|l| l.starts_with("store-memory:")),
        "the guest logged through host-log"
    );
    Ok(())
}

#[test]
fn provider_openai_complete_streams_text() -> Result<()> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionChunk, CompletionRequest, Message, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log};
    use provider_bind::ProviderWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "provider-openai.wasm") else {
        return Ok(());
    };

    // A canned, non-streaming Chat Completions reply.
    let body = json!({
        "choices": [{
            "message": { "role": "assistant", "content": "pong" },
            "finish_reason": "stop"
        }]
    });
    let host = TestHost::new(
        json!({ "base-url": "http://mock/v1", "model": "mock-1", "api-key": "test" }),
        MockHttp {
            status: 200,
            body: serde_json::to_vec(&body).unwrap(),
        },
    );

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

    let mut store = Store::new(&engine, host);
    let world = ProviderWorld::instantiate(&mut store, &component, &linker)?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let provider = world.jan_klod_interfaces_llm_provider();

    let ctx = ExtensionContext {
        id: "provider.openai".to_string(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)?
        .map_err(wasmtime::Error::msg)?;
    lifecycle.call_start(&mut store)?.map_err(wasmtime::Error::msg)?;

    let request = CompletionRequest {
        model: String::new(), // falls back to the configured model
        messages: vec![Message {
            role: Role::User,
            content: "ping".to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: Some(16),
        temperature: Some(0.0),
    };

    let handle = provider
        .call_complete(&mut store, &request)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;

    // The canned body parses to exactly one text delta, closed by done("stop").
    let mut text = String::new();
    let mut done_reason = None;
    while let Some(chunk) = provider.call_next_chunk(&mut store, handle)? {
        match chunk {
            CompletionChunk::TextDelta(t) => text.push_str(&t),
            CompletionChunk::Done(reason) => {
                done_reason = Some(reason);
                break;
            }
            CompletionChunk::ToolCallRequest(call) => {
                panic!("unexpected tool call: {call:?}");
            }
        }
    }
    provider.call_close_stream(&mut store, handle)?;

    assert_eq!(text, "pong");
    assert_eq!(done_reason.as_deref(), Some("stop"));
    Ok(())
}

#[test]
fn provider_openai_maps_auth_error() -> Result<()> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionRequest, Message, ProviderError, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log};
    use provider_bind::ProviderWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "provider-openai.wasm") else {
        return Ok(());
    };

    // A 401 at the host boundary becomes client-error(401); the guest must map it
    // to auth-failed.
    let host = TestHost::new(
        json!({ "base-url": "http://mock/v1", "model": "mock-1", "api-key": "bad" }),
        MockHttp { status: 401, body: vec![] },
    );

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

    let mut store = Store::new(&engine, host);
    let world = ProviderWorld::instantiate(&mut store, &component, &linker)?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let provider = world.jan_klod_interfaces_llm_provider();

    let ctx = ExtensionContext {
        id: "provider.openai".to_string(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)?
        .map_err(wasmtime::Error::msg)?;
    lifecycle.call_start(&mut store)?.map_err(wasmtime::Error::msg)?;

    let request = CompletionRequest {
        model: String::new(),
        messages: vec![Message {
            role: Role::User,
            content: "ping".to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: Some(16),
        temperature: Some(0.0),
    };

    let result = provider.call_complete(&mut store, &request)?;
    assert!(
        matches!(result, Err(ProviderError::AuthFailed)),
        "401 should map to auth-failed, got {result:?}"
    );
    Ok(())
}
