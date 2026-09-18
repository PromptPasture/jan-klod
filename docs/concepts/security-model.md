---
title: Security model
description: What a component is granted, how, where it is enforced, and which test proves it.
---

# Security model

The core trusts nothing it runs. This page is the ledger: every capability a
component can reach, what it gets by default, what an operator must write to grant
it, where the rule is enforced, and which test proves it.

Most rows were written after finding a defect that had sat in green: the permission
gate never confirmed the write tool, subprocesses inherited `OPENAI_API_KEY`, a grep
returned `.env`, guests could read the host's stdin. The tests now ask "what proves
this?", and `docs_match_config.rs` fails if a test named here does not exist.

## Capabilities

| Capability | Default | Grant | Enforced in | Proven by |
|---|---|---|---|---|
| Filesystem (`host-fs`) | Working directory, **unless** it is `$HOME` or a filesystem root — refused because a jail that wide protects nothing | `workspace:` names any root explicitly | `core::host_fs::Workspace::resolve` | `core/src/host_fs.rs::escapes_are_denied`, `core/src/lib.rs::a_workspace_is_not_adopted_from_home_or_a_root` |
| Ambient filesystem (`wasi:filesystem`) | Denied — the linker wires it, no preopens are configured | none | `WasiCtxBuilder` (no preopens) | `host/tests/it/sandbox_boundary.rs::a_guest_cannot_read_the_hosts_filesystem` |
| Subprocesses (`host-process`) | Denied | `execution.enabled` and a workspace | `core::host_process::ProcessRunner::exec` — cwd jail, timeout, output cap | `core/src/host_process.rs::a_cwd_escape_is_denied`, `::a_slow_command_times_out`, `::output_is_capped` |
| A subprocess's environment | `PATH`, `HOME`, `CARGO_HOME`, `RUSTUP_HOME`, `LANG`, `LC_ALL`, `LC_CTYPE` — gateway holds `OPENAI_API_KEY` and `JAN_KLOD_TOKEN`. `TMPDIR` is **set, not inherited**: `<workspace>/.jan-klod/tmp`, so a confined command's temp writes land inside the jail and nothing outside it is granted (#212) | `execution.env-passthrough: [NAME]` | `core::host_process::ProcessRunner::environment` | `core/src/host_process.rs::a_command_does_not_inherit_the_hosts_secrets`, `::a_commands_temp_directory_is_inside_the_workspace`, `::a_command_writing_to_its_temp_directory_stays_in_the_workspace`, `host/tests/it/host_process.rs::a_guest_run_command_does_not_receive_the_hosts_credentials` |
| Extension manifest | Refused: no manifest, imports undeclared `host-*` interface, or incompatible `jan-klod:interfaces` version. Declaring more than `config.yaml` grants is allowed but grants nothing | `allow-unmanifested: true` loads a component that declares nothing | `core::manifest::Manifest::beside` and `core::manifest::Manifest::undeclared`, compared against `core::host_capabilities`, at `Runtime::boot` | `host/tests/it/manifest.rs::a_component_importing_more_than_it_declares_is_refused`, `::a_component_with_no_manifest_is_refused`, `::the_grant_is_off_by_default`, `::a_component_built_against_another_api_version_is_refused`, `::what_a_component_imports_matches_what_its_manifest_declares` |
| Extension install (`ext install`) | Refused unless minisign signature over both component and manifest verifies under a `registry.trusted-keys` key — ships empty so **every** install is refused until a key is configured. Prehashed signatures only. Digest checked when given. Nothing half-verified is visible: staged in `ext/.staging/<name>/`, refusal leaves `ext/` byte-identical. **Remote source must be public**, refused before a byte moves and per redirect hop | `registry.trusted-keys` names who may vouch. `--allow-unsigned` requires `--sha256 <hex>` | `core::ext::install` — digest, signature over both files, then `core::inspect` (same as `Runtime::boot`), atomic rename. URL adds `core::ext::check_remote` per file and `core::ext::policy_bound_http` | `host/tests/it/ext_install.rs::a_signature_from_a_trusted_key_over_both_files_installs`, `::a_signature_over_the_component_but_not_the_manifest_is_refused`, `::a_valid_signature_from_an_untrusted_key_is_refused`, `::bytes_changed_after_signing_are_refused`, `::a_legacy_format_signature_is_refused`, `::no_configured_keys_refuses_rather_than_accepting_anything`, `::allow_unsigned_needs_a_digest_and_says_so`, `::a_correct_digest_installs_and_a_tampered_byte_is_refused`, `::a_manifest_that_under_declares_is_refused_and_nothing_lands`, `::a_component_installed_here_is_loaded_by_a_real_boot`, `core/src/ext.rs::a_file_that_is_not_a_component_is_refused_and_leaves_nothing`, `core/src/ext.rs::each_refusal_says_something_different`, `host/tests/it/ext_install.rs::a_loopback_source_is_refused_before_anything_is_fetched`, `::private_link_local_and_metadata_sources_are_refused`, `::a_refused_destination_is_not_even_requested`, `::a_redirect_to_a_refused_destination_ends_a_remote_install`, `::bytes_tampered_in_flight_are_refused`, `::a_url_with_a_query_is_refused_rather_than_mangled` |
| Registry index (`registry.url`) | A **directory, not an authority**: reading grants nothing; entries still go through install row above. Remote index is a destination — refused before a byte moves unless public, per redirect hop. Entry digest becomes install digest; contradicting `--sha256` stops install. Malformed or absent index is a refusal naming the file, never empty result | `registry.url` names index; `registry.trusted-keys` decides what lands; empty list refuses all as untrusted | `core::ext_index::load` — `ext::check_remote` then `ext::policy_bound_http` for URL; `core::ext_index::install` sets digest and hands entry's URL to `ext::install_from_url` | `host/tests/it/ext_registry.rs::a_loopback_index_is_refused_before_it_is_requested`, `::bytes_that_are_not_the_ones_the_index_describes_are_refused`, `::a_digest_the_index_contradicts_is_refused_before_anything_is_fetched`, `::an_unreachable_index_says_so_rather_than_reading_as_empty`, `::a_malformed_local_index_names_the_file_and_the_problem`, `::a_name_from_a_local_index_installs_offline`, `core/src/ext_index.rs::something_that_is_not_an_index_is_refused_rather_than_read_as_empty`, `core/src/ext_index.rs::every_required_field_is_required_and_names_itself` |
| Command effects (`execution.sandbox`) | **macOS/Linux: confined.** Seatbelt via `sandbox-exec`; Landlock via ruleset. Both: reads allowed, writes only under `writable` plus `/dev/null`, no network. **Elsewhere: approval-only.** Default `mode: os`, `writable: ["."]`, `network: false` | `execution.sandbox.mode`; `require: true` denies `host-process` outright — on macOS/Linux it permits commands | `core::sandbox::SandboxPolicy::resolve` and `core::host_process::ProcessRunner::exec` both from one decision `Runtime::open_process_runner` reports at boot | macOS: `host/tests/it/sandbox_seatbelt.rs::a_confined_command_cannot_write_outside_the_workspace`, `::a_confined_command_can_still_write_inside_the_workspace`, `::a_confined_command_cannot_reach_the_network`, `::a_guest_running_a_command_is_confined_too`, `::a_confined_command_can_redirect_to_dev_null`, `::granting_dev_null_does_not_grant_the_device_directory`, `core/src/sandbox_seatbelt.rs::dev_null_is_writable_without_anyone_asking`, `::a_writable_path_reaches_the_profile_symlink_resolved`, `::a_path_that_would_inject_sbpl_is_refused_rather_than_escaped`. Linux: `host/tests/it/sandbox_landlock.rs::a_confined_command_cannot_write_outside_the_workspace`, `::a_confined_command_can_still_write_inside_the_workspace`, `::a_confined_command_cannot_reach_the_network`, `::a_guest_running_a_command_is_confined_too`, `core/src/sandbox_landlock.rs::what_the_backend_writes_is_what_the_wrapper_reads`, `::a_confine_that_cannot_read_its_policy_refuses_rather_than_running`. Both: `core/src/sandbox.rs::os_mode_with_no_backend_becomes_approval_only_and_says_why`. Boot-path runner: `host/tests/it/execution_config.rs::a_config_built_runner_is_confined_by_the_backend_it_resolved`, `::a_confined_config_built_runner_can_still_write_inside_the_workspace` |
| `/dev/null`, for a confined command | **Writable, always, by nobody's configuration.** A sink: a write to it discloses nothing, persists nothing and reaches nothing. Granted because the shell opens `2>/dev/null` *before* running the program, so refusing it means the command never executes — and the error names `/dev/null`, which reads like the sandbox working (#212). Granted as the device, not `/dev` | none — not an operator knob | `core::sandbox_seatbelt::DEV_NULL_RULE`; the matching Landlock `WriteFile` rule in `core::sandbox_landlock::apply` | macOS: `host/tests/it/sandbox_seatbelt.rs::a_confined_command_can_redirect_to_dev_null`, `::granting_dev_null_does_not_grant_the_device_directory`, `core/src/sandbox_seatbelt.rs::dev_null_is_writable_without_anyone_asking`. Linux: `host/tests/it/sandbox_landlock.rs::a_confined_command_can_redirect_to_dev_null`, `::granting_dev_null_does_not_grant_the_device_directory` |
| The `self-extend` distribution | **Moves no boundary, and that is the claim.** It enables `tool-shell` and `execution`, so the agent can compile — with `writable: ["."]`, `network: false` and `require: true`, the same jail every other distribution gets. No toolchain path is granted, no build cache is granted, no `writable` entry leaves the workspace: a `cargo build --target wasm32-wasip2` needs only the workspace, `/dev/null` and the `TMPDIR` in the two rows above. Dependencies are vendored (34 MB, 47 crates) rather than fetched, which is what lets `network: false` and `require: true` both be true (#192) | `make bundle DIST=self-extend`; `scripts/distributions/self-extend/config.yaml` | `core::sandbox::SandboxPolicy` unchanged — the distribution is configuration, not a code path | `host/tests/it/self_extend.rs::the_self_extend_distribution_compiles_to_wasm_inside_the_jail`, `::the_self_extend_distribution_still_refuses_a_write_outside_the_workspace`, `::the_coding_distribution_refuses_the_same_command` |
| Outbound HTTP (`host-http`) | Public only; loopback, private, link-local, unique-local refused. Resolved before decision — one lookup, connection made to classified addresses. **Every redirect hop checked**, up to 10, then refused. `Authorization`, `Cookie`, `Proxy-Authorization` do not survive a hop | Origins in `config.yaml` (`base-url`, `endpoint`) plus `network.allow`. Granted origin trusted by name, not pinned | `core::egress::EgressPolicy::check` called per hop by redirect loop in `http::fetch_within` (sets `max_redirects(0)`). Addresses handed to agent as fixed resolver (`http::Pinned`) | `core/src/egress.rs::loopback_and_private_addresses_are_refused`, `::a_resolved_name_hands_back_the_addresses_it_classified`, `core/src/http.rs::the_pinned_resolver_ignores_the_name_it_is_asked_about`, `host/tests/it/egress_boundary.rs::a_live_local_service_is_not_reachable`, `::every_guest_facing_backend_goes_through_the_policy`, `::a_permitted_origin_cannot_redirect_to_a_refused_one`, `::a_redirect_to_a_permitted_destination_is_followed`, `::credentials_do_not_survive_a_redirect`, `::a_302_becomes_a_get_and_a_307_keeps_the_post` |
| Long-lived subprocesses (`host-process.spawn`) | Denied. Guest holds child only if `execution.long-lived` **names** it — guest picks name, host picks command/args; guest string never becomes program. Empty list grants none; `execution.enabled: true` alone grants none. Confined by same `execution.sandbox` policy. **Killed when instance goes**, including on gateway exit | `execution.long-lived: [ { name, command, args } ]`, one at a time | `core::host_process::ProcessRunner::spawn_long_lived` decides before start, reuses `ProcessRunner::prepared`. Child owned by per-instance `core::tool_host::ToolHost`, killed by `core::host_process::LiveChild`'s `Drop` | `host/tests/it/execution_config.rs::an_unnamed_long_lived_child_is_refused`, `::a_granted_long_lived_child_starts_and_answers`, `::a_long_lived_child_is_confined_like_a_one_shot_command`, `::a_long_lived_child_does_not_outlive_the_runtime` |
| Raw sockets (`wasi:sockets`) | Denied — the linker wires TCP and UDP, every address is refused | none | `WasiCtx`'s `SocketAddrCheck` (deny-all default) | `host/tests/it/sandbox_boundary.rs::a_guest_cannot_open_its_own_socket`, `::a_guest_cannot_open_a_socket_to_a_public_address` |
| The host's environment | None. It holds `OPENAI_API_KEY` (config expands it) and `JAN_KLOD_TOKEN`, so a guest reading it directly is the shortest path to the operator's credentials | none | `WasiCtxBuilder` (`inherit_env` is never called) | `host/tests/it/sandbox_boundary.rs::a_guest_cannot_read_the_hosts_environment` |
| The host's stdin | None. `jan-klod-gateway` runs in a terminal, so this is the user's keystrokes — including an answer being typed at a permission prompt | none | `WasiCtxBuilder::inherit_stderr` (not `inherit_stdio`) | `host/tests/it/sandbox_boundary.rs::a_guest_gets_no_standard_input` |
| Persistence (`host-storage`) | A private map that dies with the process, which is what keeps the permission gate's standing grants run-scoped. Run-scoped, not session-scoped: one namespace per component for the whole run | `persist: true` per instance for durability; `scope: session` on a **tool** instance to key by session as well — interceptors have no such key, deliberately, since a standing grant that stopped crossing sessions would no longer mean "always" (#215) | `core::guest_storage::GuestStorage` — namespaces prefixed with the component id, and by session when a tool asked | `host/tests/it/storage_scope.rs::a_standing_grant_does_not_survive_a_restart`, `::a_namespace_a_guest_can_name_never_reaches_another_components_data` |
| Tool calls | Confirmed, unless the call is on the read-only allowlist (`find`, `fs:read`, `fs:grep`, `git`, `edit:view`, `proc-probe`) | `safe-calls` replaces the allowlist | `interceptor-permission` at `Phase::ToolCall` | `interceptor-permission/src/rules.rs::a_tool_nobody_has_classified_is_confirmed`, `host/tests/it/tool_wiring.rs::an_edit_is_confirmed_before_it_touches_the_file` |
| Content policy (`interceptor-guardrails`) | **Off**, twice over: the instance is disabled in the shipped `config.yaml`, and an *enabled* instance with no rules returns `proceed` — so enabling it is not itself a policy | An `extensions.interceptor.guardrails` entry, with rules under `deny-tool-arguments` (matched against a call's arguments) and `redact` (matched against model output, the final answer, and messages bound for the provider) | The guest, at `tool-call`, `after-response`, `finalize` and `select-context` — **and the host behind it**: a malformed rule set invalidates the whole set, every dispatch reports an internal error, and the host fails closed at `tool-call`, so the guest never runs. A typo stops tool calls rather than leaving a guardrail silently absent | `host/tests/it/guardrails.rs::a_secret_in_the_model_output_never_reaches_the_transcript`, `::a_denied_tool_argument_blocks_with_the_reason_surfaced` |
| Credential files | Skipped by `find` and tree `grep`; explicit read confirmed, never covered by "always" | Pattern names file (`**/.env`); nothing widens read | `guest_fs::hidden_credential`, `interceptor-permission`'s `TouchesCredentials` | `guest-fs/src/lib.rs::credential_files_are_recognised`, `host/tests/it/tool_fleet.rs::a_tree_grep_skips_credential_files` |
| `ext install`'s staging directory | User-private (`0700` unix), named per **call** not process. Component, manifest, both signatures staged **before verification**, read back to check — world-readable drop box at guessable path is wrong. Per-call: `install_from_url` `remove_dir_all`s both ends, so two installs sharing one deleted each other's downloads | none | `core::ext::staging_dir` for name, `core::wasm_cache::ensure_private_dir` for mode | `core/src/ext.rs::two_installs_in_one_process_do_not_share_a_staging_directory` |
| Whether a tool call failed | Host's own verdict; `tool-result` interceptor cannot change it. Guest at `Phase::ToolResult` may rewrite `content`, but `wit/interceptor.wit`'s `tool-outcome` carries no `failed` field — guest's `Replace` would arrive as `failed: false` whatever the truth, denial or trap as success | none | `core::interceptor_host::preserve_tool_result_failed` restores the flag from dispatched state | `core/src/interceptor_host.rs::a_guest_replace_cannot_flip_the_failed_flag`, `::other_decisions_are_left_alone` |
| A parked turn | Ends within heartbeat of client disappearing, or at timeout; default is denial. **Over ACP no timeout**: answer on same pipe, closed pipe detected at once, refuses | `JK_ANSWER_TIMEOUT_SECS` (REST only) | `host::serve::PromptDriver::wait_for_answer`; `host::acp::PipeAsker::ask` (stdio) | `host/tests/it/prompt_disconnect.rs::a_disconnected_client_does_not_hold_the_turn_open`, `host/src/acp.rs::a_closed_pipe_while_parked_refuses_rather_than_hanging` |
| Who may answer a confirmation (`acp`) | **Editor only** through `session/request_permission` — options are gate's own answers. Unreadable answers refuse: `cancelled`, `selected` with no `optionId`, empty, JSON-RPC error, closed pipe. MCP port: nobody can answer, all take default denial | none — port is the grant, subprocess editor spawned | `host::acp::PipeAsker::ask` builds options and maps outcome; `host::acp::AcpDriver` hands answer to permission gate | `host/tests/it/acp.rs::an_editor_that_grants_permission_gets_the_write`, `::an_editor_that_refuses_permission_prevents_the_write`, `host/src/acp.rs::a_permission_request_carries_the_gates_own_options`, `::an_answer_that_cannot_be_read_refuses` |
| The terminal client's ask dialog (Slice 19f) | `default` is **denial** — rendering mistake = security consequence. Selection is `default`'s index, found by search; if absent, no selection, explicit choice required. Answer is byte-identical to chosen entry, carries **notification's session**, not client's. `Esc` closes without answering, prompt stays queued. Second `ask` queues, not stacks. Marks survive `Mode::Mono` (default in text, not only colour) | none — client's rendering of `interceptor-permission`'s answers | `jan-klod-ui`'s `app::App::ask`/`take_answer`/`prompt_move`/`prompt_jump` queue and selection; `blocks::prompt_dialog` renders; `tui.rs`'s `dialog_keys` and `enter_means` route keys | `tui/src/app.rs::the_initial_selection_is_defaults_index_wherever_it_sits`, `::the_answer_is_byte_identical_to_the_chosen_option`, `::the_answer_carries_the_prompts_own_session`, `::a_second_ask_queues_behind_the_first_and_neither_disturbs_the_other`, `::esc_closes_without_answering_and_the_prompt_survives`, `tui/src/blocks.rs::the_dialog_says_what_silence_means_and_names_the_default`, `::monochrome_still_marks_the_default_option_in_text`, `tui/src/tui.rs::esc_closes_the_dialog_without_sending_turn_answer`, `::the_answer_is_sent_with_the_prompts_own_session_not_the_clients` |
| The REST surface | Open when no token is set — loopback-only by default, and a non-loopback bind without a token is warned about at boot | `JAN_KLOD_TOKEN` | `host::serve::authorised` | `host/tests/it/auth.rs::without_a_token_a_turn_is_refused_and_never_reaches_the_agent`, `::an_unauthenticated_caller_cannot_answer_a_permission_prompt` |
| The token inside the GUI window (`jan-klod --gui`) | Shell seeds `JAN_KLOD_TOKEN` into `sessionStorage` — **only when `location.origin` is core's own**, since Tauri script runs in every frame. Off-origin navigation **refused**, handed to system browser — origin check is second line. No token: nothing written, not empty string | `JAN_KLOD_TOKEN` from launching process; no other source or prompt | `jan-klod-gui`'s `seed_token_script` (`location.origin` guard, `serde_json` escaping) and `on_navigation` in `src/gui/src/main.rs` | `src/gui/src/main.rs::the_token_is_seeded_only_on_the_cores_own_origin`, `::no_token_means_nothing_is_written`, `::a_token_with_javascript_metacharacters_is_escaped`, `::origin_drops_the_path_and_keeps_a_non_default_port`, and e2e: `src/gui/tests/smoke.rs::the_window_loads_the_page_and_the_token_is_already_there`, `::with_no_token_the_page_finds_nothing_rather_than_an_empty_string` |
| Compiled component cache (`storage.cache-dir`) | Hit is `Engine::load_code_bytes` handed a file this process didn't compile — deserialized, not re-verified. Wasmtime cache key (bytes + triple + flags + version, hashed) stops stale/foreign artefact reuse but says nothing about write access to the directory | `storage.cache-dir` (path only; default `wasmtime-cache` beside `config.yaml`) | `core::wasm_cache::ensure_private_dir` — `0700` unix, applied before handed to `Config::cache`/`Cache::new` at boot | `core/src/wasm_cache.rs::the_cache_directory_is_created_user_private_on_unix` |

## Two rules that shape all of it

**A grant is written down, and it is narrow.** Every widening above names a thing
— an origin, a variable, a call, a root — rather than flipping a mode.
`network.allow` could have been `allow-local: true`, which would have permitted
the Postgres beside Ollama; `env-passthrough` could have been `inherit-env: true`,
which is the bug it replaced.

**A check whose coverage is a literal will miss the thing added after it.** The
egress guard listed three files implementing `host-http` but there were four.
Three checks on this page missed their own class by enumerating. Where a check
enumerates, it should ask the source rather than carry a list.

**A check inside the sandbox is advice, not a boundary.** `tool-fetch` carries an
SSRF guard that was once the runtime's only defence. It protects a confused model
from a URL the model chose, but says nothing about a component — the thing being
distrusted. The guard stays because confused models are common; the boundary moved
host-side. If a rule can only be enforced by the thing being constrained, it is
not enforced.

**And a boundary in the right place can still check the wrong thing.** The egress
check was host-side and correct about every address class — yet for weeks it was
applied to the URL the caller *named* rather than the destination actually
contacted, because the HTTP client followed ten redirects on its own. A permitted
origin answering `302 Location: http://169.254.169.254/…` reached cloud metadata
with the policy's blessing. The tests never redirected, so nothing disagreed with
the claim ([#107](https://github.com/PromptPasture/jan-klod/issues/107)). *Where*
a check runs is only half of it; a library's defaults are part of the boundary
whether or not anyone chose them.

## Known gaps

- **`host-process` confines the caller, not the command.** The grant is narrow —
  `execution.enabled`, workspace-relative cwd, timeout, output cap, scrubbed
  environment — but the command runs with the user's privileges and can read or
  write anywhere the user can. Closing it needs OS-level sandboxing (Seatbelt on
  macOS, Landlock + seccomp on Linux) — [decision 2](../decisions/2026-09-08-harness-platform-vision/Vision.md#decisions),
  [Phase 15](roadmap.md#phase-15--os-level-effect-sandbox). **Closed on macOS
  (15b) and Linux (15c); open on Windows.** A command's writes are refused by the
  kernel rather than by a prompt. Tests run the same command unconfined first,
  since a failed unrelated command looks exactly like a denial. On Windows: the
  policy reads, mode resolves to `approval-only`, and boot warns. A Windows
  backend is a *spike* rather than an implementation
  ([Slice 15d](roadmap.md#phase-15--os-level-effect-sandbox)).

  Two limits of the macOS half:

  - **Reads are not confined**, only writes and network. A confined command can
    read anything the user can. Narrowing that has a real cost — a command cannot
    read its own toolchain.
  - **A command needing a Mach service fails** on macOS rather than running
    unconfined — `git` and `cargo` reach for some. That is the right direction,
    but it is a rough edge, not a polished sandbox.
  - **Denying network on Linux needs a 6.7 kernel** (Landlock ABI 4). Below that
    the runtime refuses the command rather than reporting a restriction it cannot
    apply, and the error names both ways out.
  - **Both halves are verified by CI on different schedules.** Landlock runs on
    every `main` push; Seatbelt runs in `ci-macos.yml` only when sandbox sources,
    sandbox tests, `wit/host-process.wit` or the workflow change — plus on demand.
    The asymmetry is deliberate: this repository is private, and GitHub bills a
    macOS minute at ten Linux minutes ([#95](https://github.com/PromptPasture/jan-klod/issues/95)).
    A change breaking Seatbelt **without touching the macOS-job paths** lands
    green. `.github/AGENTS.md` records the cost reasoning. `.github/hooks/pre-push`
    runs on the developer's machine, so a **Linux**-only failure is invisible
    before a Mac push — how four `execution_config` tests reached `main` red
    ([#124](https://github.com/PromptPasture/jan-klod/issues/124)).
  - One piece of 15a is **deferred**: a per-turn `Warning` that the command is
    not isolated. Nothing distinguishes a tool using `host-process` from one
    reading files, so the warning could only fire on every turn, teaching people
    to ignore it. It waits for [Phase 16a](roadmap.md#phase-16--capability-manifest--signed-registry)'s
    component-import introspection.
- **A granted origin is trusted by name, not by address.** DNS rebinding is
  closed — resolved addresses are pinned into the connection. A grant skips that
  deliberately: `network.allow` and `config.yaml` origins are the operator saying
  *this endpoint*. Re-deciding where it may point second-guesses them. The pin
  rides on `ureq::unversioned`, which its docs exempt from semver, so ureq
  upgrades must be read rather than merged.
- **The credential-file rule is a name heuristic.** It covers `.env`, `*.pem`,
  `id_rsa` and conventions. It misses `config/production.yaml` holding a database
  URL.
- **A guest with a credential can leak it.** `provider-openai` is given `api-key`
  because it cannot call the API without one, and it can write anything to stderr.
  The boundary protects components that were *not* handed the secret.
- **stderr is shared.** Guests can write misleading lines. Host lines are tagged,
  so this is cosmetic but not nothing.
- **A confirmation answer can be lost.** One `auth.rs` run waited out two full
  timeouts. Twenty-one further attempts have not reproduced it, and three
  structural explanations were ruled out. A lost answer now fails loudly instead
  of silently taking the default, so the next occurrence arrives with diagnostics.
  That is not a fix, and is not claimed as one.

## What this does not claim

Not a defence against a malicious *operator* — anyone who can edit `config.yaml`
can name any root, origin, or variable; that is correct: it is their machine. The
threat model is a misbehaving component, a confused model, or the ordinary
accident of running the agent in the wrong directory.
