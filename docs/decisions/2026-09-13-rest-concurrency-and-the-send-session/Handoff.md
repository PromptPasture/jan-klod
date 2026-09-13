---
title: REST concurrency and the `!Send` session
date: 2026-09-13
---

# REST concurrency and the `!Send` session

## The question

`#43` (WebSocket transport) had been deferred twice, and the recorded reason was
a property of `tiny_http`: its `Request::upgrade` hands back a
`Box<dyn ReadWrite + Send>` with no `try_clone` and no `set_read_timeout`, so an
upgraded socket can neither time out a parked `ask` nor be read while it is
written. Asked whether replacing `tiny_http` would fix both WebSocket **and**
REST, the answer turned out to be more interesting than either yes or no — and
the reason this record exists is that **three separate comments in this
repository state a constraint that is not true.**

## What was measured

Every number below is reproducible; none of it is an estimate.

### 1. wasmtime is not what makes the session `!Send`

`serve.rs:12` said the REST surface serves "one request at a time on the thread
that owns the `!Send` `AgentSession`". `mcp.rs:14` said the core is "deliberately
synchronous: the agent session is `!Send` and lives on one thread, and
`tiny_http` was chosen over `axum` for the same reason". `rpc.rs:27` says the
same. All three read as though the WebAssembly runtime imposes it.

It does not. In wasmtime 46.0.3, `Store<T>`, `Instance`, `Engine`, `Component`
and wasmtime-wasi's `WasiCtx`/`ResourceTable` contain no `Rc`, no `RefCell` and
no thread-locals; the raw pointers inside `StoreOpaque` are deliberately
hardened (`unsafe impl Send for StorePtr`, `SendSyncPtr<VMContext>`), and
`ResourceTable`'s entries are typed `Box<dyn Any + Send>`.

**The actual cause is two of this repository's own traits.** `Interceptor`
(`intercept.rs:263`) and `Completer` (`conductor.rs:85`) declare no `Send`
supertrait, so `Box<dyn Interceptor>` and `Box<dyn Completer>` are `!Send` by
the auto-trait rules — regardless of what is inside them.

**Verified by compiler, not by reading.** A probe asserting `T: Send` for every
concrete production type an `AgentSession` holds — `WasmInterceptor`,
`ProviderCompleter`, `CombinedFleet`, `LazyToolFleet`, `LazyRegistryFleet`,
`Arc<Mutex<Store>>`, `Limits`, `wasmtime::Engine`,
`wasmtime::component::Component` — **compiles and passes**. Adding the two
supertrait bounds was then tried directly: **no production code failed**. The
only two compile errors were test stubs in `conductor.rs` holding
`Rc<RefCell<Vec<String>>>` as a shared call log, which `Arc<Mutex<_>>` replaces
mechanically.

So `AgentSession: Send` is roughly two trait bounds and a handful of test
fixtures away, and has been for some time.

### 2. axum costs 16 packages, not 53

A greenfield `axum` + `tokio` lockfile holds 53 packages, and that is the number
worth **not** quoting. 36 of them are already in `src/core/Cargo.lock` — `tokio`
itself arrives transitively through `wasmtime-wasi`, along with most of hyper's
supporting cast.

The marginal cost is **16 packages** on a 406-package workspace: **+3.9%**.

```
atomic-waker  axum  axum-core  http-body  http-body-util  hyper  hyper-util
matchit  mime  serde_path_to_error  serde_urlencoded  sync_wrapper
tokio-macros  tower  tower-layer  tower-service
```

For scale: `schemars` was rejected at 7 packages (#41), and Tauri was accepted at
+256 (#141).

### 3. It passes the licence and advisory policy unmodified

`cargo deny --config deny.toml check` against that tree:
**advisories ok, bans ok, licenses ok, sources ok.**

This is the contrast with #141 that matters. Tauri needed eleven named
`deny.toml` entries — five MPL-2.0 exceptions and six unmaintained advisories —
and #141 recorded that there was no thinner path to a system webview which the
policy accepted. `axum` needs no widening at all.

### 4. `serde` is already here, and is orthogonal

`serde` and `serde_json` are workspace dependencies used throughout
(`protocol/Cargo.toml:15`, `core/Cargo.toml:17`). They are not something an async
REST surface would newly enable, and they are not an argument either way.

## What this does and does not fix

Replacing the HTTP library fixes **WebSocket**: a real `TcpStream` has
`try_clone` and `set_read_timeout`, which is exactly the pair `tiny_http` cannot
supply, and #43's central obstacle disappears.

It does **not**, by itself, fix REST's one-request-at-a-time behaviour. That
comes from the session being `!Send` and owned by the accept loop's thread — a
constraint upstream of any HTTP library. The two are independent, and conflating
them is what made this question look like one decision instead of two.

The clearest illustration is `serve.rs`'s mid-turn confirmation mechanism
(`PromptDriver::wait_for_answer`, ~90 lines): because the turn blocks inside
`Driver::ask` on the thread that owns the socket, the *driver serves the socket
itself*, calling `Server::recv_timeout` in a loop and replying `409` to
everything that is not the matching answer. That whole apparatus exists to work
around single-threadedness. Given worker threads it collapses into routing the
answer onto a channel.

## The decision

**Adopt `axum` + `tokio` for the inbound HTTP surface, and give the session
thread affinity rather than making every driver `Send`.**

The async layer handles connections; a turn runs on a thread that owns its
session, reached over a channel. `AgentSession` does not have to become `Send`
for that shape to work, though §1 shows it cheaply could.

`mcp.rs:14`'s rejection of `axum` was correct on the evidence available when it
was written and is superseded here on three measured grounds: the marginal cost
is 16 packages rather than a new ecosystem, the policy accepts it unmodified,
and the `!Send` premise it rested on is a property of two local trait
declarations rather than of the runtime.

### The alternative, and why it was not taken

**An actor thread behind the existing `tiny_http` loop** — one thread per
session receiving `(message, reply)` over an `mpsc::channel` — reaches the same
concurrency with **zero** new packages, and this repository already does exactly
that: `acp.rs:298-310` runs a reader thread handing lines to the session-owning
thread over a channel, for precisely this reason.

It was not taken because it leaves #43 exactly where it was. `tiny_http` still
cannot hand back a socket that both times out a read and reads while writing, so
WebSocket would still need either a second listener (a second place the
`JAN_KLOD_TOKEN` rule is enforced — a security check existing twice) or a
hand-rolled server. Choosing `axum` answers both questions with one change.

## What it touches

- `serve.rs` (851 lines) — the route table, and `PromptDriver`/`wait_for_answer`.
- `mcp.rs`, `rpc.rs`, `acp.rs`, `telegram.rs` — the four drivers sharing writers
  through `Rc<RefCell<_>>`, and the comments in all of them asserting a
  constraint that this record retires.
- `host/src/main.rs:854` — where the server is constructed.
- Seven test files using `tiny_http::Server` as a *fixture*; those can keep it as
  a dev-dependency rather than being rewritten.
- **No test asserts the current one-request-at-a-time contract.** `409` appears
  only in `serve.rs` and one `rpc.rs` comment, so the behaviour being changed is
  currently unverified in either direction — which is its own finding, and means
  the replacement needs the tests the original never had.

## Reproducing the measurements

```sh
# marginal package cost
cargo new --lib /tmp/axum-probe && cd /tmp/axum-probe
cargo add axum && cargo add tokio --features rt-multi-thread,macros,net
comm -23 <(grep '^name = ' Cargo.lock | sed 's/name = "//;s/"//' | sort -u) \
         <(grep '^name = ' <repo>/src/core/Cargo.lock | sed 's/name = "//;s/"//' | sort -u)

# policy
cargo deny -L error --config <repo>/deny.toml check

# the Send probe: add `Send` to Interceptor and Completer, then
#   const fn assert_send<T: Send>() {}  and call it on AgentSession
```
