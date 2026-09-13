---
title: REST concurrency and the `!Send` session
date: 2026-09-13
---

# REST concurrency and the `!Send` session

## The question

`#43` (WebSocket) was deferred due to `tiny_http`'s `Box<dyn ReadWrite + Send>`—no `try_clone` or `set_read_timeout`. Asked whether replacing it fixes both WebSocket and REST, the answer revealed **three repository comments stating a false constraint.**

## What was measured

Every number below is reproducible; none of it is an estimate. The measurements answer three questions: what actually makes the session `!Send`, what the cost of `axum` really is, and whether the policy accepts it.

### 1. wasmtime is not what makes the session `!Send`

`serve.rs:12`, `mcp.rs:14`, and `rpc.rs:27` all claim the WASM runtime forces `!Send`. It doesn't. In wasmtime 46.0.3, `Store<T>`, `Instance`, `Engine`, `Component`, and wasmtime-wasi's `WasiCtx`/`ResourceTable` contain no `Rc`, no `RefCell`, and no thread-locals; the raw pointers inside `StoreOpaque` are deliberately hardened, and `ResourceTable` entries are typed `Box<dyn Any + Send>`.

**The actual cause is two of this repository's own traits.** `Interceptor` (`intercept.rs:263`) and `Completer` (`conductor.rs:85`) declare no `Send` supertrait, so `Box<dyn Interceptor>` and `Box<dyn Completer>` are `!Send` by the auto-trait rules.

**Verified by compiler, not by reading.** A probe asserting `T: Send` for every concrete type held by `AgentSession`—`WasmInterceptor`, `ProviderCompleter`, `CombinedFleet`, `LazyToolFleet`, `LazyRegistryFleet`, `Arc<Mutex<Store>>`, `wasmtime::Engine`, `wasmtime::component::Component`—**compiles and passes**. Adding the two supertrait bounds causes **no production code failures**; only two test stubs holding `Rc<RefCell<Vec<String>>>` mechanically convert to `Arc<Mutex<_>>`.

`AgentSession: Send` is two trait bounds and a handful of test changes away, and has been for some time.

### 2. axum costs 16 packages, not 53

A greenfield `axum` + `tokio` lockfile holds 53 packages; 36 of them are already in `src/core/Cargo.lock` — `tokio` itself arrives transitively through `wasmtime-wasi`, along with most of hyper's supporting cast. The marginal cost is **16 packages** on a 406-package workspace: **+3.9%**.

```
atomic-waker  axum  axum-core  http-body  http-body-util  hyper  hyper-util
matchit  mime  serde_path_to_error  serde_urlencoded  sync_wrapper
tokio-macros  tower  tower-layer  tower-service
```

For scale: `schemars` was rejected at 7 packages (#41), and Tauri was accepted at
+256 (#141).

### 3. It passes the licence and advisory policy unmodified

`cargo deny` passes unmodified. Contrast with #141: Tauri needed eleven `deny.toml` entries (five MPL-2.0, six advisories); `axum` needs none.

### 4. `serde` is already here, and is orthogonal

`serde` and `serde_json` are workspace dependencies; async wouldn't newly enable them.

## What this does and does not fix

Replacing the HTTP library fixes **WebSocket**: a real `TcpStream` has `try_clone` and `set_read_timeout` that `tiny_http` cannot supply. #43's obstacle disappears.

It doesn't fix REST's one-request-at-a-time behavior—that comes from `!Send` session ownership, upstream of any library. The two are independent; conflating them made this look like one decision instead of two.

`serve.rs`'s mid-turn confirmation (`PromptDriver::wait_for_answer`) illustrates the workaround: the turn blocks on the socket-owning thread, so the driver serves it itself, looping on `Server::recv_timeout` and replying `409` to non-matching messages. This apparatus exists to work around single-threadedness; worker threads collapse it to channel routing.

## The decision

**Adopt `axum` + `tokio` for the inbound HTTP surface, and give the session thread affinity rather than making every driver `Send`.**

The async layer handles connections; a turn runs on a thread that owns its session, reached over a channel. `AgentSession` does not have to become `Send` for this shape to work, though §1 shows it cheaply could.

`mcp.rs:14`'s rejection of `axum` was correct on the evidence available when written, and is superseded here on three measured grounds: the marginal cost is 16 packages rather than a new ecosystem, the policy accepts it unmodified, and the `!Send` premise rested on two local trait declarations rather than the runtime.

### The alternative, and why it was not taken

**One thread per session over `mpsc::channel`** reaches the same concurrency with zero packages (`acp.rs:298-310` already does this). It's not taken because it leaves #43 unresolved—`tiny_http` still can't hand back a socket that both times out and reads-while-writing. `axum` answers both with one change.

## What it touches

- `serve.rs` (851 lines)—the route table, and `PromptDriver`/`wait_for_answer`.
- `mcp.rs`, `rpc.rs`, `acp.rs`, `telegram.rs`—the four drivers sharing writers through `Rc<RefCell<_>>`, and comments in all asserting a constraint this record retires.
- `host/src/main.rs:854`—where the server is constructed.
- Seven test files using `tiny_http::Server` as a fixture; these can keep it as a dev-dependency.
- **No test asserts the current one-request-at-a-time contract.** `409` appears only in `serve.rs` and one `rpc.rs` comment, so the behavior is currently unverified either way. The replacement needs the tests the original never had.

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
