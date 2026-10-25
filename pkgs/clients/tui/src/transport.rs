//! Transport to the core: over stdio to a spawned gateway, or over REST.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio as ChildIo};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use jk_protocol::{
    compatible, jsonrpc, Command as Rpc, Notification, SessionGetResult, PROTOCOL_VERSION,
};

use crate::StreamEvent;

/// One session from `session/list`: id and preview of first user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session id.
    pub id: String,
    /// First 80 characters of the session's first user message.
    pub preview: String,
}

/// One way to drive a core.
pub trait Transport: Send + Sync {
    /// Allocate a new session and return its id.
    ///
    /// # Errors
    /// A human-readable message if the connection fails or the core refuses.
    fn create_session(&self) -> Result<String, String>;

    /// All sessions with previews, fetched fresh each time: the core owns the session model.
    ///
    /// # Errors
    /// A human-readable message if the connection fails or the core refuses.
    fn session_list(&self) -> Result<Vec<SessionSummary>, String>;

    /// One session's transcript.
    ///
    /// # Errors
    /// A human-readable message if the connection fails or the session does not exist.
    fn session_get(&self, session: &str) -> Result<SessionGetResult, String>;

    /// Stop the running turn.
    ///
    /// # Errors
    /// A human-readable message if the cancellation cannot be sent or acted on.
    fn cancel(&self, session: &str) -> Result<(), String>;

    /// Drive one turn, calling `on_event` as the core reports.
    ///
    /// # Errors
    /// A human-readable message if the connection fails or the turn does.
    fn stream_turn(
        &self,
        session: &str,
        message: &str,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<(), String>;

    /// Answer a [`StreamEvent::Prompt`] that is holding a turn open.
    ///
    /// # Errors
    /// A human-readable message if the answer cannot be sent.
    fn answer(&self, session: &str, answer: &str) -> Result<(), String>;

    /// Steer a running turn (not cancel).
    ///
    /// # Errors
    /// Always over REST, which cannot steer; shown so a dropped keystroke is not silent.
    fn follow_up(&self, session: &str, message: &str) -> Result<(), String>;

    /// What the extensions contributed, as reported at connect.
    fn contributions(&self) -> Vec<jk_protocol::Contributions> {
        Vec::new()
    }

    /// Run a command an extension contributed, returning what it answered.
    ///
    /// # Errors
    /// A human-readable message if the call cannot be sent or the extension refuses it.
    fn invoke_contribution(&self, extension: &str, name: &str) -> Result<String, String> {
        let _ = (extension, name);
        Err("contributions are not available over this connection".to_owned())
    }

    /// How to describe this connection in a status line.
    fn describe(&self) -> String;

    /// Whether the transport is alive, checked between turns.
    fn alive(&self) -> bool {
        true
    }

    /// Whether the gateway is a child that dies with this client (stdio), not an `--addr` one.
    fn is_stdio(&self) -> bool {
        false
    }
}

/// REST + SSE gateway surface.
pub struct Rest {
    addr: String,
    /// `cancel` takes and shuts it down; an empty slot tells the reader that a cancel, not a failure, closed it.
    live: Arc<Mutex<Option<TcpStream>>>,
}

impl Rest {
    /// Talk to the gateway at `addr` (`host:port`).
    #[must_use]
    pub fn new(addr: String) -> Self {
        Self {
            addr,
            live: Arc::new(Mutex::new(None)),
        }
    }
}

impl Transport for Rest {
    fn create_session(&self) -> Result<String, String> {
        crate::create_session(&self.addr)
    }

    fn session_list(&self) -> Result<Vec<SessionSummary>, String> {
        crate::list_sessions(&self.addr)
    }

    fn session_get(&self, session: &str) -> Result<SessionGetResult, String> {
        crate::get_session(&self.addr, session)
    }

    fn cancel(&self, _session: &str) -> Result<(), String> {
        let taken = self
            .live
            .lock()
            .map_err(|_| "the live turn's socket is poisoned".to_owned())?
            .take();
        taken.map_or_else(
            || Err("no turn is running to cancel".to_owned()),
            |stream| {
                stream
                    .shutdown(std::net::Shutdown::Both)
                    .map_err(|err| format!("shutting down the turn's connection: {err}"))
            },
        )
    }

    fn stream_turn(
        &self,
        session: &str,
        message: &str,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<(), String> {
        crate::stream_turn_cancellable(&self.addr, session, message, on_event, &self.live)
    }

    fn answer(&self, session: &str, answer: &str) -> Result<(), String> {
        crate::answer_prompt(&self.addr, session, answer)
    }

    fn follow_up(&self, _session: &str, _message: &str) -> Result<(), String> {
        Err(
            "this connection is REST, which cannot steer a turn in flight — \
             start the gateway over stdio to follow up, or wait for this turn \
             to finish"
                .to_owned(),
        )
    }

    fn describe(&self) -> String {
        format!("{} over REST", self.addr)
    }
}

/// Gateway log output handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logs {
    /// Inherit stderr: fine for the line REPL.
    Inherit,
    /// Discard stderr: the TUI owns the terminal, and logs would draw over it.
    Discard,
}

/// Spawned gateway, driven over stdin/stdout.
pub struct Stdio {
    child: Mutex<Child>,
    writer: Mutex<ChildStdin>,
    reader: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicI64,
    described: String,
    /// Set at the handshake, before any reader exists; the notification arrives once, ahead of the hello answer.
    contributed: Mutex<Vec<jk_protocol::Contributions>>,
}

impl Stdio {
    /// Spawn gateway and negotiate protocol version.
    ///
    /// # Errors
    /// If gateway can't start or speaks incompatible protocol.
    pub fn spawn(logs: Logs) -> Result<Self, String> {
        Self::spawn_from(&crate::gateway_bin(), &[], logs)
    }

    /// Spawn the gateway at `bin` with `args` after `rpc`.
    ///
    /// # Errors
    /// As `spawn`.
    pub fn spawn_from(bin: &Path, args: &[&str], logs: Logs) -> Result<Self, String> {
        let mut child = Command::new(bin)
            .arg("rpc")
            .args(args)
            .stdin(ChildIo::piped())
            .stdout(ChildIo::piped())
            .stderr(match logs {
                Logs::Inherit => ChildIo::inherit(),
                Logs::Discard => ChildIo::null(),
            })
            .spawn()
            .map_err(|err| {
                format!(
                    "could not start {} ({err}). Install it, or pass --addr <host:port> to \
                     use a gateway that is already running.",
                    bin.display()
                )
            })?;
        let writer = child.stdin.take().ok_or("the gateway has no stdin")?;
        let reader = BufReader::new(child.stdout.take().ok_or("the gateway has no stdout")?);
        let transport = Self {
            child: Mutex::new(child),
            writer: Mutex::new(writer),
            reader: Mutex::new(reader),
            next_id: AtomicI64::new(1),
            described: format!("{} over stdio", bin.display()),
            contributed: Mutex::new(Vec::new()),
        };
        match transport.handshake() {
            Ok(()) => Ok(transport),
            Err(err) => Err(transport.explain(err, logs)),
        }
    }

    /// If the gateway exited, its stderr says why, which a bare "connection closed" does not.
    fn explain(&self, err: String, logs: Logs) -> String {
        let exited = self
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten());
        exited.map_or(err, |status| {
            let where_to_look = match logs {
                Logs::Inherit => "Its own explanation is above.",
                Logs::Discard => {
                    "Run the line REPL (`jan-klod repl`) to see why — the \
                     full-screen UI hides the gateway's stderr because it would be drawn over."
                }
            };
            format!("the gateway exited before answering ({status}). {where_to_look}")
        })
    }

    fn handshake(&self) -> Result<(), String> {
        let id = self.send(&Rpc::Hello {
            version: PROTOCOL_VERSION.to_owned(),
        })?;
        let result = self.read_until(&id, &mut |event| {
            if let StreamEvent::Contributions(sets) = event {
                if let Ok(mut held) = self.contributed.lock() {
                    *held = sets;
                }
            }
        })?;
        let core = result
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or("the gateway's handshake carried no version")?;
        if compatible(core, PROTOCOL_VERSION) {
            Ok(())
        } else {
            Err(format!(
                "this client speaks protocol {PROTOCOL_VERSION} and the gateway speaks {core}. \
                 Install a matching jan-klod-gateway."
            ))
        }
    }

    fn send(&self, command: &Rpc) -> Result<jsonrpc::Id, String> {
        let id = jsonrpc::Id::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let frame = jsonrpc::Request::new(id.clone(), command.clone());
        let line = serde_json::to_string(&frame).map_err(|err| err.to_string())?;
        {
            let mut writer = self.writer.lock().map_err(|_| "the writer is poisoned")?;
            writeln!(writer, "{line}").map_err(|err| format!("writing to the gateway: {err}"))?;
            writer
                .flush()
                .map_err(|err| format!("writing to the gateway: {err}"))?;
        }
        Ok(id)
    }

    #[allow(clippy::significant_drop_tightening)]
    fn read_until(
        &self,
        id: &jsonrpc::Id,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<serde_json::Value, String> {
        let mut reader = self.reader.lock().map_err(|_| "the reader is poisoned")?;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return Err("the gateway closed the connection".to_owned()),
                Ok(_) => {}
                Err(err) => return Err(format!("reading from the gateway: {err}")),
            }
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            if let Ok(response) = serde_json::from_str::<jsonrpc::Response>(text) {
                match response.outcome {
                    jsonrpc::Outcome::Result(value) if &response.id == id => return Ok(value),
                    jsonrpc::Outcome::Error(error) if &response.id == id => {
                        return Err(error.message)
                    }
                    jsonrpc::Outcome::Error(error) => on_event(StreamEvent::Error(error.message)),
                    jsonrpc::Outcome::Result(_) => {}
                }
                continue;
            }
            match serde_json::from_str::<jsonrpc::Notification>(text) {
                Ok(framed) => {
                    if let Some(event) = event_for(&framed.notification) {
                        on_event(event);
                    }
                }
                Err(err) => on_event(StreamEvent::Error(format!(
                    "the gateway sent something this client cannot read ({err}): {text}"
                ))),
            }
        }
    }
}

impl Stdio {
    /// Only safe when no turn streams: `stream_turn` holds the reader, so a racing call could steal its frame.
    fn create_session_over_stdio(&self) -> Result<String, String> {
        let id = self.send(&Rpc::SessionCreate)?;
        let result = self.read_until(&id, &mut |_| {})?;
        result
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "the gateway's session/create answer carried no id".to_owned())
    }

    /// Only safe when no turn streams (see `session_get`).
    fn session_list_over_stdio(&self) -> Result<Vec<SessionSummary>, String> {
        let id = self.send(&Rpc::SessionList)?;
        let result = self.read_until(&id, &mut |_| {})?;
        let sessions = result
            .get("sessions")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "the gateway's session/list answer carried no `sessions`".to_owned())?;
        sessions
            .iter()
            .map(|entry| {
                let id = entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "a session/list entry carried no id".to_owned())?
                    .to_owned();
                let preview = entry
                    .get("preview")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                Ok(SessionSummary { id, preview })
            })
            .collect()
    }

    /// Only safe when no turn streams.
    fn session_get_over_stdio(&self, session: &str) -> Result<SessionGetResult, String> {
        let id = self.send(&Rpc::SessionGet {
            session: session.to_owned(),
        })?;
        let result = self.read_until(&id, &mut |_| {})?;
        serde_json::from_value(result).map_err(|err| format!("malformed session/get answer: {err}"))
    }
}

impl Transport for Stdio {
    fn create_session(&self) -> Result<String, String> {
        self.create_session_over_stdio()
    }

    fn session_list(&self) -> Result<Vec<SessionSummary>, String> {
        self.session_list_over_stdio()
    }

    fn session_get(&self, session: &str) -> Result<SessionGetResult, String> {
        self.session_get_over_stdio(session)
    }

    fn cancel(&self, session: &str) -> Result<(), String> {
        self.send(&Rpc::TurnCancel {
            session: session.to_owned(),
        })?;
        Ok(())
    }

    fn stream_turn(
        &self,
        session: &str,
        message: &str,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<(), String> {
        let id = self.send(&Rpc::SessionMessage {
            session: session.to_owned(),
            message: message.to_owned(),
        })?;
        self.read_until(&id, on_event)?;
        Ok(())
    }

    fn answer(&self, session: &str, answer: &str) -> Result<(), String> {
        self.send(&Rpc::TurnAnswer {
            session: session.to_owned(),
            answer: answer.to_owned(),
        })?;
        Ok(())
    }

    fn follow_up(&self, session: &str, message: &str) -> Result<(), String> {
        self.send(&Rpc::TurnFollowUp {
            session: session.to_owned(),
            message: message.to_owned(),
        })?;
        Ok(())
    }

    fn contributions(&self) -> Vec<jk_protocol::Contributions> {
        self.contributed
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    /// Only safe when no turn streams, like `session_get`: it reads its answer off the pipe a turn's reader holds.
    fn invoke_contribution(&self, extension: &str, name: &str) -> Result<String, String> {
        let id = self.send(&Rpc::SurfaceInvoke {
            extension: extension.to_owned(),
            name: name.to_owned(),
            arguments: Vec::new(),
        })?;
        let result = self.read_until(&id, &mut |event| {
            if let StreamEvent::Contributions(sets) = event {
                if let Ok(mut held) = self.contributed.lock() {
                    *held = sets;
                }
            }
        })?;
        Ok(result
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    fn describe(&self) -> String {
        self.described.clone()
    }

    /// `try_wait`, not `wait`: polled between turns; blocking would hang the event loop on a healthy child.
    fn alive(&self) -> bool {
        self.child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok())
            .is_none_or(|status| status.is_none())
    }

    fn is_stdio(&self) -> bool {
        true
    }
}

impl Drop for Stdio {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Notification → `StreamEvent`, or `None` when unmapped.
#[must_use]
pub fn event_for(notification: &Notification) -> Option<StreamEvent> {
    match notification {
        Notification::TextDelta { text } => Some(StreamEvent::Delta(text.clone())),
        Notification::ToolInvoked {
            id,
            name,
            arguments,
        } => Some(StreamEvent::Tool {
            id: id.clone(),
            name: name.clone(),
            arguments: Some(arguments.clone()),
        }),
        Notification::Warning { message } => Some(StreamEvent::Warning(message.clone())),
        Notification::Done { answer, .. } => Some(StreamEvent::Done(answer.clone())),
        Notification::Ask {
            session,
            question,
            options,
            default,
        } => Some(StreamEvent::Prompt {
            session: session.clone(),
            question: question.clone(),
            options: options.clone(),
            default: default.clone(),
        }),
        Notification::Error { message } => Some(StreamEvent::Error(message.clone())),
        Notification::ToolResult {
            id,
            content,
            failed,
        } => Some(StreamEvent::ToolResult {
            id: id.clone(),
            content: content.clone(),
            failed: *failed,
        }),
        Notification::SurfaceContributions { extensions } => {
            Some(StreamEvent::Contributions(extensions.clone()))
        }
        Notification::SessionUpdated { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{event_for, Logs, Rest, Rpc, Stdio, Transport};
    use crate::StreamEvent;
    use jk_protocol::{Notification, PROTOCOL_VERSION};

    #[test]
    fn a_contributions_notification_becomes_an_event() {
        let event = event_for(&Notification::SurfaceContributions {
            extensions: vec![jk_protocol::Contributions {
                extension: "interceptor.system".to_owned(),
                commands: vec![],
                status_items: vec![],
                forms: vec![],
            }],
        });
        let Some(StreamEvent::Contributions(sets)) = event else {
            panic!("a contributions notification must reach the client: {event:?}");
        };
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].extension, "interceptor.system");
    }

    #[test]
    fn the_stdio_mapping_carries_the_call_id_and_its_arguments() {
        let event = event_for(&Notification::ToolInvoked {
            id: "c7".to_owned(),
            name: "fs.read".to_owned(),
            arguments: r#"{"path":"a.txt"}"#.to_owned(),
        });
        assert_eq!(
            event,
            Some(StreamEvent::Tool {
                id: "c7".to_owned(),
                name: "fs.read".to_owned(),
                arguments: Some(r#"{"path":"a.txt"}"#.to_owned()),
            }),
            "the id is what pairs an invocation with its result"
        );
    }

    #[test]
    fn every_notification_is_decided_about() {
        let cases = [
            Notification::TextDelta {
                text: "hi".to_owned(),
            },
            Notification::ToolInvoked {
                id: "c1".to_owned(),
                name: "fs.read".to_owned(),
                arguments: "{}".to_owned(),
            },
            Notification::ToolResult {
                id: "c1".to_owned(),
                content: "ok".to_owned(),
                failed: false,
            },
            Notification::Warning {
                message: "w".to_owned(),
            },
            Notification::Done {
                answer: "42".to_owned(),
                agentic: true,
            },
            Notification::Ask {
                session: "s".to_owned(),
                question: "?".to_owned(),
                options: vec!["yes".to_owned()],
                default: "no".to_owned(),
            },
            Notification::Error {
                message: "e".to_owned(),
            },
            Notification::SessionUpdated {
                session: "s".to_owned(),
                preview: "hi".to_owned(),
            },
        ];
        let dropped: Vec<usize> = cases
            .iter()
            .enumerate()
            .filter(|(_, n)| event_for(n).is_none())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            dropped,
            vec![7],
            "`session/updated` is the only notification this client drops"
        );
    }

    #[test]
    fn an_ask_keeps_its_options_and_its_default() {
        let event = event_for(&Notification::Ask {
            session: "s".to_owned(),
            question: "Run it?".to_owned(),
            options: vec!["yes".to_owned(), "no".to_owned()],
            default: "no".to_owned(),
        })
        .expect("an ask is shown");
        assert_eq!(
            event,
            StreamEvent::Prompt {
                session: "s".to_owned(),
                question: "Run it?".to_owned(),
                options: vec!["yes".to_owned(), "no".to_owned()],
                default: "no".to_owned(),
            }
        );
    }

    #[test]
    fn an_ask_carries_the_session_it_was_asked_on() {
        let event = event_for(&Notification::Ask {
            session: "the-turns-session".to_owned(),
            question: "Run it?".to_owned(),
            options: vec!["yes".to_owned(), "no".to_owned()],
            default: "no".to_owned(),
        })
        .expect("an ask is shown");
        let StreamEvent::Prompt { session, .. } = event else {
            panic!("expected a Prompt event, got {event:?}")
        };
        assert_eq!(session, "the-turns-session");
    }

    #[test]
    fn a_missing_gateway_says_what_to_do_instead() {
        let outcome = Stdio::spawn_from(
            std::path::Path::new("/nonexistent/jan-klod-gateway"),
            &[],
            Logs::Discard,
        );
        let Err(err) = outcome else {
            panic!("there is no gateway at that path")
        };
        assert!(err.contains("--addr"), "names the alternative: {err}");
    }

    #[test]
    fn rest_refuses_to_steer_and_says_why_rather_than_how() {
        let rest = Rest::new("127.0.0.1:9".to_owned());
        let refusal = rest
            .follow_up("s1", "actually, use the other file")
            .expect_err("REST cannot steer a turn in flight");
        assert!(
            refusal.contains("REST"),
            "the reason does not name the transport: {refusal:?}"
        );
        assert!(
            refusal.contains("stdio") || refusal.contains("finish"),
            "a refusal a user can read says what to do instead: {refusal:?}"
        );
    }

    #[test]
    fn steering_frames_as_the_method_the_core_matches_on() {
        let framed = serde_json::to_value(jk_protocol::Command::TurnFollowUp {
            session: "s1".to_owned(),
            message: "steer".to_owned(),
        })
        .expect("a command serializes");
        assert_eq!(
            framed.get("method").and_then(serde_json::Value::as_str),
            Some("turn/follow-up"),
            "`rpc.rs` matches on this string, and a turn that is running queues \
             the message onto `state.follow_ups` when it sees it"
        );
        assert_eq!(
            framed
                .get("params")
                .and_then(|p| p.get("message"))
                .and_then(serde_json::Value::as_str),
            Some("steer")
        );
    }

    #[cfg(unix)]
    #[test]
    fn steering_over_stdio_writes_a_follow_up_frame() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let sent = dir.join(format!("jk-steer-{}.jsonl", std::process::id()));
        let bin = dir.join(format!("jk-gateway-{}.sh", std::process::id()));
        let _ = std::fs::remove_file(&sent);

        let script = format!(
            r#"#!/bin/sh
IFS= read -r hello
id=$(printf '%s' "$hello" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"version":"{version}"}}}}\n' "$id"
while IFS= read -r line; do printf '%s\n' "$line" >> "{path}"; done
"#,
            version = PROTOCOL_VERSION,
            path = sent.display()
        );
        std::fs::write(&bin, script).expect("write the fake gateway");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let Ok(gateway) = Stdio::spawn_from(&bin, &[], Logs::Discard) else {
            panic!("the fake gateway did not start or did not shake hands")
        };
        gateway
            .follow_up("s1", "use the other file")
            .expect("steering is written, not waited for");

        let mut frame = String::new();
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&sent) {
                if !text.trim().is_empty() {
                    frame = text;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(gateway);
        let _ = std::fs::remove_file(&sent);
        let _ = std::fs::remove_file(&bin);

        assert!(
            frame.contains(r#""method":"turn/follow-up""#),
            "the client sent something else: {frame:?}"
        );
        assert!(
            frame.contains("use the other file"),
            "the steering message did not go with it: {frame:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancel_over_stdio_writes_a_turn_cancel_frame() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let sent = dir.join(format!("jk-cancel-{}.jsonl", std::process::id()));
        let bin = dir.join(format!("jk-gateway-cancel-{}.sh", std::process::id()));
        let _ = std::fs::remove_file(&sent);

        let script = format!(
            r#"#!/bin/sh
IFS= read -r hello
id=$(printf '%s' "$hello" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"version":"{version}"}}}}\n' "$id"
while IFS= read -r line; do printf '%s\n' "$line" >> "{path}"; done
"#,
            version = PROTOCOL_VERSION,
            path = sent.display()
        );
        std::fs::write(&bin, script).expect("write the fake gateway");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let Ok(gateway) = Stdio::spawn_from(&bin, &[], Logs::Discard) else {
            panic!("the fake gateway did not start or did not shake hands")
        };
        gateway
            .cancel("s1")
            .expect("cancel is written, not waited for");

        let mut frame = String::new();
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&sent) {
                if !text.trim().is_empty() {
                    frame = text;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(gateway);
        let _ = std::fs::remove_file(&sent);
        let _ = std::fs::remove_file(&bin);

        assert!(
            frame.contains(r#""method":"turn/cancel""#),
            "the client sent something else: {frame:?}"
        );
        assert!(
            frame.contains(r#""session":"s1""#),
            "the session did not go with it: {frame:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn alive_reports_false_once_the_child_has_exited() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let bin = dir.join(format!("jk-gateway-exit-{}.sh", std::process::id()));

        let script = format!(
            r#"#!/bin/sh
IFS= read -r hello
id=$(printf '%s' "$hello" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"version":"{PROTOCOL_VERSION}"}}}}\n' "$id"
IFS= read -r goodbye
"#
        );
        std::fs::write(&bin, script).expect("write the fake gateway");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let Ok(gateway) = Stdio::spawn_from(&bin, &[], Logs::Discard) else {
            panic!("the fake gateway did not start or did not shake hands")
        };
        assert!(gateway.alive(), "the child is still blocked on stdin");

        gateway
            .send(&Rpc::SessionList)
            .expect("the frame that lets the fake gateway finish");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut seen_dead = false;
        while std::time::Instant::now() < deadline {
            if !gateway.alive() {
                seen_dead = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&bin);
        assert!(seen_dead, "alive() never reported the child had exited");
    }

    #[cfg(unix)]
    #[test]
    fn create_session_over_stdio_reads_the_new_id() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let bin = dir.join(format!("jk-gateway-create-{}.sh", std::process::id()));

        let script = format!(
            r#"#!/bin/sh
IFS= read -r hello
id=$(printf '%s' "$hello" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"version":"{PROTOCOL_VERSION}"}}}}\n' "$id"
IFS= read -r req
id2=$(printf '%s' "$req" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"id":"sess-42"}}}}\n' "$id2"
while IFS= read -r line; do :; done
"#
        );
        std::fs::write(&bin, script).expect("write the fake gateway");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let Ok(gateway) = Stdio::spawn_from(&bin, &[], Logs::Discard) else {
            panic!("the fake gateway did not start or did not shake hands")
        };
        let id = gateway
            .create_session()
            .expect("the gateway answers session/create");
        let _ = std::fs::remove_file(&bin);

        assert_eq!(id, "sess-42");
    }

    #[test]
    fn create_session_over_rest_posts_and_reads_the_id() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("binds ephemeral port");
        let port = server.server_addr().to_ip().expect("ip addr").port();
        let addr = format!("127.0.0.1:{port}");

        let server_thread = std::thread::spawn(move || {
            let request = server.recv().expect("receives request");
            assert_eq!(request.url(), "/sessions");
            assert_eq!(*request.method(), tiny_http::Method::Post);
            let reply = tiny_http::Response::from_string(r#"{"id":"sess-7"}"#);
            request.respond(reply).unwrap();
        });

        let rest = Rest::new(addr);
        let id = rest.create_session().expect("the server answers");
        assert_eq!(id, "sess-7");

        server_thread.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn session_list_and_session_get_over_stdio_read_their_own_answers() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let bin = dir.join(format!("jk-gateway-sessions-{}.sh", std::process::id()));

        let script = format!(
            r#"#!/bin/sh
IFS= read -r hello
id=$(printf '%s' "$hello" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"version":"{PROTOCOL_VERSION}"}}}}\n' "$id"
IFS= read -r req1
id1=$(printf '%s' "$req1" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessions":[{{"id":"s1","preview":"hi"}}]}}}}\n' "$id1"
IFS= read -r req2
id2=$(printf '%s' "$req2" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"id":"s1","messages":[{{"seq":1,"role":"user","content":"hi"}}]}}}}\n' "$id2"
while IFS= read -r line; do :; done
"#
        );
        std::fs::write(&bin, script).expect("write the fake gateway");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let Ok(gateway) = Stdio::spawn_from(&bin, &[], Logs::Discard) else {
            panic!("the fake gateway did not start or did not shake hands")
        };
        assert!(gateway.is_stdio());

        let sessions = gateway
            .session_list()
            .expect("the gateway answers session/list");
        assert_eq!(
            sessions,
            vec![super::SessionSummary {
                id: "s1".to_owned(),
                preview: "hi".to_owned(),
            }]
        );

        let result = gateway
            .session_get("s1")
            .expect("the gateway answers session/get");
        let _ = std::fs::remove_file(&bin);

        assert_eq!(result.id, "s1");
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[0].content, "hi");
    }

    #[test]
    fn session_list_and_session_get_over_rest_get_and_parse() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("binds ephemeral port");
        let port = server.server_addr().to_ip().expect("ip addr").port();
        let addr = format!("127.0.0.1:{port}");

        let server_thread = std::thread::spawn(move || {
            let request = server.recv().expect("receives request");
            assert_eq!(request.url(), "/sessions");
            assert_eq!(*request.method(), tiny_http::Method::Get);
            request
                .respond(tiny_http::Response::from_string(
                    r#"{"sessions":[{"id":"s1","preview":"hi"}]}"#,
                ))
                .unwrap();

            let request = server.recv().expect("receives a second request");
            assert_eq!(request.url(), "/session/s1");
            assert_eq!(*request.method(), tiny_http::Method::Get);
            request
                .respond(tiny_http::Response::from_string(
                    r#"{"id":"s1","messages":[{"seq":1,"role":"user","content":"hi"}]}"#,
                ))
                .unwrap();
        });

        let rest = Rest::new(addr);
        assert!(!rest.is_stdio());
        let sessions = rest.session_list().expect("the server answers");
        assert_eq!(
            sessions,
            vec![super::SessionSummary {
                id: "s1".to_owned(),
                preview: "hi".to_owned(),
            }]
        );

        let result = rest.session_get("s1").expect("the server answers");
        assert_eq!(result.id, "s1");
        assert_eq!(result.messages[0].content, "hi");

        server_thread.join().unwrap();
    }

    #[test]
    fn rest_cancel_with_nothing_running_is_a_clear_error() {
        let rest = Rest::new("127.0.0.1:9".to_owned());
        let err = rest
            .cancel("s1")
            .expect_err("nothing is streaming on a fresh transport");
        assert!(
            err.contains("no turn is running"),
            "a refusal a user can read says what happened: {err:?}"
        );
    }
}
