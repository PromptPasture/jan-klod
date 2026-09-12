---
type: guide
title: Writing an extension
description: From nothing to a working guest — generate a crate, build it, test it, and install it — and the one refusal that will otherwise stop you.
tags: [extensions, pdk, wit, component-model, wasm, guide]
created: 2026-09-12
updated: 2026-09-12
---

# Writing an extension

An extension is a WebAssembly component. The core loads it, hands it the host
interfaces it declared, and calls the one interface its kind exports. This page
goes from nothing to a guest that is generated, built, tested and installed.

Three commands do the first three of those. The fourth has a catch, and it is
the reason the last section exists.

## 1. Generate the crate

```console
$ make ext-new NAME=tool-hello KIND=tool
```

`KIND` is one of:

| `KIND` | World | What it exports beside `extension-lifecycle` |
|---|---|---|
| `provider` | `provider-world` | `llm-provider` — talks to a model |
| `tool` | `tool-world` | `tool-callable` — something the model can call |
| `interceptor` | `interceptor-world` | `interceptor` — sees and can change a turn |
| `registry-skills` | `skill-registry-world` | `skill-registry` — offers skills |
| `registry-mcp` | `mcp-registry-world` | `mcp-registry` — proxies MCP servers |

There is no `agent` kind. `configuration.md` lists `agent` among the config
*categories*, but `wit/` has no agent guest world and the runtime has no arm
for one — a crate generated for it would compile against nothing and never
load. The category list documents a naming scheme, not a set of implemented
kinds.

`ext-new` does more than write files: it adds the crate to the extensions
workspace and to the Makefile's `GUESTS`, then formats it. That is why the next
step is a build and not more setup.

## 2. Build it

```console
$ make extensions
```

This compiles every guest to `wasm32-wasip2`, stages them in `ext/`, and
**generates each one's manifest from the component's real imports**. You do not
write a manifest. You cannot make one claim a capability the component does not
import, because it is read out of the artefact rather than written beside it.

Look at what yours declared:

```console
$ cat ext/tool-hello.manifest.toml
```

A freshly generated tool declares `host-log` and nothing else, because logging
is all the template uses. Call `host-fs` from your `invoke` and `host-fs`
appears here the next time you build — and `config.yaml` still has to grant it
before the host will wire it up. Declaring is not being granted.

## 3. Test it

The generated crate compiles. That is not the same as working, and the host
suite is where the difference shows:

```console
$ cd src/core && cargo nextest run -p jan-klod-host --features jan-klod-host/integration generated_guest::
```

`src/core/host/tests/it/generated_guest.rs` is written to be copied. Its module
docs name the three things that are yours — the component, the instance id, and
the assertions — and say the rest is harness. It loads the component and calls
it directly, with no `Runtime`, no config and no provider, because while you are
writing a guest a whole agent tells you less and takes longer to fail.

Copy it, change those three things, and add `mod your_module;` to
`src/core/host/tests/it/main.rs`. **Not a new file beside it**: that is one test
binary on purpose, because `wasmtime` links statically and every extra
integration target is another multi-gigabyte link.

That file also carries a table of how it is known to fail. It is worth reading
before you trust your copy of it: a harness that loads nothing passes exactly as
quietly as one that loads a working component.

## 4. Enable it

A staged component does nothing until `config.yaml` names it:

```yaml
extensions:
  tool:
    hello:
      enabled: true
```

The category and the name are how the host finds the component — `tool` +
`hello` resolves to `ext/tool-hello.wasm`.

## 5. Install one somebody else built

Everything above builds a guest inside this repository. To take a component
from elsewhere:

```console
$ jan-klod-gateway ext install ./tool-theirs.wasm
```

**This will refuse, and the refusal is the point.** An install is verified
against a minisign signature over *both* the component and its manifest, from a
key named in `registry.trusted-keys` — and that list ships **empty**. So until a
key is published, every signed install is refused because there is nobody
trusted to have signed it.

The way through today is to say plainly that you are not verifying a signature,
and to pin what you are installing instead:

```console
$ shasum -a 256 ./tool-theirs.wasm
$ jan-klod-gateway ext install ./tool-theirs.wasm --allow-unsigned --sha256 <hex>
```

`--allow-unsigned` **requires** `--sha256`. That is deliberate: waiving the
signature and waiving the digest would land a component with no evidence at all,
so the flags do not compose that way. A digest is weaker than a signature — it
says the bytes are the ones you looked at, not that anyone vouched for them —
which is why this is the fallback and not the default.

The full command surface:

```console
$ jan-klod-gateway ext list
$ jan-klod-gateway ext install <path.wasm|url> [ext-dir] [--sha256 <hex>] [--allow-unsigned]
$ jan-klod-gateway ext remove <name>
```

A remote source has to be a public address, refused before a byte moves and
re-checked on every redirect hop.

## Where to look next

- [Contracts](../concepts/contracts.md) — the WIT interfaces, and the version rule your crate is built against.
- [Configuration](../concepts/configuration.md) — every key, including what each capability's grant looks like.
- [Security model](../concepts/security-model.md) — what your extension is and is not given, and the test that proves each one.
