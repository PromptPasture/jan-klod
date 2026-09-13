//! How the client reaches the core: over a pipe to a gateway it spawned, or
//! over the REST surface of one that is already running.
//!
//! # Why this is a trait and not two functions
//!
//! The REST client's connection handle was the address, a `String`, cloned into
//! a fresh thread for every turn and every answer. A pipe to a child process
//! cannot be cloned that way: there is one stdin, one stdout, and two threads
//! that want them — the turn streaming its events, and the keypress answering a
//! confirmation. So the handle became one shared object, and the two paths
//! became implementations of it. `app.rs` never knew which transport was
//! underneath and still does not; `tui.rs` knew, only because it held the
//! address.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio as ChildIo};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_protocol::{
    compatible, jsonrpc, Command as Rpc, Notification, SessionGetResult, PROTOCOL_VERSION,
};

use crate::StreamEvent;

/// One session `session/list` reports: an id and a preview of its first user
/// message (#105).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session id.
    pub id: String,
    /// First 80 characters of the session's first user message.
    pub preview: String,
}

/// One way of driving a core.
///
/// `Send + Sync` because the TUI hands it to a turn thread and an answer thread
/// at the same time, which is the whole reason it exists.
pub trait Transport: Send + Sync {
    /// Allocate a new session and return its id.
    ///
    /// # Errors
    /// A human-readable message if the connection fails or the core refuses.
    fn create_session(&self) -> Result<String, String>;

    /// Every session with a preview (#105). Populated fresh at every open of
    /// the switcher, never cached — the model of what exists is the core's,
    /// not this client's.
    ///
    /// # Errors
    /// A human-readable message if the connection fails or the core refuses.
    fn session_list(&self) -> Result<Vec<SessionSummary>, String>;

    /// One session's transcript, to rebuild this client's view of it after a
    /// switch (#105). **Only safe to call when no turn is streaming** — like
    /// [`Transport::create_session`]'s stdio implementation, this reads its
    /// own answer off the same pipe a running turn's reader holds, and a call
    /// made while one is in flight would race it for the next line. The
    /// session switcher enforces this by refusing to switch while a turn is
    /// running (`App::sessions_confirm`), which is what makes it safe to call
    /// here at all.
    ///
    /// # Errors
    /// A human-readable message if the connection fails or the session does
    /// not exist.
    fn session_get(&self, session: &str) -> Result<SessionGetResult, String>;

    /// Stop the turn running on `session`, if any.
    ///
    /// One method, two honest implementations: over stdio this sends
    /// `turn/cancel`; over REST it drops the SSE connection, which is the only
    /// thing that stops a turn on that surface (see the `Rest` impl). The menu
    /// above this trait does not know which, and must not need to.
    ///
    /// # Errors
    /// A human-readable message if the cancellation cannot be sent or acted on
    /// — see each implementation for what that means on its transport.
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

    /// Answer a [`StreamEvent::Prompt`] that is holding a turn open. Called
    /// while [`Transport::stream_turn`] is still running, from another thread.
    ///
    /// # Errors
    /// A human-readable message if the answer cannot be sent.
    fn answer(&self, session: &str, answer: &str) -> Result<(), String>;

    /// Steer the turn that is running, without cancelling it (#159). Called
    /// while [`Transport::stream_turn`] is still running, from another thread —
    /// the same shape as [`Transport::answer`], and for the same reason.
    ///
    /// # Errors
    /// A human-readable message if the steering cannot be sent. **Over REST
    /// that is always**, and the message says why: the error is a value the
    /// caller shows a user, not a log line, because a keystroke that vanishes
    /// teaches somebody that steering does not work rather than that this
    /// connection cannot carry it.
    fn follow_up(&self, session: &str, message: &str) -> Result<(), String>;

    /// How to describe this connection in a status line.
    fn describe(&self) -> String;

    /// Whether the transport still looks alive, checked between turns (#104).
    ///
    /// The default answers `true`: most of the ways a transport dies already
    /// surface as an `Err` from [`Transport::stream_turn`], which the caller
    /// reads as a disconnect on its own. [`Stdio`] overrides this because it
    /// alone holds a child process that can exit with nothing in flight to
    /// fail — the gateway's own `Drop`/`Logs` already know about that exit;
    /// this is what lets the client's event loop notice it before the next
    /// turn tries to use a pipe with nothing on the other end.
    fn alive(&self) -> bool {
        true
    }

    /// Whether this transport is the spawned-gateway kind (#105). Over stdio
    /// the gateway is a child of this process and dies with it, so a turn
    /// still running when the client quits is genuinely lost; over `--addr`
    /// it is not, because the gateway outlives this client. The quit confirm
    /// worded that fact from wherever it is actually known, which is here —
    /// the default answers `false`, since [`Rest`] is the common "it does
    /// not" case and [`Stdio`] is the one that overrides it.
    fn is_stdio(&self) -> bool {
        false
    }
}

/// The REST + SSE surface of a gateway that is already listening.
pub struct Rest {
    addr: String,
    /// The socket of the turn currently streaming, if one is. `stream_turn`
    /// stores a clone here for the length of the turn and clears it when the
    /// turn ends, however it ends; `cancel` shuts it down, which is what makes
    /// the reader's line loop see EOF and return (`rpc.rs` reads a closed
    /// connection as `Flow::Stop` — this is that, from the client side).
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
        // No turn running is a clear error rather than a silent no-op: a
        // `/cancel` that "worked" against nothing would teach a user the
        // opposite of what happened.
        let guard = self
            .live
            .lock()
            .map_err(|_| "the live turn's socket is poisoned".to_owned())?;
        guard.as_ref().map_or_else(
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
        // Not "not implemented yet". The REST surface has no route that reaches
        // a turn already in flight — 13b recorded that when it added the two
        // mid-turn commands to the protocol, noting that over REST the only way
        // to stop a turn is to drop the SSE connection and there is no way at
        // all to steer one. So this is a property of the transport, and the
        // client says which rather than dropping the keystroke.
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

/// What to do with the gateway's own log output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logs {
    /// Let it through to this process's stderr. Right for a line-mode REPL,
    /// where an interleaved log line is readable and often what you wanted.
    Inherit,
    /// Throw it away. Right for the full-screen TUI, which owns the terminal —
    /// a log line written into it lands on top of the rendering. Losing the
    /// diagnostics is the cost; a log *file* would be better than either, and
    /// is not this slice's business.
    Discard,
}

/// A gateway this client spawned, driven over its stdin and stdout.
///
/// One child, one pipe each way. `stream_turn` holds the reader for the length
/// of a turn; `answer` only needs the writer, which it takes briefly. Writing
/// and reading are separate locks for exactly that reason — sharing one would
/// deadlock the moment a confirmation arrived.
pub struct Stdio {
    child: Mutex<Child>,
    writer: Mutex<ChildStdin>,
    reader: Mutex<BufReader<ChildStdout>>,
    /// Request ids are ours to choose; a counter is enough, and it has to be
    /// atomic because two threads mint them.
    next_id: AtomicI64,
    described: String,
}

impl Stdio {
    /// Spawn `jan-klod-gateway rpc` beside this binary (or on `PATH`) and
    /// negotiate the protocol version.
    ///
    /// # Errors
    /// A human-readable message if the gateway cannot be started, or speaks a
    /// protocol this client cannot talk to.
    pub fn spawn(logs: Logs) -> Result<Self, String> {
        Self::spawn_from(&crate::gateway_bin(), &[], logs)
    }

    /// Spawn a named gateway binary, passing `args` after `rpc`.
    ///
    /// Separate from [`Stdio::spawn`] so a test can point the client at the
    /// binary it just built and at a config it wrote, rather than at whatever
    /// happens to be installed.
    ///
    /// # Errors
    /// As [`Stdio::spawn`].
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
        // Taken out of the child, so the struct owns each end exactly once.
        let writer = child.stdin.take().ok_or("the gateway has no stdin")?;
        let reader = BufReader::new(child.stdout.take().ok_or("the gateway has no stdout")?);
        let transport = Self {
            child: Mutex::new(child),
            writer: Mutex::new(writer),
            reader: Mutex::new(reader),
            next_id: AtomicI64::new(1),
            described: format!("{} over stdio", bin.display()),
        };
        match transport.handshake() {
            Ok(()) => Ok(transport),
            Err(err) => Err(transport.explain(err, logs)),
        }
    }

    /// Turn a failed handshake into something a first-time user can act on.
    ///
    /// The common failure is not a protocol mismatch, it is a gateway that
    /// refused to boot — no API key, a config it could not read — and said so
    /// on its own stderr before exiting. "The gateway closed the connection" is
    /// true and useless next to that, so if the child is already gone, this
    /// says so and points at where the reason is.
    fn explain(&self, err: String, logs: Logs) -> String {
        // `try_wait`, not `wait`: a version mismatch leaves the gateway running
        // and waiting for the next frame, and blocking on it would hang.
        let exited = self
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten());
        exited.map_or(err, |status| {
            let where_to_look = match logs {
                Logs::Inherit => "Its own explanation is above.",
                Logs::Discard => {
                    "Run the line REPL (`jan-klod`, without `tui`) to see why — the \
                     full-screen UI hides the gateway's stderr because it would be drawn over."
                }
            };
            format!("the gateway exited before answering ({status}). {where_to_look}")
        })
    }

    /// Agree a protocol version before anything else is sent.
    ///
    /// The core requires this and closes the connection on a mismatch, so a
    /// client that skipped it would fail later and less clearly.
    fn handshake(&self) -> Result<(), String> {
        let id = self.send(&Rpc::Hello {
            version: PROTOCOL_VERSION.to_owned(),
        })?;
        let result = self.read_until(&id, &mut |_| {})?;
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

    /// Frame `command` and write it. Returns the id to expect an answer against.
    fn send(&self, command: &Rpc) -> Result<jsonrpc::Id, String> {
        let id = jsonrpc::Id::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let frame = jsonrpc::Request::new(id.clone(), command.clone());
        let line = serde_json::to_string(&frame).map_err(|err| err.to_string())?;
        {
            // Scoped: the writer is released before this returns, so an answer
            // from another thread never waits on a turn that is only reading.
            let mut writer = self.writer.lock().map_err(|_| "the writer is poisoned")?;
            writeln!(writer, "{line}").map_err(|err| format!("writing to the gateway: {err}"))?;
            writer
                .flush()
                .map_err(|err| format!("writing to the gateway: {err}"))?;
        }
        Ok(id)
    }

    /// Read frames until the answer to `id`, handing notifications to
    /// `on_event` on the way.
    // The reader is held for the whole exchange on purpose: one turn owns the
    // stream until its answer arrives, which is what keeps two threads from
    // interleaving reads of the same pipe. Releasing it sooner, as clippy
    // suggests, is precisely the bug.
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
                    // Someone else's answer: an `answer` sent from the other
                    // thread, which does not wait for its own reply. A refusal
                    // there matters — it means the answer was not taken — so it
                    // is surfaced rather than dropped.
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
    /// Send `session/create` and read its own answer.
    ///
    /// Safe only when called while no turn is streaming: `stream_turn` holds
    /// the reader for the whole exchange, so a call made while one is running
    /// would race it for the next line off the pipe and could steal a frame
    /// that belonged to the turn thread. The menu calls this to start `/new`,
    /// which is exactly a moment nothing else is reading.
    ///
    /// # Errors
    /// A human-readable message if the request cannot be sent, the response
    /// cannot be read, or it carries no `id`.
    fn create_session_over_stdio(&self) -> Result<String, String> {
        let id = self.send(&Rpc::SessionCreate)?;
        let result = self.read_until(&id, &mut |_| {})?;
        result
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "the gateway's session/create answer carried no id".to_owned())
    }

    /// Send `session/list` and read its own answer. Safe only when called
    /// while no turn is streaming — see [`Transport::session_get`]'s docs,
    /// which this shares the caveat with.
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

    /// Send `session/get` and read its own answer. Safe only when called
    /// while no turn is streaming, for the reason [`Transport::session_get`]
    /// documents.
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
        // Written and not waited for, exactly like `answer` and `follow_up`:
        // the turn thread owns the reader, so this one cannot read its own
        // acknowledgement. A refusal ("no turn is running") comes back as an
        // error response with this id, which `read_until` surfaces as an
        // event.
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
        // Written and not waited for: the turn thread owns the reader, so this
        // one cannot read its own acknowledgement. A refusal comes back as an
        // error response with this id, which `read_until` surfaces as an
        // event — the turn thread is the one that can display it anyway.
        self.send(&Rpc::TurnAnswer {
            session: session.to_owned(),
            answer: answer.to_owned(),
        })?;
        Ok(())
    }

    fn follow_up(&self, session: &str, message: &str) -> Result<(), String> {
        // Written and not waited for, exactly like `answer`: the turn thread
        // owns the reader, so this one cannot read its own acknowledgement. The
        // core queues the message on `state.follow_ups` and answers
        // `{"queued": true}`; a refusal comes back as an error response with
        // this id, which `read_until` surfaces as an event.
        self.send(&Rpc::TurnFollowUp {
            session: session.to_owned(),
            message: message.to_owned(),
        })?;
        Ok(())
    }

    fn describe(&self) -> String {
        self.described.clone()
    }

    /// `try_wait`, not `wait`: this is polled between turns while the gateway
    /// is expected to still be running, and blocking on it would hang the
    /// event loop against a healthy child.
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
    /// The client owns the process, so it ends with the client.
    ///
    /// Killed rather than politely hung up on: closing stdin would let the
    /// gateway finish on its own, but a turn still in flight would hold the
    /// wait open — and a client that is exiting should not block on a model.
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The [`StreamEvent`] a notification becomes, or `None` when this client has
/// nowhere to put it.
///
/// Exhaustive — no `_ =>` arm — so a notification added to the protocol has to
/// be decided about here rather than silently ignored.
#[must_use]
pub fn event_for(notification: &Notification) -> Option<StreamEvent> {
    match notification {
        Notification::TextDelta { text } => Some(StreamEvent::Delta(text.clone())),
        // `{ name, .. }` here used to discard both the id and the arguments the
        // notification carries — so the stdio path threw away more than the SSE
        // frame was ever given (#154, #161).
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
        // `session/updated`: this client shows one session at a time, so a list
        // that moved is nothing to refresh.
        Notification::SessionUpdated { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{event_for, Logs, Rest, Rpc, Stdio, Transport};
    use crate::StreamEvent;
    use jan_klod_protocol::{Notification, PROTOCOL_VERSION};

    /// The stdio half of #154: the id reaches the client, and so do the
    /// arguments, which this mapping used to throw away with `{ name, .. }`.
    ///
    /// Asserted here as well as in `tests/parse_frame.rs` because #81 was one
    /// gap with two faces and only the loud one had been reported — a client
    /// that pairs tool calls over SSE and not over stdio would be the same
    /// shape of bug.
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

    /// Every notification is either shown or deliberately dropped, and the two
    /// that are dropped are the two named in `event_for`'s documentation.
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
        // The default is what the core takes if nobody answers, so a client
        // that lost it would offer the user a choice the core will not honour.
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

    /// #103's first design point: the session the client answers with is the
    /// notification's own, not whichever one the client happens to be on.
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

    /// A gateway that is not there is a message naming the alternative, not a
    /// panic and not a hang.
    #[test]
    fn a_missing_gateway_says_what_to_do_instead() {
        let outcome = Stdio::spawn_from(
            std::path::Path::new("/nonexistent/jan-klod-gateway"),
            &[],
            Logs::Discard,
        );
        // Not `expect_err`: `Stdio` has no `Debug`, and giving it one would
        // mean deciding how to print a live child process.
        let Err(err) = outcome else {
            panic!("there is no gateway at that path")
        };
        assert!(err.contains("--addr"), "names the alternative: {err}");
    }

    /// Acceptance line 3's transport half: the refusal carries a reason.
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
        // Nothing was sent: the address is the discard port, and this returned
        // without touching it.
    }

    /// The method name is the whole contract with the core, and a wrong one
    /// would be indistinguishable from a message that was simply ignored.
    #[test]
    fn steering_frames_as_the_method_the_core_matches_on() {
        let framed = serde_json::to_value(jan_klod_protocol::Command::TurnFollowUp {
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

    /// What actually leaves the client when a turn is steered.
    ///
    /// The serialization test above proves the *protocol* spells the method
    /// the core matches on; it says nothing about which command `follow_up`
    /// builds. A probe that made it send `turn/answer` instead passed every
    /// other test in this file, and over a real pipe that mistake is silent —
    /// the core answers "no confirmation is pending" into a reader that belongs
    /// to the turn thread, and the steering simply never happens. So this
    /// drives a fake gateway and reads back the bytes.
    ///
    /// A file rather than `sh -c`, because `spawn_from` puts its own `rpc`
    /// argument in front of the caller's — which is a fact about the real
    /// gateway's CLI, and one this test had to discover.
    #[cfg(unix)]
    #[test]
    fn steering_over_stdio_writes_a_follow_up_frame() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let sent = dir.join(format!("jk-steer-{}.jsonl", std::process::id()));
        let bin = dir.join(format!("jk-gateway-{}.sh", std::process::id()));
        let _ = std::fs::remove_file(&sent);

        // Answer the handshake with the client's own id and a version it
        // accepts, then record every frame after it.
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

        // The write is another process's read, so this waits for it rather
        // than asserting on a race.
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

    /// What actually leaves the client for `/cancel` over stdio.
    ///
    /// The same shape and the same reason as
    /// `steering_over_stdio_writes_a_follow_up_frame`: a probe that made
    /// `cancel` send the wrong method would pass every test that only checks
    /// the protocol enum, and over a real pipe the core would answer "no turn
    /// is running" into the turn thread's reader — the cancel simply would
    /// not happen. So this drives a fake gateway and reads back the bytes.
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

    /// #104: the event loop notices a dead gateway even with nothing in
    /// flight to fail — the fake gateway here exits without a turn ever
    /// being started.
    ///
    /// Both halves are waited for rather than raced against. The script used
    /// to exit as soon as it had answered the handshake, which made
    /// `alive()` before the exit a coin flip: on a loaded machine the child
    /// was often already gone by the time `spawn_from` returned, and the
    /// "still alive" assertion failed for a reason that had nothing to do
    /// with `alive()`. So the script blocks on a second frame instead — it
    /// cannot exit until this test lets it — and the exit is then polled for
    /// against a deadline.
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
        // `spawn_from` returned, so the handshake was answered and the script
        // is now blocked on its second `read` — it has no way to exit yet.
        assert!(gateway.alive(), "the child is still blocked on stdin");

        // Any frame ends that `read` and with it the script. Sent, not asked:
        // nothing answers it, and nothing here waits for an answer.
        gateway
            .send(&Rpc::SessionList)
            .expect("the frame that lets the fake gateway finish");

        // The exit is still asynchronous — the deadline is long enough that
        // only a genuinely stuck child reaches it.
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

    /// `create_session` over stdio reads its own answer, unlike `answer` and
    /// `follow_up` — it is called when no turn is streaming, so nothing else
    /// is waiting on the reader.
    #[cfg(unix)]
    #[test]
    fn create_session_over_stdio_reads_the_new_id() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let bin = dir.join(format!("jk-gateway-create-{}.sh", std::process::id()));

        // Answer the handshake, then answer `session/create` with an id —
        // never logging to a file, because this test reads the id back
        // through the client rather than inspecting bytes on disk.
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

    /// `create_session` over REST: `POST /sessions`, and the id in the body
    /// comes back out.
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

    /// `session_list`/`session_get` over stdio (#105): both read their own
    /// answer, the same shape as `create_session`, and the caveat above them
    /// says why that is only safe with nothing else streaming.
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

    /// `session_list`/`session_get` over REST (#105): `GET /sessions` and
    /// `GET /session/:id`, and the JSON comes back parsed.
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

    /// Acceptance line 2's other half: cancelling with no turn running is a
    /// clear error, not a no-op that looks like it worked.
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
