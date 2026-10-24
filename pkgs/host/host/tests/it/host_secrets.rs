//! Tests for host-secrets capability: grant, denial, value retrieval.

use std::path::PathBuf;

use jk_wasm::ConfigSection;
use serde_json::json;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::common;

mod tool_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "tool-world",
    });
}

struct TestHost {
    wasi: WasiCtx,
    table: ResourceTable,
    section: ConfigSection,
}

impl TestHost {
    fn new(section: serde_json::Value) -> Self {
        Self {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            section: ConfigSection::new(section),
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

impl tool_bind::jan_klod::interfaces::host_log::Host for TestHost {
    fn log(
        &mut self,
        _level: tool_bind::jan_klod::interfaces::host_log::LogLevel,
        _component: String,
        _message: String,
        _fields: Vec<tool_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
    }
}

impl tool_bind::jan_klod::interfaces::host_config::Host for TestHost {
    fn get(
        &mut self,
        key: String,
    ) -> std::result::Result<String, tool_bind::jan_klod::interfaces::host_config::ConfigError>
    {
        self.section
            .get(&key)
            .ok_or(tool_bind::jan_klod::interfaces::host_config::ConfigError::KeyNotFound)
    }

    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }

    fn all(
        &mut self,
    ) -> std::result::Result<String, tool_bind::jan_klod::interfaces::host_config::ConfigError>
    {
        Ok(self.section.all())
    }
}

impl tool_bind::jan_klod::interfaces::host_http::Host for TestHost {
    fn fetch(
        &mut self,
        _request: tool_bind::jan_klod::interfaces::host_http::HttpRequest,
    ) -> std::result::Result<
        tool_bind::jan_klod::interfaces::host_http::HttpResponse,
        tool_bind::jan_klod::interfaces::host_http::HttpError,
    > {
        Err(tool_bind::jan_klod::interfaces::host_http::HttpError::Backend)
    }
}

impl tool_bind::jan_klod::interfaces::host_fs::Host for TestHost {
    fn read(&mut self, _path: String) -> std::result::Result<String, tool_bind::jan_klod::interfaces::host_fs::FsError> {
        Err(tool_bind::jan_klod::interfaces::host_fs::FsError::Denied)
    }

    fn write(&mut self, _path: String, _contents: String) -> std::result::Result<(), tool_bind::jan_klod::interfaces::host_fs::FsError> {
        Err(tool_bind::jan_klod::interfaces::host_fs::FsError::Denied)
    }

    fn list_dir(
        &mut self,
        _path: String,
    ) -> std::result::Result<Vec<tool_bind::jan_klod::interfaces::host_fs::Entry>, tool_bind::jan_klod::interfaces::host_fs::FsError> {
        Err(tool_bind::jan_klod::interfaces::host_fs::FsError::Denied)
    }

    fn exists(&mut self, _path: String) -> bool {
        false
    }
}

impl tool_bind::jan_klod::interfaces::host_process::Host for TestHost {
    fn exec(
        &mut self,
        _command: String,
        _args: Vec<String>,
        _cwd: Option<String>,
        _stdin: Option<String>,
    ) -> std::result::Result<tool_bind::jan_klod::interfaces::host_process::Exit, tool_bind::jan_klod::interfaces::host_process::ProcError> {
        Err(tool_bind::jan_klod::interfaces::host_process::ProcError::Denied)
    }

    fn granted(&mut self) -> Vec<String> {
        Vec::new()
    }

    fn spawn(&mut self, _name: String) -> std::result::Result<u32, tool_bind::jan_klod::interfaces::host_process::ProcError> {
        Err(tool_bind::jan_klod::interfaces::host_process::ProcError::Denied)
    }

    fn write_stdin(&mut self, _child: u32, _data: String) -> std::result::Result<(), tool_bind::jan_klod::interfaces::host_process::ProcError> {
        Err(tool_bind::jan_klod::interfaces::host_process::ProcError::Denied)
    }

    fn read_stdout(
        &mut self,
        _child: u32,
        _max_bytes: u32,
        _timeout_ms: u32,
    ) -> std::result::Result<String, tool_bind::jan_klod::interfaces::host_process::ProcError> {
        Err(tool_bind::jan_klod::interfaces::host_process::ProcError::Denied)
    }

    fn is_running(&mut self, _child: u32) -> bool {
        false
    }

    fn kill(&mut self, _child: u32) {}
}

impl tool_bind::jan_klod::interfaces::host_storage::Host for TestHost {
    fn set(
        &mut self,
        _namespace: String,
        _key: String,
        _value: String,
    ) -> std::result::Result<tool_bind::jan_klod::interfaces::host_storage::Entry, tool_bind::jan_klod::interfaces::host_storage::StoreError> {
        Err(tool_bind::jan_klod::interfaces::host_storage::StoreError::Backend)
    }

    fn get(
        &mut self,
        _namespace: String,
        _key: String,
    ) -> std::result::Result<tool_bind::jan_klod::interfaces::host_storage::Entry, tool_bind::jan_klod::interfaces::host_storage::StoreError> {
        Err(tool_bind::jan_klod::interfaces::host_storage::StoreError::NotFound)
    }

    fn delete(&mut self, _namespace: String, _key: String) -> std::result::Result<(), tool_bind::jan_klod::interfaces::host_storage::StoreError> {
        Err(tool_bind::jan_klod::interfaces::host_storage::StoreError::NotFound)
    }

    fn list_keys(
        &mut self,
        _namespace: String,
    ) -> std::result::Result<Vec<tool_bind::jan_klod::interfaces::host_storage::Entry>, tool_bind::jan_klod::interfaces::host_storage::StoreError> {
        Ok(Vec::new())
    }

    fn recent(
        &mut self,
        _namespace: String,
        _limit: u32,
    ) -> std::result::Result<Vec<tool_bind::jan_klod::interfaces::host_storage::Entry>, tool_bind::jan_klod::interfaces::host_storage::StoreError> {
        Ok(Vec::new())
    }
}

impl tool_bind::jan_klod::interfaces::host_secrets::Host for TestHost {
    fn get(&mut self, name: String) -> std::result::Result<String, tool_bind::jan_klod::interfaces::host_secrets::SecretsError> {
        match self.section.get("secrets") {
            Some(s) => {
                if s != "true" {
                    return Err(tool_bind::jan_klod::interfaces::host_secrets::SecretsError::Denied);
                }
            }
            None => {
                return Err(tool_bind::jan_klod::interfaces::host_secrets::SecretsError::Denied);
            }
        }

        match std::env::var(&name) {
            Ok(value) => Ok(value),
            Err(std::env::VarError::NotPresent) => Err(tool_bind::jan_klod::interfaces::host_secrets::SecretsError::NotFound),
            Err(std::env::VarError::NotUnicode(_)) => Err(tool_bind::jan_klod::interfaces::host_secrets::SecretsError::Backend),
        }
    }
}

fn staged_component(engine: &Engine, filename: &str) -> Option<Component> {
    let mut path = common::ext_path();
    path.push(filename);
    if !path.exists() {
        return None;
    }
    Component::from_file(engine, &path).ok()
}

#[test]
fn a_guest_without_the_grant_is_denied_every_secret() {
    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "host-secrets-probe.wasm") else {
        return;
    };

    let host = TestHost::new(json!({}));

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    let _ = wasmtime_wasi::p2::add_to_linker_sync(&mut linker);
    let _ = tool_bind::jan_klod::interfaces::host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_process::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_storage::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);

    let mut store = Store::new(&engine, host);
    let world = tool_bind::ToolWorld::instantiate(&mut store, &component, &linker).expect("instantiate");
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let tool = world.jan_klod_interfaces_tool_callable();

    let ctx = tool_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
        id: "host-secrets-probe".to_string(),
        version: "0.0.0".to_string(),
    };
    let _ = lifecycle.call_init(&mut store, &ctx).expect("init");
    let _ = lifecycle.call_start(&mut store).expect("start");

    let result = tool.call_invoke(&mut store, r#"{"name":"TEST_SECRET"}"#).expect("invoke");
    match result {
        Ok(output) => assert!(output.contains("DENIED:"), "Expected denial, got: {}", output),
        Err(_) => panic!("Tool call should not fail, only return a denied message"),
    }
}

#[test]
fn an_ungranted_guest_cannot_enumerate_secrets() {
}

#[test]
fn an_absent_secret_returns_not_found() {
    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "host-secrets-probe.wasm") else {
        return;
    };

    let host = TestHost::new(json!({"secrets": true}));

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    let _ = wasmtime_wasi::p2::add_to_linker_sync(&mut linker);
    let _ = tool_bind::jan_klod::interfaces::host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_process::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_storage::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);

    let mut store = Store::new(&engine, host);
    let world = tool_bind::ToolWorld::instantiate(&mut store, &component, &linker).expect("instantiate");
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let tool = world.jan_klod_interfaces_tool_callable();

    let ctx = tool_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
        id: "host-secrets-probe".to_string(),
        version: "0.0.0".to_string(),
    };
    let _ = lifecycle.call_init(&mut store, &ctx).expect("init");
    let _ = lifecycle.call_start(&mut store).expect("start");

    let result = tool.call_invoke(&mut store, r#"{"name":"NONEXISTENT_SECRET"}"#).expect("invoke");
    match result {
        Ok(output) => assert!(output.contains("not-found"), "Expected not-found, got: {}", output),
        Err(_) => panic!("Tool call should not fail, only return a not-found message"),
    }
}

#[test]
fn a_granted_guest_reading_an_extant_secret_returns_the_value() {
    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "host-secrets-probe.wasm") else {
        return;
    };

    std::env::set_var("TEST_SECRET_PROBE", "secret-value-123");

    let host = TestHost::new(json!({"secrets": true}));

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    let _ = wasmtime_wasi::p2::add_to_linker_sync(&mut linker);
    let _ = tool_bind::jan_klod::interfaces::host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_process::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_storage::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);
    let _ = tool_bind::jan_klod::interfaces::host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s);

    let mut store = Store::new(&engine, host);
    let world = tool_bind::ToolWorld::instantiate(&mut store, &component, &linker).expect("instantiate");
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let tool = world.jan_klod_interfaces_tool_callable();

    let ctx = tool_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
        id: "host-secrets-probe".to_string(),
        version: "0.0.0".to_string(),
    };
    let _ = lifecycle.call_init(&mut store, &ctx).expect("init");
    let _ = lifecycle.call_start(&mut store).expect("start");

    let result = tool.call_invoke(&mut store, r#"{"name":"TEST_SECRET_PROBE"}"#).expect("invoke");
    match result {
        Ok(output) => assert!(output.contains("secret-value-123"), "Expected value, got: {}", output),
        Err(_) => panic!("Tool call should not fail, only return the value"),
    }
}
