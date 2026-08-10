---
title: Security model
description: What a component is granted, how, where it is enforced, and which test proves it.
---

# Security model

The claim is that the core trusts nothing it runs. This page is the ledger behind
it: every capability a component can reach, what it gets by default, what an
operator has to write to widen that, the single place the rule is enforced, and
the test that fails if it stops being true.

The last column is the point. Nine of the eleven rows below were written *after* a
defect in that row, and in most cases the defect had been sitting in a green
suite: the permission gate never confirmed the write tool, subprocesses inherited
`OPENAI_API_KEY`, a grep returned `.env`, guests could read the host's stdin. Each
was invisible because nothing asked "what proves this?" — so the question is now
asked in a table, and `docs_match_config.rs` fails if a test named here does not
exist.

## Capabilities

| Capability | Default | Grant | Enforced in | Proven by |
|---|---|---|---|---|
| Filesystem (`host-fs`) | The working directory, **unless** it is `$HOME` or a filesystem root — those are refused with a reason, because a jail that wide protects nothing | `workspace:` names any root explicitly | `core::host_fs::Workspace::resolve` | `core/src/host_fs.rs::escapes_are_denied`, `core/src/lib.rs::a_workspace_is_not_adopted_from_home_or_a_root` |
| Ambient filesystem (`wasi:filesystem`) | Denied — the linker wires it, no preopens are configured | none | `WasiCtxBuilder` (no preopens) | `host/tests/sandbox_boundary.rs::a_guest_cannot_read_the_hosts_filesystem` |
| Subprocesses (`host-process`) | Denied | `execution.enabled` **and** a workspace | `core::host_process::ProcessRunner::exec` — cwd jail, timeout, output cap | `core/src/host_process.rs::a_cwd_escape_is_denied`, `::a_slow_command_times_out`, `::output_is_capped` |
| A subprocess's environment | `PATH`, `HOME`, `CARGO_HOME`, `RUSTUP_HOME`, `TMPDIR`, `LANG`, `LC_ALL`, `LC_CTYPE` — the gateway's own environment holds `OPENAI_API_KEY` and `JAN_KLOD_TOKEN` | `execution.env-passthrough: [NAME]` | `core::host_process::ProcessRunner::environment` | `core/src/host_process.rs::a_command_does_not_inherit_the_hosts_secrets`, `host/tests/host_process.rs::a_guest_run_command_does_not_receive_the_hosts_credentials` |
| Outbound HTTP (`host-http`) | Public destinations only; loopback, private, link-local and unique-local refused. Hostnames are resolved before the decision | Any origin `config.yaml` already names (`base-url`, `endpoint`), plus `network.allow` | `core::egress::EgressPolicy::check`, via `http::fetch_within` | `core/src/egress.rs::loopback_and_private_addresses_are_refused`, `host/tests/egress_boundary.rs::a_live_local_service_is_not_reachable`, `::every_guest_facing_backend_goes_through_the_policy` |
| Raw sockets (`wasi:sockets`) | Denied — the linker wires TCP and UDP, every address is refused | none | `WasiCtx`'s `SocketAddrCheck` (deny-all default) | `host/tests/sandbox_boundary.rs::a_guest_cannot_open_its_own_socket`, `::a_guest_cannot_open_a_socket_to_a_public_address` |
| The host's stdin | None. `jan-klod-gateway` runs in a terminal, so this is the user's keystrokes — including an answer being typed at a permission prompt | none | `WasiCtxBuilder::inherit_stderr` (not `inherit_stdio`) | `host/tests/sandbox_boundary.rs::a_guest_gets_no_standard_input` |
| Persistence (`host-storage`) | A private map that dies with the process, which is what keeps the permission gate's standing grants run-scoped | `persist: true` per instance | `core::interceptor_host::Storage` — namespaces prefixed with the component id | `host/tests/storage_scope.rs::a_standing_grant_does_not_survive_a_restart`, `::a_namespace_a_guest_can_name_never_reaches_another_components_data` |
| Tool calls | Confirmed, unless the call is on the read-only allowlist (`find`, `fs:read`, `fs:grep`, `git`, `edit:view`, `proc-probe`) | `safe-calls` replaces the allowlist | `interceptor-permission` at `Phase::ToolCall` | `interceptor-permission/src/rules.rs::a_tool_nobody_has_classified_is_confirmed`, `host/tests/tool_wiring.rs::an_edit_is_confirmed_before_it_touches_the_file` |
| Credential files | Skipped by `find` and tree-wide `grep`; an explicit read is confirmed and never covered by an "always" | A pattern naming the file (`**/.env`) lists it; nothing widens the read | `guest_fs::hidden_credential`, `interceptor-permission`'s `TouchesCredentials` | `guest-fs/src/lib.rs::credential_files_are_recognised`, `host/tests/tool_fleet.rs::a_tree_grep_skips_credential_files` |
| A parked turn | Ends within one heartbeat of the client disappearing, and at the confirmation timeout otherwise; the default answer is a denial | `JK_ANSWER_TIMEOUT_SECS` | `core::serve::PromptDriver::wait_for_answer` | `host/tests/prompt_disconnect.rs::a_disconnected_client_does_not_hold_the_turn_open` |
| The REST surface | Open when no token is set — loopback-only by default, and a non-loopback bind without a token is warned about at boot | `JAN_KLOD_TOKEN` | `core::serve::authorised` | `host/tests/auth.rs::without_a_token_a_turn_is_refused_and_never_reaches_the_agent`, `::an_unauthenticated_caller_cannot_answer_a_permission_prompt` |

## Two rules that shape all of it

**A grant is written down, and it is narrow.** Every widening above names a thing
— an origin, a variable, a call, a root — rather than flipping a mode. This is not
tidiness. `network.allow` could have been `allow-local: true`, and then permitting
a local Ollama would have permitted the local Postgres beside it; `env-passthrough`
could have been `inherit-env: true`, which is the bug it replaced.

**A check inside the sandbox is advice, not a boundary.** `tool-fetch` carries an
SSRF guard, and for weeks that guard *was* the runtime's SSRF defence. It runs in
the guest. It protects a confused model from a URL the model chose and says
nothing about a component, which is the party being distrusted — a component that
would rather not check simply does not. The guard stays, because a confused model
is the common case; the boundary moved host-side. The same reasoning applies to
every row: if the rule can only be enforced by the thing being constrained, it is
not enforced.

## Known gaps

Stated because a security page that lists only its wins is marketing.

- **DNS rebinding.** The egress policy resolves a hostname and checks every
  address it answers with, then hands the URL to the HTTP client, which resolves
  again. A name that answers differently the second time slips through. Closing it
  needs the resolved address pinned into the connection, which `ureq` does not
  expose.
- **The credential-file rule is a name heuristic.** It covers `.env`, `*.pem`,
  `id_rsa` and the rest of the conventional set. It will not notice
  `config/production.yaml` holding a database URL.
- **A guest with a credential can leak it.** `provider-openai` is given
  `api-key` because it cannot call the API without one, and it can write whatever
  it likes to stderr. Nothing here protects a secret from the component that was
  handed it; the boundary is about components that were *not*.
- **stderr is shared.** Guests can write misleading lines to it. Host log lines
  are tagged by the host, so this is cosmetic, but it is not nothing.
- **A confirmation answer can be lost.** One run of `auth.rs` waited out two full
  answer timeouts. Twenty-one further attempts — six isolated, fifteen under CPU
  load — have not reproduced it, and three structural explanations were checked
  and ruled out: `tiny_http` grows its worker pool rather than starving on a
  held-open stream, SSE frames are flushed as they are written, and a first failed
  write already short-circuits. A lost answer now fails loudly instead of silently
  taking the default, so the next occurrence arrives with a diagnostic. That is
  not a fix, and it is not claimed as one.

## What this does not claim

Not a defence against a malicious *operator* — anyone who can edit `config.yaml`
can name any root, any origin, any variable, and that is correct: it is their
machine. The threat model is a component that misbehaves, a model that is confused
or steered by content it read, and the ordinary accident of running the agent in
the wrong directory.
