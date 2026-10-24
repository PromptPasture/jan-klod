//! `registry-mcp` — SSE MCP gateway implementing `mcp-registry`.
//! Reads `servers` from `host-config`, connects at `init`, and proxies tool calls over `host-http` or stdio.
//! Stdio entries name a child granted in `execution.long-lived`.

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
use bindings::jan_klod::interfaces::host_process;

/// How a server is reached: one JSON-RPC protocol over two wires.
#[derive(Clone)]
enum Wire {
    /// SSE / streamable-HTTP over `host-http`.
    Http(String),
    /// Stdio child held open by the host.
    Stdio(u32),
}

/// One configured MCP server.
#[derive(Clone)]
struct Server {
    id: String,
    wire: Wire,
    status: bool,
    tools: Vec<McpTool>,
}

impl Server {
    /// Wire name for `list-servers`.
    const fn transport_name(&self) -> &'static str {
        match self.wire {
            Wire::Http(_) => "sse",
            Wire::Stdio(_) => "stdio",
        }
    }
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

/// Total stdio reply timeout; matches the HTTP transport's `timeout_ms`.
const STDIO_REPLY_BUDGET_MS: u32 = 10_000;

/// One read's window; short enough for liveness checks, long enough not to spin.
const STDIO_POLL_MS: u32 = 200;

/// Send a JSON-RPC request over a stdio child and read its reply.
/// Newline-delimited, per the MCP stdio transport spec.
fn json_rpc_stdio(child: u32, method: &str, params: Option<Value>) -> Result<Value, String> {
    let id = next_id();
    let mut request = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if let Some(p) = params {
        request["params"] = p;
    }
    let line = format!("{request}\n");
    host_process::write_stdin(child, &line).map_err(|e| format!("write failed: {e:?}"))?;

    let mut buffered = String::new();
    let mut waited = 0;
    while waited < STDIO_REPLY_BUDGET_MS {
        let chunk = host_process::read_stdout(child, 8192, STDIO_POLL_MS)
            .map_err(|e| format!("read failed: {e:?}"))?;
        if chunk.is_empty() {
            if !host_process::is_running(child) {
                return Err("server exited".to_owned());
            }
            waited += STDIO_POLL_MS;
            continue;
        }
        buffered.push_str(&chunk);
        if let Some(end) = buffered.find('\n') {
            let line = buffered[..end].trim();
            return serde_json::from_str(line).map_err(|e| format!("malformed reply: {e}"));
        }
    }
    Err(format!("no reply within {STDIO_REPLY_BUDGET_MS}ms"))
}

/// One JSON-RPC call, dispatched over the appropriate wire.
fn json_rpc(wire: &Wire, method: &str, params: Option<Value>) -> Result<Value, String> {
    match wire {
        Wire::Http(url) => json_rpc_post(url, method, params),
        Wire::Stdio(child) => json_rpc_stdio(*child, method, params),
    }
}

/// Start a stdio server and complete the MCP handshake.
/// The host supplies the command, so this guest cannot choose what runs.
fn connect_stdio(id: &str, child_name: &str) -> Server {
    let Ok(child) = host_process::spawn(child_name) else {
        log(
            LogLevel::Warn,
            &format!(
                "server {id}: `{child_name}` was refused — name it in \
                 `execution.long-lived` to grant it"
            ),
        );
        return Server {
            id: id.to_owned(),
            wire: Wire::Stdio(0),
            status: false,
            tools: Vec::new(),
        };
    };
    let wire = Wire::Stdio(child);
    if let Err(err) = json_rpc(
        &wire,
        "initialize",
        Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "jan-klod", "version": "0.1.0" },
        })),
    ) {
        log(
            LogLevel::Warn,
            &format!("server {id}: initialize failed: {err}"),
        );
        return Server {
            id: id.to_owned(),
            wire,
            status: false,
            tools: Vec::new(),
        };
    }
    connect_over(id, wire)
}

fn connect_server(id: &str, url: &str) -> Server {
    connect_over(id, Wire::Http(url.to_owned()))
}

/// Ask a server for its tools; `tools/list` is identical on both wires.
fn connect_over(id: &str, wire: Wire) -> Server {
    match json_rpc(&wire, "tools/list", None) {
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
                wire,
                status: true,
                tools,
            }
        }
        Err(err) => {
            log(LogLevel::Warn, &format!("failed to connect to {id}: {err}"));
            Server {
                id: id.to_owned(),
                wire,
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
                if id.is_empty() {
                    continue;
                }
                if transport == "stdio" {
                    let child = entry
                        .get("child")
                        .and_then(Value::as_str)
                        .filter(|c| !c.is_empty())
                        .unwrap_or(id);
                    servers.push(connect_stdio(id, child));
                    continue;
                }
                if url.is_empty() {
                    continue;
                }
                if transport != "sse" && transport != "streamable-http" && !transport.is_empty() {
                    log(
                        LogLevel::Warn,
                        &format!(
                            "server {id}: transport `{transport}` is not one of \
                             sse, streamable-http, stdio; skipping"
                        ),
                    );
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
                    transport: srv.transport_name().to_owned(),
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
        let (wire, bare_name) = SERVERS
            .with(|s| {
                s.borrow().iter().find_map(|srv| {
                    if !srv.status {
                        return None;
                    }
                    let bare = name.split("::").last().unwrap_or(&name);
                    srv.tools
                        .iter()
                        .find(|t| t.name == bare)
                        .map(|_| (srv.wire.clone(), bare.to_owned()))
                })
            })
            .ok_or(McpError::ToolNotFound)?;

        let args: Value = serde_json::from_str(&arguments)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::default()));
        let params = json!({"name": bare_name, "arguments": args});
        let resp =
            json_rpc(&wire, "tools/call", Some(params)).map_err(|_| McpError::InvocationFailed)?;

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
        let (idx, wire) = SERVERS
            .with(|s| {
                s.borrow()
                    .iter()
                    .enumerate()
                    .find_map(|(i, srv)| (srv.id == server_id).then(|| (i, srv.wire.clone())))
            })
            .ok_or(McpError::ServerNotFound)?;

        let updated = connect_over(&server_id, wire);
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
