//! `registry-mcp` — SSE MCP gateway implementing `mcp-registry`.
//!
//! Reads a `servers` array from `host-config` (each entry: `{name, transport, url}`),
//! connects to each SSE endpoint at `init`, lists tools, and proxies `invoke-tool`
//! via `host-http` (JSON-RPC over HTTP POST to the server URL).
//!
//! v0.1.0: SSE/streamable-HTTP transport only. Stdio requires host-process.
//!
//! MCP wire protocol (2024-11-05 spec):
//! - `tools/list` → POST `{"jsonrpc":"2.0","id":N,"method":"tools/list"}`
//! - `tools/call`  → POST `{"jsonrpc":"2.0","id":N,"method":"tools/call","params":{"name":"...","arguments":{...}}}`

#[allow(
    unsafe_code,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]
mod bindings {
    wit_bindgen::generate!({
        world: "mcp-registry-world",
        path: "../../../wit",
    });
}

use std::cell::RefCell;

use serde_json::{json, Value};

use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
    ExtensionContext, Guest as Lifecycle, HealthStatus,
};
use bindings::exports::jan_klod::interfaces::mcp_registry::{
    Guest as McpRegistry, McpError, ServerInfo, ServerStatus, ToolInfo,
};
use bindings::jan_klod::interfaces::host_config;
use bindings::jan_klod::interfaces::host_http::{self, HttpHeader, HttpRequest};
use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

/// One configured MCP server.
#[derive(Clone)]
struct Server {
    id: String,
    url: String,
    status: bool,
    tools: Vec<McpTool>,
}

#[derive(Clone)]
struct McpTool {
    name: String,
    description: String,
    input_schema: String,
}

thread_local! {
    static SERVERS: RefCell<Vec<Server>> = const { RefCell::new(Vec::new()) };
    static REQ_ID: RefCell<u64> = const { RefCell::new(1) };
}

fn log(level: LogLevel, message: &str) {
    host_log::log(level, "registry-mcp", message, &[]);
}

fn next_id() -> u64 {
    REQ_ID.with(|n| {
        let id = *n.borrow();
        *n.borrow_mut() = id.wrapping_add(1);
        id
    })
}

fn json_rpc_post(url: &str, method: &str, params: Option<Value>) -> Result<Value, String> {
    let id = next_id();
    let mut body_val = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if let Some(p) = params {
        body_val["params"] = p;
    }
    let body = serde_json::to_vec(&body_val).map_err(|e| e.to_string())?;
    let req = HttpRequest {
        method: "POST".to_owned(),
        url: url.to_owned(),
        headers: vec![HttpHeader {
            name: "Content-Type".to_owned(),
            value: "application/json".to_owned(),
        }],
        body: Some(body),
        timeout_ms: 10_000,
    };
    let resp = host_http::fetch(&req).map_err(|e| format!("{e:?}"))?;
    serde_json::from_slice(&resp.body).map_err(|e| e.to_string())
}

fn connect_server(id: &str, url: &str) -> Server {
    match json_rpc_post(url, "tools/list", None) {
        Ok(resp) => {
            let tools: Vec<McpTool> = resp
                .pointer("/result/tools")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|t| McpTool {
                            name: t
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            description: t
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            input_schema: t
                                .get("inputSchema")
                                .map_or_else(|| "{}".to_owned(), Value::to_string),
                        })
                        .filter(|t| !t.name.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            log(
                LogLevel::Info,
                &format!("connected to {id} ({} tools)", tools.len()),
            );
            Server {
                id: id.to_owned(),
                url: url.to_owned(),
                status: true,
                tools,
            }
        }
        Err(err) => {
            log(LogLevel::Warn, &format!("failed to connect to {id}: {err}"));
            Server {
                id: id.to_owned(),
                url: url.to_owned(),
                status: false,
                tools: Vec::new(),
            }
        }
    }
}

struct Component;

impl Lifecycle for Component {
    fn init(ctx: ExtensionContext) -> Result<(), String> {
        let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        let config: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

        let mut servers = Vec::new();
        if let Some(arr) = config.get("servers").and_then(Value::as_array) {
            for entry in arr {
                let id = entry.get("name").and_then(Value::as_str).unwrap_or("");
                let transport = entry.get("transport").and_then(Value::as_str).unwrap_or("");
                let url = entry.get("url").and_then(Value::as_str).unwrap_or("");
                if id.is_empty() || url.is_empty() {
                    continue;
                }
                if transport != "sse" && transport != "streamable-http" && !transport.is_empty() {
                    log(LogLevel::Warn, &format!("server {id}: transport `{transport}` not supported in v0.1.0 (sse only); skipping"));
                    continue;
                }
                servers.push(connect_server(id, url));
            }
        }
        let count = servers.len();
        SERVERS.with(|s| *s.borrow_mut() = servers);
        log(
            LogLevel::Info,
            &format!("init id={} servers={count}", ctx.id),
        );
        Ok(())
    }

    fn start() -> Result<(), String> {
        log(LogLevel::Info, "started");
        Ok(())
    }

    fn stop() {
        SERVERS.with(|s| s.borrow_mut().clear());
    }

    fn health() -> HealthStatus {
        if SERVERS.with(|s| s.borrow().iter().any(|srv| srv.status)) {
            HealthStatus::Up
        } else {
            HealthStatus::Down
        }
    }
}

impl McpRegistry for Component {
    fn list_servers() -> Result<Vec<ServerInfo>, McpError> {
        Ok(SERVERS.with(|s| {
            s.borrow()
                .iter()
                .map(|srv| ServerInfo {
                    id: srv.id.clone(),
                    status: if srv.status {
                        ServerStatus::Connected
                    } else {
                        ServerStatus::Down
                    },
                    transport: "sse".to_owned(),
                    tool_count: u32::try_from(srv.tools.len()).unwrap_or(u32::MAX),
                })
                .collect()
        }))
    }

    fn list_tools() -> Result<Vec<ToolInfo>, McpError> {
        Ok(SERVERS.with(|s| {
            s.borrow()
                .iter()
                .filter(|srv| srv.status)
                .flat_map(|srv| {
                    srv.tools.iter().map(|t| ToolInfo {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        server_id: srv.id.clone(),
                        input_schema: t.input_schema.clone(),
                    })
                })
                .collect()
        }))
    }

    fn invoke_tool(name: String, arguments: String) -> Result<String, McpError> {
        let (url, bare_name) = SERVERS
            .with(|s| {
                s.borrow().iter().find_map(|srv| {
                    if !srv.status {
                        return None;
                    }
                    // Try exact match, then bare name after `::`
                    let bare = name.split("::").last().unwrap_or(&name);
                    srv.tools
                        .iter()
                        .find(|t| t.name == bare)
                        .map(|_| (srv.url.clone(), bare.to_owned()))
                })
            })
            .ok_or(McpError::ToolNotFound)?;

        let args: Value = serde_json::from_str(&arguments)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::default()));
        let params = json!({"name": bare_name, "arguments": args});
        let resp = json_rpc_post(&url, "tools/call", Some(params))
            .map_err(|_| McpError::InvocationFailed)?;

        if resp.get("error").is_some() {
            return Err(McpError::InvocationFailed);
        }

        let result = resp
            .pointer("/result/content")
            .map(Value::to_string)
            .or_else(|| resp.get("result").map(Value::to_string))
            .unwrap_or_else(|| "{}".to_owned());
        Ok(result)
    }

    fn reconnect(server_id: String) -> Result<(), McpError> {
        let (idx, url) = SERVERS
            .with(|s| {
                s.borrow()
                    .iter()
                    .enumerate()
                    .find_map(|(i, srv)| (srv.id == server_id).then(|| (i, srv.url.clone())))
            })
            .ok_or(McpError::ServerNotFound)?;

        let updated = connect_server(&server_id, &url);
        SERVERS.with(|s| {
            if let Some(slot) = s.borrow_mut().get_mut(idx) {
                *slot = updated;
            }
        });
        Ok(())
    }
}

#[allow(
    unsafe_code,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]
mod glue {
    use crate::{bindings, Component};
    bindings::export!(Component with_types_in bindings);
}
