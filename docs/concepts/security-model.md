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
| Ambient filesystem (`wasi:filesystem`) | Denied — the linker wires it, no preopens are configured | none | `WasiCtxBuilder` (no preopens) | `host/tests/it/sandbox_boundary.rs::a_guest_cannot_read_the_hosts_filesystem` |
| Subprocesses (`host-process`) | Denied | `execution.enabled` **and** a workspace | `core::host_process::ProcessRunner::exec` — cwd jail, timeout, output cap | `core/src/host_process.rs::a_cwd_escape_is_denied`, `::a_slow_command_times_out`, `::output_is_capped` |
| A subprocess's environment | `PATH`, `HOME`, `CARGO_HOME`, `RUSTUP_HOME`, `TMPDIR`, `LANG`, `LC_ALL`, `LC_CTYPE` — the gateway's own environment holds `OPENAI_API_KEY` and `JAN_KLOD_TOKEN` | `execution.env-passthrough: [NAME]` | `core::host_process::ProcessRunner::environment` | `core/src/host_process.rs::a_command_does_not_inherit_the_hosts_secrets`, `host/tests/it/host_process.rs::a_guest_run_command_does_not_receive_the_hosts_credentials` |
| Extension manifest | Refused: a component with no manifest, one that imports a `host-*` interface it does not declare, or one built against an incompatible `jan-klod:interfaces` version. Declaring *more* than `config.yaml` grants is allowed and grants nothing | `allow-unmanifested: true` (top-level) loads a component that declares nothing | `core::manifest::Manifest::beside` and `core::manifest::Manifest::undeclared`, compared against `core::host_capabilities`, both at `Runtime::boot` | `host/tests/it/manifest.rs::a_component_importing_more_than_it_declares_is_refused`, `::a_component_with_no_manifest_is_refused`, `::the_grant_is_off_by_default`, `::a_component_built_against_another_api_version_is_refused`, `::what_a_component_imports_matches_what_its_manifest_declares` |
| Extension install (`ext install`) | Refused unless a minisign signature over **both** the component and its manifest verifies under a key named in `registry.trusted-keys` — which ships empty, so **every** install is refused until a key is configured. Prehashed signatures only; the legacy format is refused. A digest is checked when given. Nothing half-verified is ever visible: the pair is staged in `ext/.staging/<name>/` and a refusal leaves `ext/` byte-identical. **A remote source must be a public destination**, refused before a byte moves and again per redirect hop by the client that fetches it | `registry.trusted-keys` names who may vouch. `--allow-unsigned` waives the signature and then **requires** `--sha256 <hex>`, so no combination lands a component with no evidence | `core::ext::install` — digest, then signature over both files, then `core::inspect` (the same cross-check `Runtime::boot` uses, so an install cannot accept what boot refuses), then an atomic rename. A URL adds `core::ext::check_remote` per file and `core::ext::policy_bound_http`, then hands the bytes to the same `install`, so the download has no verification path of its own | `host/tests/it/ext_install.rs::a_signature_from_a_trusted_key_over_both_files_installs`, `::a_signature_over_the_component_but_not_the_manifest_is_refused`, `::a_valid_signature_from_an_untrusted_key_is_refused`, `::bytes_changed_after_signing_are_refused`, `::a_legacy_format_signature_is_refused`, `::no_configured_keys_refuses_rather_than_accepting_anything`, `::allow_unsigned_needs_a_digest_and_says_so`, `::a_correct_digest_installs_and_a_tampered_byte_is_refused`, `::a_manifest_that_under_declares_is_refused_and_nothing_lands`, `::a_component_installed_here_is_loaded_by_a_real_boot`, `core/src/ext.rs::a_file_that_is_not_a_component_is_refused_and_leaves_nothing`, `core/src/ext.rs::each_refusal_says_something_different`, `host/tests/it/ext_install.rs::a_loopback_source_is_refused_before_anything_is_fetched`, `::private_link_local_and_metadata_sources_are_refused`, `::a_refused_destination_is_not_even_requested`, `::a_redirect_to_a_refused_destination_ends_a_remote_install`, `::bytes_tampered_in_flight_are_refused`, `::a_url_with_a_query_is_refused_rather_than_mangled` |
| Command effects (`execution.sandbox`) | **macOS and Linux: the command is confined.** Seatbelt via a generated `sandbox-exec` profile; Landlock via a ruleset the gateway applies to itself before becoming the command. Both: reads allowed, writes only under `writable`, no network. **Elsewhere: approval-only**, and then the confirmation prompt is the only barrier. Default `mode: os`, `writable: ["."]`, `network: false` | `execution.sandbox.mode`; `require: true` denies `host-process` outright rather than degrading — on macOS and Linux it now *permits* commands, since there is something to require | `core::sandbox::SandboxPolicy::resolve` picks the mode and `core::host_process::ProcessRunner::exec` applies the backend, both from the one decision `Runtime::open_process_runner` reports at boot | macOS: `host/tests/it/sandbox_seatbelt.rs::a_confined_command_cannot_write_outside_the_workspace`, `host/tests/it/sandbox_seatbelt.rs::a_confined_command_can_still_write_inside_the_workspace`, `host/tests/it/sandbox_seatbelt.rs::a_confined_command_cannot_reach_the_network`, `host/tests/it/sandbox_seatbelt.rs::a_guest_running_a_command_is_confined_too`, `core/src/sandbox_seatbelt.rs::a_writable_path_reaches_the_profile_symlink_resolved`, `core/src/sandbox_seatbelt.rs::a_path_that_would_inject_sbpl_is_refused_rather_than_escaped`. Linux: `host/tests/it/sandbox_landlock.rs::a_confined_command_cannot_write_outside_the_workspace`, `host/tests/it/sandbox_landlock.rs::a_confined_command_can_still_write_inside_the_workspace`, `host/tests/it/sandbox_landlock.rs::a_confined_command_cannot_reach_the_network`, `host/tests/it/sandbox_landlock.rs::a_guest_running_a_command_is_confined_too`, `core/src/sandbox_landlock.rs::what_the_backend_writes_is_what_the_wrapper_reads`, `core/src/sandbox_landlock.rs::a_confine_that_cannot_read_its_policy_refuses_rather_than_running`. Both: `core/src/sandbox.rs::os_mode_with_no_backend_becomes_approval_only_and_says_why` |
| Outbound HTTP (`host-http`) | Public destinations only; loopback, private, link-local and unique-local refused. Hostnames are resolved before the decision, and **every hop of a redirect chain is checked, not only the URL the caller named** — up to 10 hops, then refused. `Authorization`, `Cookie` and `Proxy-Authorization` do not survive a hop | Any origin `config.yaml` already names (`base-url`, `endpoint`), plus `network.allow` | `core::egress::EgressPolicy::check`, called per hop by the redirect loop in `http::fetch_within` — which sets `max_redirects(0)` on the agent so ureq cannot follow one unchecked | `core/src/egress.rs::loopback_and_private_addresses_are_refused`, `host/tests/it/egress_boundary.rs::a_live_local_service_is_not_reachable`, `::every_guest_facing_backend_goes_through_the_policy`, `::a_permitted_origin_cannot_redirect_to_a_refused_one`, `::a_redirect_to_a_permitted_destination_is_followed`, `::credentials_do_not_survive_a_redirect`, `::a_302_becomes_a_get_and_a_307_keeps_the_post` |
| Raw sockets (`wasi:sockets`) | Denied — the linker wires TCP and UDP, every address is refused | none | `WasiCtx`'s `SocketAddrCheck` (deny-all default) | `host/tests/it/sandbox_boundary.rs::a_guest_cannot_open_its_own_socket`, `::a_guest_cannot_open_a_socket_to_a_public_address` |
| The host's environment | None. It holds `OPENAI_API_KEY` (config expands it) and `JAN_KLOD_TOKEN`, so a guest reading it directly is the shortest path to the operator's credentials | none | `WasiCtxBuilder` (`inherit_env` is never called) | `host/tests/it/sandbox_boundary.rs::a_guest_cannot_read_the_hosts_environment` |
| The host's stdin | None. `jan-klod-gateway` runs in a terminal, so this is the user's keystrokes — including an answer being typed at a permission prompt | none | `WasiCtxBuilder::inherit_stderr` (not `inherit_stdio`) | `host/tests/it/sandbox_boundary.rs::a_guest_gets_no_standard_input` |
| Persistence (`host-storage`) | A private map that dies with the process, which is what keeps the permission gate's standing grants run-scoped | `persist: true` per instance | `core::interceptor_host::Storage` — namespaces prefixed with the component id | `host/tests/it/storage_scope.rs::a_standing_grant_does_not_survive_a_restart`, `::a_namespace_a_guest_can_name_never_reaches_another_components_data` |
| Tool calls | Confirmed, unless the call is on the read-only allowlist (`find`, `fs:read`, `fs:grep`, `git`, `edit:view`, `proc-probe`) | `safe-calls` replaces the allowlist | `interceptor-permission` at `Phase::ToolCall` | `interceptor-permission/src/rules.rs::a_tool_nobody_has_classified_is_confirmed`, `host/tests/it/tool_wiring.rs::an_edit_is_confirmed_before_it_touches_the_file` |
| Credential files | Skipped by `find` and tree-wide `grep`; an explicit read is confirmed and never covered by an "always" | A pattern naming the file (`**/.env`) lists it; nothing widens the read | `guest_fs::hidden_credential`, `interceptor-permission`'s `TouchesCredentials` | `guest-fs/src/lib.rs::credential_files_are_recognised`, `host/tests/it/tool_fleet.rs::a_tree_grep_skips_credential_files` |
| A parked turn | Ends within one heartbeat of the client disappearing, and at the confirmation timeout otherwise; the default answer is a denial | `JK_ANSWER_TIMEOUT_SECS` | `core::serve::PromptDriver::wait_for_answer` | `host/tests/it/prompt_disconnect.rs::a_disconnected_client_does_not_hold_the_turn_open` |
| The REST surface | Open when no token is set — loopback-only by default, and a non-loopback bind without a token is warned about at boot | `JAN_KLOD_TOKEN` | `core::serve::authorised` | `host/tests/it/auth.rs::without_a_token_a_turn_is_refused_and_never_reaches_the_agent`, `::an_unauthenticated_caller_cannot_answer_a_permission_prompt` |

## Two rules that shape all of it

**A grant is written down, and it is narrow.** Every widening above names a thing
— an origin, a variable, a call, a root — rather than flipping a mode. This is not
tidiness. `network.allow` could have been `allow-local: true`, and then permitting
a local Ollama would have permitted the local Postgres beside it; `env-passthrough`
could have been `inherit-env: true`, which is the bug it replaced.

**A check whose coverage is a literal will miss the thing added after it.** The
egress guard listed the three files implementing `host-http` and there were four —
`route.rs`, the provider path, and the busiest egress route here. Three checks on
this page have now been written around the instance that prompted them and missed
their own class: the store category, the `type:` scan, and this. Where a check
enumerates, it should ask the source rather than carry a list.

**A check inside the sandbox is advice, not a boundary.** `tool-fetch` carries an
SSRF guard, and for weeks that guard *was* the runtime's SSRF defence. It runs in
the guest. It protects a confused model from a URL the model chose and says
nothing about a component, which is the party being distrusted — a component that
would rather not check simply does not. The guard stays, because a confused model
is the common case; the boundary moved host-side. The same reasoning applies to
every row: if the rule can only be enforced by the thing being constrained, it is
not enforced.

**And a boundary in the right place can still check the wrong thing.** The egress
check was host-side, careful about resolvers, and correct about every address
class — and for weeks it was applied to the URL the caller *named* rather than the
destination actually contacted, because the HTTP client followed up to ten
redirects on its own. A permitted origin answering `302 Location:
http://169.254.169.254/…` reached cloud metadata with the policy's blessing. The
row above claimed "public destinations only" throughout, and the tests it cited
never redirected, so nothing disagreed with it
([#107](https://github.com/PromptPasture/jan-klod/issues/107)). Worth keeping
next to the row: *where* a check runs is only half of it, and a library's
defaults are part of the boundary whether or not anyone chose them.

## Known gaps

Stated because a security page that lists only its wins is marketing.

- **`host-process` confines the caller, not the command.** The grant is narrow —
  `execution.enabled`, a workspace-relative cwd, a timeout, an output cap, a
  scrubbed environment — but the command itself runs with the user's privileges
  and can read or write anywhere the user can. The path jail belongs to
  `host-fs`; nothing here confines what `sh -c` does once it is running. Closing
  it needs an OS-level sandbox (Seatbelt on macOS, Landlock + seccomp on Linux) —
  vision [decision 2](../decisions/2026-09-08-harness-platform-vision/Vision.md#decisions),
  planned as [roadmap Phase 15](roadmap.md#phase-15--os-level-effect-sandbox).
  **Closed on macOS (15b) and Linux (15c); open on Windows.** A command's writes
  are refused by the kernel rather than by a prompt — Seatbelt wraps it in
  `sandbox-exec`, Landlock has the gateway restrict *itself* and then become the
  command. Proven by the tests in the row above, each of which runs the same
  command unconfined first, because a command that failed for an unrelated reason
  looks exactly like a denial. On Windows nothing has changed: the policy is read,
  the mode resolves to `approval-only`, and the boot warning says so. A Windows
  backend is a *spike* rather than an implementation
  ([Slice 15d](roadmap.md#phase-15--os-level-effect-sandbox)).

  Two limits of the macOS half, because "confined" invites more confidence than
  it should:

  - **Reads are not confined**, only writes and the network. A confined command
    can still read anything the user can. Narrowing that is a separate argument
    with a real cost — a command that cannot read its own toolchain does not run.
  - **A command needing a Mach service fails** on macOS rather than running
    unconfined — some of what `git` and `cargo` reach for. That is the right
    direction to fail in and it is visible in the command's own error, but it is a
    rough edge, not a polished sandbox.
  - **Denying the network on Linux needs a 6.7 kernel** (Landlock ABI 4). Below
    that the runtime refuses to run the command rather than reporting a
    restriction it cannot apply, and the error says both ways out.
  - **Only the Linux half is verified by CI.** CI runs on `ubuntu-latest`, which
    has Landlock, so a regression there cannot land green. The Seatbelt tests are
    macOS-only and CI has no macOS runner
    ([#95](https://github.com/PromptPasture/jan-klod/issues/95)), so that half is
    verified locally — where this repository is developed, but that is a
    developer's diligence rather than a gate. The two backends are equally
    tested and unequally *guarded*.
  - One piece of 15a is **deferred**: a per-turn `Warning` telling the user, on
    every turn that runs a command, that the command is not isolated. Nothing
    currently distinguishes a tool that uses `host-process` from one that only
    reads files — every tool is handed the same runner — so the warning could
    only fire on *every* tool-using turn, which teaches people to ignore it. It
    waits for [Phase 16a](roadmap.md#phase-16--capability-manifest--signed-registry)'s
    component-import introspection, which can answer the question exactly.
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
