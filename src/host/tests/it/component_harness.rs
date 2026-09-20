//! Component test harness — load a staged guest, wire host capabilities, and
//! verify its WIT interface end-to-end through the Component Model, offline and
//! deterministically. It instantiates each category world, backs imports with
//! a reusable [`TestHost`] (config section, captured logs, canned `host-http`),
//! then runs lifecycle plus the guest's interface.
//!
//! Each test skips with a note when its component is not staged in `ext/`, so a
//! bare `cargo test` stays green. Build guests first (`make extensions`) or
//! run the bundled target (`make harness`).

// Dominated by `bindgen!`-generated code; exempt from the workspace lints, as the
// sibling `provider_probe` example is.
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::PathBuf;

use jan_klod_core::ConfigSection;
use serde_json::json;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::common;

/// `provider-world`: lifecycle + `llm-provider`, also imports `host-http`.
mod provider_bind {
    wasmtime::component::bindgen!({
        path: "../../wit",
        world: "provider-world",
    });
}

/// `skill-registry-world`: lifecycle + `skill-registry`, imports `host-fs`.
mod skill_registry_bind {
    wasmtime::component::bindgen!({
        path: "../../wit",
        world: "skill-registry-world",
    });
}

/// A canned `host-http` reply. The harness mirrors the host boundary in
/// [`jan_klod_core::http`]: 5xx → `server-error`, 4xx → `client-error`, else
/// → success. One mock serves both happy path and error-mapping tests.
#[derive(Clone)]
struct MockHttp {
    status: u16,
    body: Vec<u8>,
}

/// Reusable host backing every guest's imports: config section, log buffer,
/// and canned HTTP reply.
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

// --- host-secrets: grant and retrieve from environment ---

impl provider_bind::jan_klod::interfaces::host_secrets::Host for TestHost {
    fn get(
        &mut self,
        name: String,
    ) -> std::result::Result<String, provider_bind::jan_klod::interfaces::host_secrets::SecretsError>
    {
        use provider_bind::jan_klod::interfaces::host_secrets::SecretsError;
        // Check if the instance is granted the secrets capability.
        match self.section.get("secrets") {
            Some(s) => {
                // Check if it's the boolean true (will be "true" as a JSON string)
                if s != "true" {
                    return Err(SecretsError::Denied);
                }
            }
            None => {
                // Not granted
                return Err(SecretsError::Denied);
            }
        }

        // Attempt to read from environment
        match std::env::var(&name) {
            Ok(value) => Ok(value),
            Err(std::env::VarError::NotPresent) => Err(SecretsError::NotFound),
            Err(std::env::VarError::NotUnicode(_)) => Err(SecretsError::Backend),
        }
    }
}

// --- host-fs: read, write, list, and exists for skill-registry-world ---

impl skill_registry_bind::jan_klod::interfaces::host_fs::Host for TestHost {
    fn read(
        &mut self,
        path: String,
    ) -> std::result::Result<String, skill_registry_bind::jan_klod::interfaces::host_fs::FsError>
    {
        use skill_registry_bind::jan_klod::interfaces::host_fs::FsError;
        std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FsError::NotFound
            } else {
                FsError::Io
            }
        })
    }

    fn write(
        &mut self,
        path: String,
        contents: String,
    ) -> std::result::Result<(), skill_registry_bind::jan_klod::interfaces::host_fs::FsError> {
        use skill_registry_bind::jan_klod::interfaces::host_fs::FsError;
        // Create parent directories if needed
        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|_| FsError::Io)?;
            }
        }
        std::fs::write(&path, contents).map_err(|_| FsError::Io)
    }

    fn list_dir(
        &mut self,
        path: String,
    ) -> std::result::Result<
        Vec<skill_registry_bind::jan_klod::interfaces::host_fs::Entry>,
        skill_registry_bind::jan_klod::interfaces::host_fs::FsError,
    > {
        use skill_registry_bind::jan_klod::interfaces::host_fs::{Entry, FsError};
        match std::fs::read_dir(&path) {
            Ok(entries) => {
                let mut result = Vec::new();
                for entry in entries {
                    if let Ok(entry) = entry {
                        if let Ok(metadata) = entry.metadata() {
                            result.push(Entry {
                                name: entry.file_name().to_string_lossy().to_string(),
                                is_dir: metadata.is_dir(),
                            });
                        }
                    }
                }
                Ok(result)
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Err(FsError::NotFound)
                } else {
                    Err(FsError::Io)
                }
            }
        }
    }

    fn exists(&mut self, path: String) -> bool {
        std::path::Path::new(&path).exists()
    }
}

// --- host-log for skill-registry-world ---

impl skill_registry_bind::jan_klod::interfaces::host_log::Host for TestHost {
    fn log(
        &mut self,
        _level: skill_registry_bind::jan_klod::interfaces::host_log::LogLevel,
        component: String,
        message: String,
        _fields: Vec<skill_registry_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
        self.logs.push(format!("{component}: {message}"));
    }
}

// --- host-config for skill-registry-world ---

impl skill_registry_bind::jan_klod::interfaces::host_config::Host for TestHost {
    fn get(
        &mut self,
        key: String,
    ) -> std::result::Result<
        String,
        skill_registry_bind::jan_klod::interfaces::host_config::ConfigError,
    > {
        self.section
            .get(&key)
            .ok_or(skill_registry_bind::jan_klod::interfaces::host_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }
    fn all(
        &mut self,
    ) -> std::result::Result<
        String,
        skill_registry_bind::jan_klod::interfaces::host_config::ConfigError,
    > {
        Ok(self.section.all())
    }
}

/// Resolve a staged guest at `<repo>/ext/<file>`. Returns `None` (with a skip
/// note) when absent, so an unbuilt tree still passes.
fn staged_component(engine: &Engine, file: &str) -> Option<Component> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "ext", file]
        .iter()
        .collect();
    if !common::guests_staged(&[file]) {
        return None;
    }
    Some(Component::from_file(engine, &path).expect("staged component should compile"))
}

#[test]
fn provider_openai_complete_streams_text() -> Result<()> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionChunk, CompletionRequest, Message, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log, host_secrets};
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
    host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

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
    lifecycle
        .call_start(&mut store)?
        .map_err(wasmtime::Error::msg)?;

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
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log, host_secrets};
    use provider_bind::ProviderWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "provider-openai.wasm") else {
        return Ok(());
    };

    // A 401 at the host boundary becomes client-error(401); the guest must map it
    // to auth-failed.
    let host = TestHost::new(
        json!({ "base-url": "http://mock/v1", "model": "mock-1", "api-key": "bad" }),
        MockHttp {
            status: 401,
            body: vec![],
        },
    );

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

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
    lifecycle
        .call_start(&mut store)?
        .map_err(wasmtime::Error::msg)?;

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

/// A reply carrying **both** a preamble and tool calls must yield both — a
/// parser that treats them as `if tool_calls ... else if content ...` silently
/// drops the preamble text that a model narrating before acting routinely sends.
#[test]
fn provider_openai_keeps_text_that_accompanies_tool_calls() -> Result<()> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionChunk, CompletionRequest, Message, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log, host_secrets};
    use provider_bind::ProviderWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "provider-openai.wasm") else {
        return Ok(());
    };

    let body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "I'll read the file first.",
                "tool_calls": [{
                    "id": "call-1",
                    "function": { "name": "fs", "arguments": "{\"op\":\"read\"}" }
                }]
            },
            "finish_reason": "tool_calls"
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
    host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

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
    lifecycle
        .call_start(&mut store)?
        .map_err(wasmtime::Error::msg)?;

    let request = CompletionRequest {
        model: String::new(),
        messages: vec![Message {
            role: Role::User,
            content: "read it".to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: None,
        temperature: None,
    };
    let handle = provider
        .call_complete(&mut store, &request)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;

    let mut text = String::new();
    let mut calls = Vec::new();
    let mut done_reason = None;
    while let Some(chunk) = provider.call_next_chunk(&mut store, handle)? {
        match chunk {
            CompletionChunk::TextDelta(t) => text.push_str(&t),
            CompletionChunk::ToolCallRequest(call) => calls.push(call.name),
            CompletionChunk::Done(reason) => {
                done_reason = Some(reason);
                break;
            }
        }
    }
    provider.call_close_stream(&mut store, handle)?;

    assert_eq!(
        text, "I'll read the file first.",
        "the preamble survives alongside the calls"
    );
    assert_eq!(calls, vec!["fs".to_string()], "the tool call still arrives");
    assert_eq!(done_reason.as_deref(), Some("tool_calls"));
    Ok(())
}

#[test]
fn registry_skills_loads_bundled_and_workspace_overrides() -> Result<()> {
    use skill_registry_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use skill_registry_bind::jan_klod::interfaces::{host_config, host_fs, host_log};
    use skill_registry_bind::SkillRegistryWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "registry-skills.wasm") else {
        return Ok(());
    };

    // Create a temporary directory structure for testing.
    let temp = common::TempDir(PathBuf::from("/tmp/registry-skills-test"));
    let _ = std::fs::remove_dir_all(&temp.0); // Clean up if it exists
    std::fs::create_dir_all(&temp.0)?;
    std::fs::create_dir_all(temp.0.join("skills"))?;
    std::fs::create_dir_all(temp.0.join(".agents/skills"))?;

    // Copy bundled skills from the distribution into the test structure.
    let dist_skills = common::repo_root().join("scripts/distributions/coding/skills");
    if dist_skills.exists() {
        for entry in std::fs::read_dir(&dist_skills)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "md") {
                let file_name = path.file_name().unwrap();
                std::fs::copy(&path, temp.0.join("skills").join(file_name))?;
            }
        }
    }

    // Save current directory and change to the test directory.
    let old_cwd = std::env::current_dir()?;
    std::env::set_current_dir(&temp.0)?;

    // Ensure cleanup on exit by using a scope guard.
    let _guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Set up the component runtime.
        let host = TestHost::new(
            json!({}),
            MockHttp {
                status: 200,
                body: vec![],
            },
        );

        let mut linker: Linker<TestHost> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("wasi linker setup");
        host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).expect("host_log linker");
        host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
            .expect("host_config linker");
        host_fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).expect("host_fs linker");

        let mut store = Store::new(&engine, host);
        let world = SkillRegistryWorld::instantiate(&mut store, &component, &linker)
            .expect("instantiate registry-skills");
        let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
        let registry = world.jan_klod_interfaces_skill_registry();

        // Initialize the component.
        let ctx = ExtensionContext {
            id: "registry-skills".to_string(),
            version: "0.0.0".to_string(),
        };
        lifecycle
            .call_init(&mut store, &ctx)
            .expect("init call")
            .expect("init result");
        lifecycle
            .call_start(&mut store)
            .expect("start call")
            .expect("start result");

        // Verify bundled skills are loaded.
        let skills = registry
            .call_list_skills(&mut store)
            .expect("list_skills call")
            .expect("list_skills result");
        assert_eq!(
            skills.len(),
            3,
            "should load exactly 3 bundled skills, got {}",
            skills.len()
        );

        // Check that all three expected skills are present with correct names.
        let skill_names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            skill_names.contains(&"commit"),
            "commit skill not found in {:?}",
            skill_names
        );
        assert!(
            skill_names.contains(&"review"),
            "review skill not found in {:?}",
            skill_names
        );
        assert!(
            skill_names.contains(&"plan"),
            "plan skill not found in {:?}",
            skill_names
        );

        // Find the commit skill and note its original description.
        let original_commit = skills
            .iter()
            .find(|s| s.name == "commit")
            .expect("commit skill should exist");
        let original_description = original_commit.description.clone();

        // Create a workspace override for the commit skill.
        let workspace_skill = r#"---
name: commit
description: Workspace commit guide
---
# Workspace-specific commit guidance
This is the workspace override version.
"#;
        std::fs::write(temp.0.join(".agents/skills/commit.md"), workspace_skill)
            .expect("write workspace override");

        // Reload skills to pick up the workspace override.
        registry
            .call_reload(&mut store)
            .expect("reload call")
            .expect("reload result");

        // Verify that the workspace version now takes precedence.
        let reloaded_skills = registry
            .call_list_skills(&mut store)
            .expect("list_skills after reload")
            .expect("list_skills result");
        assert_eq!(
            reloaded_skills.len(),
            3,
            "should still have 3 skills after reload"
        );

        let reloaded_commit = reloaded_skills
            .iter()
            .find(|s| s.name == "commit")
            .expect("commit skill should exist after reload");
        assert_eq!(
            reloaded_commit.description, "Workspace commit guide",
            "workspace override should be active; original was: {}, got: {}",
            original_description, reloaded_commit.description
        );

        // Clean up the workspace override file.
        std::fs::remove_file(temp.0.join(".agents/skills/commit.md"))
            .expect("cleanup workspace override");
    }));

    // Restore original directory.
    let _ = std::env::set_current_dir(old_cwd);

    Ok(())
}
