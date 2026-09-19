---
title: Vision — The Enterprise Box
description: The second half of the product. The foundation is nearly finished and the box does not exist: of the themes an enterprise buyer asks about, none is tracked. This record fixes where enterprise capability lives (extensions, except what only the host can hold), answers the parked memory question, and phases the work as Phases 26–31 — trusted delivery, secrets, identity, audit, batteries, distribution.
---

# Vision — The Enterprise Box

[Harness as a Platform](../2026-09-08-harness-platform-vision/Vision.md) positioned jan-klod as an
agent runtime — kernel, distributions, clients — and phased the runtime half as Phases 13–18. That
half is essentially built. This record is about the other half, which that vision named in one
column of one table and never phased:

> **User** | one binary + a distribution (curated extensions + config) | Linux distros; Configurator

A distribution is where the product meets someone who does not want to configure anything. Today
there are four of them, they differ by which guests they carry, and not one of them answers a
question an enterprise review asks.

## Goal

Two products out of one repository, stated by the owner:

- **The foundation** — configurable any way the user wants, with nothing agent-specific welded into
  the kernel. This is the Phase 13–25 line of work and it is nearly done: 13–18 are the phases
  [Harness as a Platform](../2026-09-08-harness-platform-vision/Vision.md) phased, and 19–25 were
  added to the roadmap afterwards as the same half of the product.
- **The box** — a turnkey install for someone who will not configure anything, defined by two
  pillars:
  - **It passes review.** Authentication and RBAC on the inbound surfaces, an audit trail and
    session export, secrets management, policy and guardrails, egress control, air-gap, signed
    delivery. *"You can bring it into a company."*
  - **Batteries are included.** Providers, memory, skills, MCP servers, tools at the level of a
    modern coding agent. *"Nothing has to be assembled by hand after install."*

## Context — the foundation holds, the box does not exist

The kernel earns its ground rule. `src/core/src/conductor.rs` is a ReAct state machine that hands
every decision to an interceptor guest through `src/core/src/intercept.rs`; there is no model name,
no endpoint and no agent policy in it. Provider fallback is try-in-order. The only production code
that names a specific extension is the `ext install` / `ext search` pair in `native_tools.rs`, which
is host-side because it writes. *Zero agent behaviour* is true.

What is missing is on the other side, and it is missing from the tracker as well as from the code:

- **Nothing has ever been released.** `git tag` returns nothing. `scripts/install.sh` has nothing to
  fetch. Phase 16 built signing and a registry index and then shipped every distribution's
  `config.yaml` with `registry.trusted-keys` empty and no `registry.url` — the root `config.yaml`
  names one, the four under `scripts/distributions/` do not — so `ext search` and `ext install`
  refuse by construction
  ([#93](https://github.com/PromptPasture/jan-klod/issues/93)). The distribution machinery is
  complete and inert.
- **There is one shared bearer token.** `authorised()` in `src/host/src/serve.rs` compares a secret;
  it does not establish a principal. Every caller sees every session.
- **Telegram checks nothing.** `src/host/src/telegram.rs` turns every chat id into a session and
  answers every sender who finds the bot; there is no allowlist to configure. `headless-chat` is
  reached over nothing else.
- **Secrets are `${VAR}` expansion** (`src/config/src/lib.rs`) and nothing systematically keeps a key
  out of the event log, an SSE frame or a rebuilt transcript.
- **The log records everything and nothing reads it out**
  ([#186](https://github.com/PromptPasture/jan-klod/issues/186)).
- **`docs/concepts/architecture.md:27` promises structured logging, Prometheus and OpenTelemetry.**
  The implementation is `eprintln!`. That is a false claim in a document a reviewer will read.
- **Four distributions, none enterprise-shaped** — and they do not agree on how many there are:
  `scripts/install.sh:35` knows four, the landing page's table (`pages/index.md:44`) lists three and
  omits `self-extend`. That is the drift
  [#178](https://github.com/PromptPasture/jan-klod/issues/178) already tracks.

Batteries are the same story from the other end. Measured against the capability groups of
[`pi-onboard`](https://github.com/PromptPasture/pi-onboard) — the owner's own audit-first installer
for Pi, and the closest thing to a specification for this box — safer editing and permission
prompts ship in the `coding` distribution; planning, web fetching and MCP exist as guests
(`tool-plan`, `tool-fetch`, `registry-mcp`) that no distribution's `guests` list carries, so they are
built and not shipped. **Persistent memory, web search, scheduled tasks and sub-agents do not
exist.** `src/core/src/delegate.rs` is written and unit-tested and never instantiated,
because `CATEGORIES` at `src/core/src/lib.rs:259` is `["provider", "interceptor", "registry",
"tool"]` and `agent` is not among them.

## Where enterprise capability lives

The temptation is to answer each review item with host code, and that would spend the foundation to
buy the box. The rule this record fixes:

**Enterprise capability ships as an extension. The host gains only what the host is structurally the
only place for.** There are exactly four of those, and each has a reason that is not preference:

| Host-side | Because |
|---|---|
| Principal extraction | Only the host accepts the connection; identity asserted by a guest, or by a client, is not identity |
| Secrets backends | Keychain, libsecret and DPAPI are OS interfaces; a sandboxed guest cannot reach them, and should not |
| Event-log integrity | A hash chain has to be computed at append, inside the one writer |
| Structured logging | The subscriber has to exist before any guest is instantiated |

Everything else is a guest: RBAC is an interceptor over a principal the host establishes, session
export is an extension over `host-agent`, memory is a guest over `host-storage`, personas are an
interceptor, every provider is a guest. This keeps the claim the foundation makes — that a third
party can replace any of it — true of the enterprise features too.

## Decisions

1. **Enterprise capability is extensions, with four host-side exceptions** (the table above). A
   proposal to add host code outside those four has to argue the structural case first.
2. **The box is a distribution, not a fork.** `scripts/distributions/enterprise/` is one more entry
   of the pair that already defines a distribution — a guest list and a `config.yaml` — and it is
   held to the existing agreement test between the definitions, the installer and the release
   workflow. → **Phase 31.**
3. **Trusted delivery comes first.** An unsigned, unreleased box is not a box, and every later phase
   ships through the same channel. → **Phase 26.**
4. **Identity comes from the transport credential, never from a client's claim**, and the session
   store is where it binds. RBAC is then an ordinary interceptor reading a principal out of the turn
   context. Telegram is the one surface whose transport credential is not ours: the Bot API asserts
   the sender's id over the bot token, so `telegram:<id>` is the principal, and an allowlist of those
   ids in the distribution's config is the check — empty by default, so an unlisted sender is refused
   before a session exists. → **Phase 28.**
5. **Secrets get a capability; redaction gets one boundary.** `host-secrets` is manifest-gated like
   every other capability, and redaction is enforced at `PersistingSink`, the single point every
   persisted and streamed string passes through — not scattered across the call sites that happen to
   print. Redaction being host-side is not a fifth exception to the rule above: persistence is
   host-side by a standing rule, so the boundary it is enforced at is host-side by consequence. What
   it redacts stays extension-shaped — the patterns are the `interceptor-guardrails` rule set, which
   is already data. → **Phase 27.**
6. **Audit export is an extension over `host-agent`; integrity is a hash chain the host appends.**
   This is [#186](https://github.com/PromptPasture/jan-klod/issues/186)'s own shape and it is right:
   the format a company wants is the format it wants, and that is a guest's problem. → **Phase 29.**
7. **Curated memory: ship the episodic half, decline the curation half.** The roadmap parked this as
   *decide if before how*. The answer is: a thin `tool-memory` guest storing and recalling facts over
   the `host-storage` namespace that already exists, with no new contract, and remembering *across*
   sessions: `host-storage` is run-scoped by default and `scope: session` is the opt-in, and
   `tool-memory` does not opt in, because a memory that ends with the session adds nothing the
   context window does not already hold. Once 28b binds sessions to a principal, the namespace
   follows the principal. Semantic search, consolidation and working/episodic tiering stay out —
   they belong to an MCP server or a third party, and `registry-mcp` already reaches both.
   → **Phase 30.**
8. **Observability: retire the false claim now, ship structured logging, defer the rest.**
   `architecture.md:27` is corrected in the same change that adds a `tracing` subscriber emitting
   JSON. Prometheus and OpenTelemetry wait for an operator who asks for them by name. → **Phase 29.**
9. **Skills ship as content; MCP servers ship as configuration.** The owner's list of batteries names
   both, and the first draft of Phase 30 shipped only their registries. A first-party skill set —
   `SKILL.md` files delivered inside the distribution archive and signed with it — is content the box
   can carry. Third-party MCP servers are binaries from other projects' package managers, which the
   signed channel cannot vouch for, so the box carries the *list*: which servers it expects, with
   their commands, as `registry-mcp`'s `servers` configuration, and `setup` reports which are present.
   → **Phase 30g, Phase 31a.**

Standing rules are unchanged: the core is Rust and holds mechanism only, first-party extensions
default to Rust, storage stays host-side, capabilities are default-deny and granted per extension
through the manifest.

## Phases

Order is by dependency, and the [roadmap](../../concepts/roadmap.md) carries their state. Phases 26
and 27 are independent of everything and of each other; 28 lands after them because it is the one
that touches files Phase 21's rename will move; 29 and 30 each have a slice that waits on Phase 22's
`host-agent`; 31 consumes all of them.

**Phase 21 does not block Phase 28.** The crate split is in progress and the critique of an earlier
draft of this record read it as a prerequisite. It is not: transport already lives in
`jan-klod-host` — `architecture.md` says so and the Phase 21 row confirms it — so identity work has a
crate to land in today. What 21e ([#183](https://github.com/PromptPasture/jan-klod/issues/183))
changes is the package *name*. That is a merge-order concern, not a dependency.

### Phase 26 — Trusted delivery

The signing key exists, is published, and the shipped config trusts it; a release is tagged; the
whole thing installs with no network.

- **26a** — a minisign keypair, its public half committed, its private half a CI secret.
- **26b** — the release carries `sbom.cdx.json` and a `.minisig` beside every archive and component.
  Today `ci.yml` generates the SBOM and keeps it as a workflow artifact, `release.yml` does not
  publish it, and `make sbom` covers the Rust crates only — the npm dependencies of `src/web` have to
  be in it before it is the release's bill of materials.
- **26c** — `v0.1.0` is tagged and `release.yml` publishes all distributions per platform.
- **26d** — the shipped `config.yaml` names a default `registry.url` and lists the published key in
  `registry.trusted-keys`, so `ext install <name>` works without `--allow-unsigned`.
- **26e** — an offline bundle is proven: extract, `verify`, install an extension from a `file://`
  index, run a turn.

**Exit gate:** on a machine that can reach nothing but a self-hosted model endpoint, an operator
verifies the archive against the published key, installs an extension by name from the bundled
index, and runs a turn — and a tampered archive is refused with the reason named.

### Phase 27 — Secrets, and one place redaction happens

A key is fetched through a capability instead of read out of the environment into a config string,
and a secret that reaches the model's output does not reach the log.

- **27a** — `wit/host-secrets.wit` with `get(name) -> result<string, secrets-error>`, where
  `secrets-error` is `not-found`, `denied` or `backend`, in the shape `host-config.wit`'s
  `config-error` already has; an environment-backed implementation; and the manifest cross-check
  that already refuses undeclared imports.
- **27b** — macOS Keychain backend.
- **27c** — Linux libsecret backend, with the environment as the fallback when D-Bus is absent.
- **27d** — redaction at `PersistingSink`, its patterns read from the `interceptor-guardrails` rule
  set that is already data, and failing closed on its own terms: a malformed redaction set refuses
  to start the host rather than persisting in the clear. That rule has to be written here because
  the dispatcher does not supply it (see Risks). `docs/concepts/security-model.md` gains the row for
  what redaction catches and what it cannot.

**Exit gate:** a provider reads its key through `host-secrets`, the model echoes that key verbatim,
and neither the event log, nor a live SSE client, nor a transcript rebuilt after a restart contains
it — while a second extension that did not declare the capability cannot read the key at all. A
malformed redaction rule set refuses to start, naming the rule.

### Phase 28 — Identity at the boundary

The host knows who is asking, sessions belong to someone, and a policy guest can refuse on that
basis.

- **28a** — the inbound credential resolves to a principal in the `serve.rs` guard; an unauthenticated
  request is still refused, an authenticated one now carries a name. The mechanism is a table, not a
  token format: `serve.principals` maps a name to a token (or to the environment variable holding
  one), and today's single `JAN_KLOD_TOKEN` is the principal `operator`, so an existing install
  changes nothing. `security-model.md`'s REST row records that one token means one principal until an
  operator lists more.
- **28b** — sessions are scoped to their principal in `jk-session`; listing returns one principal's
  sessions and reading another's is refused, on every surface that has a credential to check.
- **28c** — the principal reaches the interceptor context as an optional field on `user-turn` in
  `wit/interceptor.wit`; guests that do not read it are unaffected.
- **28d** — `interceptor-rbac`, a reference guest with roles as configuration data, off by default.
  It composes with `interceptor-permission`'s per-session grants as two gates in series — both have
  to admit — and that rule is written into `security-model.md`, not left to be inferred. `ext install`
  is a tool call, so the same guest gates it at `tool-call`; `setup`, `serve` and a config reload are
  operator commands on the host and are the operating system's to protect, not RBAC's.
- **28e** — Telegram: the sender id the Bot API asserts becomes the principal `telegram:<id>`, and
  `headless-chat` ships an empty allowlist that refuses every sender until an operator lists one.
  Lands in `src/host/src/telegram.rs` if 22b
  ([#185](https://github.com/PromptPasture/jan-klod/issues/185)) has not yet moved the bot into a
  guest, and in the guest's config if it has — the rule is the same either way.

**Exit gate:** two principals each run a turn; each sees only their own sessions; the RBAC guest
admits one and stops the other at `before-loop` rather than at the tool; and the refusal names the
principal in the record. An unlisted Telegram sender is refused before a session exists.

### Phase 29 — An audit trail that survives a review

- **29a** — each event row carries the previous row's digest, and `verify_chain` rebuilds it; an
  edited payload or a reordered pair breaks it. The `security-model.md` row says what the chain
  detects — edits by anything that is not the writer — and what it does not.
- **29b** — session export as an extension over `host-agent`, emitting JSONL of the raw events and
  respecting 27d's redaction. *Waits on Phase 22.*
- **29c** — `architecture.md:27` stops promising what the code does not do, and a `tracing`
  subscriber emits structured JSON in the same change.
- **29d** — an optional interceptor recording the permission decision per tool call.

**Exit gate:** a ten-turn session with tool calls, a denial and a follow-up is exported to JSONL by
an extension, re-imported to the identical transcript, its chain verifies, a single altered row
breaks the chain — and no document claims observability the binary does not have.

### Phase 30 — The batteries

Ordered cheapest-useful-first, and the three that close the gap against the `pi-onboard` checklist
come first among those that can.

- **30a** — `tool-memory`: store and recall over `host-storage`, run-scoped so a fact stored in one
  session is recalled in the next, keyed by principal once 28b exists, no new contract. This is
  decision 7, built.
- **30b** — `tool-web-search` over `host-http`, its provider and key configuration.
- **30c** — `interceptor-persona`: personality as data, read from config at `before-loop`.
- **30d** — sub-agents: `agent` becomes a category with a world and a boot arm, and
  `src/core/src/delegate.rs` is finally instantiated. *Waits on Phase 22.*
- **30e** — scheduled tasks. *Waits on Phase 22, and on the clock question below.*
- **30f** — the provider fleet: Bedrock and Vertex as their own guests, since SigV4 and Google OAuth
  are not a base-URL change; self-hosted OpenAI-compatible endpoints already work through
  `provider-openai`'s configurable `base_url` and need documentation, not code.
- **30g** — the curated skill set: first-party `SKILL.md` files for what a coding agent is asked for
  daily (commit, review, plan at least), delivered inside the distribution archive. `registry-skills`
  reads `.agents/skills` in the workspace today and nothing else, so the slice gives it a second,
  distribution-level directory that the workspace one overrides — the catalogue is not empty on
  first run, and a team's own skills still win. This is decision 9, built.

**Exit gate:** a session in the enterprise distribution stores a fact and a *later* session recalls
it; a session searches the web, delegates to a sub-agent, answers under a named persona and runs a
shipped skill — with no extension installed by hand and no `config.yaml` edited after install.

### Phase 31 — The enterprise distribution and the guided first run

- **31a** — `scripts/distributions/enterprise/`: guardrails on, sandbox `require: true`, egress
  pinned, the registry trusting the published key, audit export enabled, the Telegram allowlist
  empty, the Phase 30 guests and the three built-but-unshipped ones (`tool-plan`, `tool-fetch`,
  `registry-mcp`) in its `guests` list, the MCP servers it expects named in `registry-mcp`'s
  `servers`, and the 30g skill set inside the archive. Every one of these is a knob an earlier phase
  built or that `src/core/src/egress.rs` already holds — 31a *configures*, it does not implement.
- **31b** — `jan-klod-gateway setup` audits the current state and prints what would run. It writes
  nothing. It is a host-side subcommand rather than a guest, and for a structural reason rather than
  convenience: it is the thing that decides which guests load, so it has to run before there are any.
- **31c** — capability groups, selected and merged by union, named for what they are for.
- **31d** — per-group configuration, in the three strategies `pi-onboard` proved sufficient: nothing,
  manual, or a walked-through prompt.
- **31e** — preview, then an atomic write; a rerun with the same answers changes no bytes, and a
  hand-edit between runs is detected and asked about rather than overwritten.
- **31f** — the first-run walkthrough in `README.md` and the quickstart, every command run rather
  than transcribed.

**Exit gate:** a fresh machine reaches a running, signed, audited enterprise install without opening
`config.yaml` once — and a second `setup` with the same answers writes nothing.

## What this record declines

Each of these was proposed during the research for it and is refused, so that it is refused once:

- **Identity in the protocol handshake.** A client that asserts its own identity authenticates
  nothing; the credential the transport already carries is the only honest source.
- **A REST export endpoint beside the export extension.** Two implementations of one format, and the
  reason [#186](https://github.com/PromptPasture/jan-klod/issues/186) chose an extension was that the
  format is not ours to fix.
- **Opaque secret handles with callback access.** The guest has to materialise the value to put it in
  an `Authorization` header; a handle buys ceremony, not confinement.
- **Prometheus and OpenTelemetry.** Decision 8. Structured logging first, the rest when asked for.
- **A `pi-onboard` package bridge.** It would couple our release to another project's package format
  for a mapping a distribution already expresses.
- **Curated memory** — semantic search, consolidation, working/episodic tiering. Decision 7.
- **New interceptor phases.** The nine dispatched phases carry every decision these six phases need;
  the closed enum is a contract, and contracts change when something cannot be expressed, not before.
- **A memory that ends with the session.** Decision 7: `tool-memory` is run-scoped, then per
  principal. `scope: session` exists for a permission gate's standing grants; a memory has no use for
  it.
- **Vendoring MCP servers.** Decision 9: other projects' binaries cannot pass through the signed
  channel, so the box names the servers it expects and `setup` finds them.

## Risks

- **Phase 26 is blocked on a human, not on code.** The keypair has to be generated and the secret
  configured by the owner; nothing downstream can start until it is.
- **Two phases wait on Phase 22, which has not begun.** 29b and 30d/30e are the slices; 29a, 29c,
  30a–30c and 30f are not, and are sequenced first for that reason.
- **Per-principal state meets per-run permission grants.** `always allow` is already per-session
  ([#232](https://github.com/PromptPasture/jan-klod/issues/232)); adding a principal gives the same
  decision a second scope, and the two have to be documented as layers or they will be confused.
- **Redaction is lossy and silent when wrong.** A pattern that does not match removes nothing and
  says nothing. The guardrails rule set fails closed on a malformed set only where the dispatcher
  does: `src/core/src/intercept.rs` blocks on an interceptor error at `tool-call` and proceeds at
  every other phase, which is where the guest redacts text today. So the redaction 27d moves to
  `PersistingSink` cannot inherit a fail-closed rule — it has to state one, and 27d does.
- **A hash chain proves less than it looks like.** The host owns the database; the chain detects
  edits by anything that is not the writer, and that limit belongs in the security model row rather
  than in a sales sentence.
- **Windows.** The OS sandbox is still deferred ([#49](https://github.com/PromptPasture/jan-klod/issues/49)),
  so an enterprise install there runs approval-only and `require: true` refuses execution outright.
  The box ships on macOS and Linux and says this plainly rather than degrading quietly.

## Open questions

- **Scheduled tasks need a clock.** 30e can poll on turn boundaries through `host-agent`, or `wit/`
  can gain a `host-clock` with `now()`. Polling is cheaper and worse; the decision belongs to the
  slice that builds it.
- **Vault and cloud secret managers**: a further `host-secrets` backend, or a sidecar that populates
  one of the existing backends? Lean: sidecar, because it is what operators already run.
- **Key rotation.** Phase 26 creates one key and no way to retire it. What a compromise looks like is
  unanswered and should not block the first release.
- **Which capability groups, and how many.** `pi-onboard` has nine; jan-klod's extensions do not map
  one-to-one, and a group that maps to nothing is a lie in a menu.
- **Where the web Configurator fits.** [`configurator.md`](../../concepts/configurator.md) plans a
  Spring-Initializr-style UI and Phase 31 builds a CLI wizard; both select capability groups and emit
  an archive. Lean: one group definition, two renderers, the CLI first — but the record does not
  decide it, and that document still describes only the UI.
- **Per-extension egress.** `src/core/src/egress.rs` is a global policy plus named origins. An
  extension granted `host-secrets` still reaches every address that policy allows, which is not how
  any other capability works. Phase 31 pins the global policy; whether egress becomes per-extension
  is unanswered.
- **Whether `enterprise` is one distribution or a posture applied to others.** A company that wants
  the coding agent *and* the compliance posture should not have to choose.
