# Storage is not an extension

**2026-08-10** — retires the `memory-store` contract and the `store-*` component
family. Persistence stays where it has always actually been: host-side, in the
core, configured by a top-level `storage:` block.

## What was there

`config.yaml` had `extensions.store` with three instances: `memory`, `sqlite`, `postgres`. Docs listed `store-sqlite`, `store-postgres`, `store-supabase` as components implementing `memory-store.wit`, but only `store-memory` existed (a Rust guest with an in-memory map).

The core never called it. `store-memory.wasm` initialized at boot but never executed again—its exports had no callers. The category's only effect: `Runtime::open_store` read `store.sqlite.path` to locate the database.

The category advertised swappable backends; three had no implementation, the fourth was unused.

## Why it stays host-side

The design intent was correct:

> The persistent store is a **host-side capability** … it is *not* SQLite-in-wasm
> (which the Go MVP confirmed does not work).

Two critical reasons:

- **The sandbox has no filesystem.** A store guest needs `host-fs`, violating the principle that components receive only required capabilities.
- **The transcript is the most sensitive runtime asset.** "Swap your backend" means handing conversation history to untrusted code—a plugin point nobody needs.

Postgres, if it is ever wanted, is a second host backend behind the same `Store`
type — an enum, not a WIT world.

## What "trusts nothing it runs" does and does not mean

It does not mean everything must be a component. It means the core grants no implicit capabilities; every crossing is typed and mediated. Storage satisfies this: guests reach it through `host-storage` (granted via `persist: true`, namespaced to each component, preventing peer key or transcript access).

Making the store a guest wouldn't add a boundary—it would move core state behind one.

## Changed

- `wit/memory-store.wit`, `src/extensions/store-memory/` — deleted.
- `extensions.store` → top-level `storage: { path }`; absent means in-memory.
- `boot_rank` loses its `store` tier; `Runtime::open_store` reads `storage.path`.
- `host-storage.wit`, `contracts.md`, `architecture.md`, `wit/README.md`,
  `development-setup.md` — corrected to describe what exists.

`docs_match_config.rs` enumerates the categories the runtime instantiates, so a
`store:` block reappearing in `config.yaml` now fails the suite rather than
sitting inert for a month.
