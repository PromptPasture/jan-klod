# Storage is not an extension

**2026-08-10** — retires the `memory-store` contract and the `store-*` component
family. Persistence stays where it has always actually been: host-side, in the
core, configured by a top-level `storage:` block.

## What was there

`config.yaml` had an `extensions.store` category with three instances —
`memory`, `sqlite`, `postgres`. `docs/concepts/architecture.md` listed
`store-sqlite`, `store-postgres` and `store-supabase` as components implementing
`memory-store.wit`. `wit/memory-store.wit` defined a seven-function interface and
a `store-world`. One component existed: `store-memory`, a Rust guest with an
in-memory map.

The core called none of it. `store-memory.wasm` was resolved at boot, taken
through `init` → `start`, and then never invoked again — its `memory-store`
exports had no caller anywhere in the host. The only thing the `store` category
did was carry `store.sqlite.path`, which `Runtime::open_store` read to decide
where to put the core's own `rusqlite` database.

So the category advertised a swappable backend family. Three of the four names
had no implementation, and the fourth was inert.

## Why it stays host-side

The same page that listed the component family also stated the design intent, and
the intent was right:

> The persistent store is a **host-side capability** … it is *not* SQLite-in-wasm
> (which the Go MVP confirmed does not work).

Two reasons, and they are the ones that matter for this project specifically:

- **The sandbox has no filesystem.** A store guest would need `host-fs` granted
  back to it. The point of the boundary is that a component gets the capabilities
  it needs and no others; a component whose entire job requires the broadest
  capability the host has is a component fighting the design.
- **The transcript is the most sensitive thing the runtime holds.** Every
  message, in one place. "Swap your storage backend" means "hand your whole
  conversation history to a third-party component". That is a plugin point nobody
  asked for, bought with the guarantee the runtime exists to make.

Postgres, if it is ever wanted, is a second host backend behind the same `Store`
type — an enum, not a WIT world.

## What "trusts nothing it runs" does and does not mean

It does not mean everything must be a component. It means the core grants
nothing implicitly, and every capability crosses a typed, mediated boundary.
Storage satisfies that in the direction that matters: guests reach it through
`host-storage`, which is **granted** (`persist: true`, default off) and
**namespaced** to the calling component, so a guest cannot name a peer's keys or
a session transcript.

Making the store itself a guest would not have added a boundary. It would have
moved core state behind one.

## Changed

- `wit/memory-store.wit`, `src/extensions/store-memory/` — deleted.
- `extensions.store` → top-level `storage: { path }`; absent means in-memory.
- `boot_rank` loses its `store` tier; `Runtime::open_store` reads `storage.path`.
- `host-storage.wit`, `contracts.md`, `architecture.md`, `wit/README.md`,
  `development-setup.md` — corrected to describe what exists.

`docs_match_config.rs` enumerates the categories the runtime instantiates, so a
`store:` block reappearing in `config.yaml` now fails the suite rather than
sitting inert for a month.
