---
type: guide
title: Writing an extension
description: From nothing to a working guest — generate a crate, build it, test it, and install it — in Rust or TypeScript, and the one refusal that will otherwise stop you.
tags: [extensions, pdk, wit, component-model, wasm, typescript, guide]
created: 2026-09-12
updated: 2026-09-18
---

# Writing an extension

An extension is a WebAssembly component. The core loads it, provides the host
interfaces it declared, and calls its kind's exported interface. This page covers
generating, building, testing, and installing a guest.

The first three steps are straightforward. The fourth — installing — has a catch
explained in the final section.

The steps below are Rust, which every first-party extension uses. TypeScript
works against the same WIT; read [In TypeScript](#in-typescript) **before** you
pick it, because the choice costs 12.7 MB per component, 326 MB of toolchain,
and ~25 s on every `make gate`.

## 1. Generate the crate

```console
$ make ext-new NAME=tool-greet KIND=tool
```

Pick any name except `tool-hello`, which is the committed example `ext-new` refuses to overwrite.

`KIND` is one of:

| `KIND` | World | What it exports beside `extension-lifecycle` |
|---|---|---|
| `provider` | `provider-world` | `llm-provider` — talks to a model |
| `tool` | `tool-world` | `tool-callable` — something the model can call |
| `interceptor` | `interceptor-world` | `interceptor` — sees and can change a turn |
| `registry-skills` | `skill-registry-world` | `skill-registry` — offers skills |
| `registry-mcp` | `mcp-registry-world` | `mcp-registry` — proxies MCP servers |

There is no `agent` kind. `configuration.md` lists it as a category, but `wit/`
has no agent world and the runtime has no handler — it would never load.

`ext-new` registers the crate in the workspace and Makefile. The next step is build, not setup.

`LANG=ts` generates the same kinds in TypeScript instead — see
[In TypeScript](#in-typescript).

## 2. Build it

```console
$ make extensions
```

Compiles guests to `wasm32-wasip2`, stages them in `ext/`, and generates manifests
from real imports. You don't write a manifest; it's extracted from the component.

Look at what yours declared:

```console
$ cat ext/tool-greet.manifest.toml
```

A fresh tool declares only `host-log` (all the template uses). Add a `host-fs` call
to your code and it appears in the manifest on the next build — but `config.yaml`
must grant it before the host wires it up. Declaration is not permission.

## 3. Test it

Compiling is not the same as working. Test with the host suite:

```console
$ cd src && cargo nextest run -p jan-klod-host --features jan-klod-host/integration generated_guest::
```

`src/host/tests/it/generated_guest.rs` is a template. Copy it, change the three
things marked yours (component, instance id, assertions), and add `mod your_module;`
to `src/host/tests/it/main.rs`. Do not create a new file — one test binary is
intentional because `wasmtime` links statically.

Read the failure table before trusting your copy — an empty harness passes as silently as a working one.

## 4. Enable it

A staged component is inert until `config.yaml` enables it:

```yaml
extensions:
  tool:
    greet:
      enabled: true
```

The host resolves `tool` + `greet` to `ext/tool-greet.wasm`.

## 5. Install from elsewhere

To use a component built outside this repository:

```console
$ jan-klod-gateway ext install ./tool-theirs.wasm
```

**This will refuse, and the refusal is the point:**

```text
jan-klod: no signature at ./tool-theirs.wasm.minisig. Sign it, name the key in
`registry.trusted-keys`, or pass --allow-unsigned with --sha256
```

Installs are verified against minisign signatures from keys in `registry.trusted-keys` —
which ships empty. Every signed install is refused until a trusted key is added.

For now, pin the bytes instead:

```console
$ shasum -a 256 ./tool-theirs.wasm        # sha256sum on Linux
$ jan-klod-gateway ext install ./tool-theirs.wasm --allow-unsigned --sha256 <hex>
```

`--allow-unsigned` requires `--sha256` — both flags together only. A hash is weaker
than a signature (it says these are the bytes you saw, not that anyone vouched for
them) — hence the requirement and why it's the fallback, not the default.

The full command surface:

```console
$ jan-klod-gateway ext list
$ jan-klod-gateway ext install <path.wasm|url> [ext-dir] [--sha256 <hex>] [--allow-unsigned]
$ jan-klod-gateway ext remove <name>
```

Remote sources must be public; URLs are validated before download and on every redirect.

## In TypeScript

The Component Model is language-neutral and this repository proves it rather
than asserting it: `src/extensions/tool-hello-ts` is the TypeScript twin of
`tool-hello`, built against the same `wit/`, dispatched by the same fleet, with
no host-side special case. If you want the proof rather than the instructions,
`src/host/tests/it/polyglot_ts.rs` runs a turn through it.

### Meet the cost first

| | TypeScript | Rust (`tool-hello`) |
|---|---|---|
| component | **12,725,895 bytes** | 54,633 bytes |
| toolchain | **326 MB** of `node_modules` | the cargo toolchain you already have |
| `make gate` | **+20–30 s** | — |

The 12.7 MB is the StarlingMonkey JavaScript engine, which every JS component
carries; it is the price of the language, not of your extension. For scale,
all twenty Rust components together are 3.5 MB.

The gate cost follows from the size: Cranelift compiles every staged component
at boot. Measured back to back on one machine, `make gate` was **129.5 s with
the component staged and 106.3 s without** — and an earlier pair on the same
machine was 120 s against 90 s, so treat the ~25 s delta as the number and the
absolutes as machine noise.

**That cost is paid for a staged component, not a built one.** Removing `jco`
stops the build, not the load — a machine that built it once keeps paying
every gate run until `make clean`.

That is why the toolchain is **not** in `make setup`, why the build skips
itself where the toolchain is absent, and why nothing in `ext/` is committed.

### 1. Install the toolchain

```console
$ npm install -g @bytecodealliance/jco@1.34.0
$ cd src/web && npm ci
```

`jco` is expected on `PATH`; `esbuild` is reused from `src/web`'s pinned
devDependency rather than installed a second time. Both versions are pinned —
`jco` in `versions.mk`, `esbuild` in `src/web/package.json`.

### 2. Generate it

```console
$ make ext-new NAME=tool-greet-ts KIND=tool LANG=ts
```

`LANG=ts` supports `tool` and `interceptor`. The other three kinds are refused
rather than given a template nobody has written by hand — a provider or a
registry in JavaScript is possible, and until someone wants one, a template is
something to keep correct for free.

You get `package.json` and `src/guest.ts`. No `Cargo.toml`, and **no generated
bindings to import**: the component is built *from* your module against the
WIT, so the exports are hand-written to match and the componentize step is what
checks them. `jco` lowers WIT's kebab-case to lowerCamelCase — interface
`tool-callable` becomes the export `toolCallable`, field `arguments-schema`
becomes `argumentsSchema`.

The generated stub returns real values rather than throwing, so it
componentizes and runs on the first build, the same rule the Rust templates
follow.

### 3. Build it

```console
$ make -C src/extensions ts-guest
```

`make extensions` runs this too. Without the toolchain it says what is missing
and exits 0 — a skip, not a silent nothing:

```text
skipping tool-hello-ts: jco is not on PATH
  install it with: npm install -g @bytecodealliance/jco@1.34.0
```

Your guest's world comes from its name prefix — `tool-*` builds against
`tool-world`, `interceptor-*` against `interceptor-world` — so `ext-new` adds
it to `TS_GUESTS` and nothing else needs editing.

### The one flag that is not an optimisation

The build passes `--disable http --disable fetch-event`. Without it
StarlingMonkey provides `fetch`, the component imports `wasi:http`, and the
host **refuses to instantiate it**:

```text
component imports instance `wasi:http/types@0.2.10`, but a matching
implementation was not found in the linker
```

That refusal is correct and must stay. A guest reaches the network through
`host-http`, which is where the egress policy lives; a linker that also offered
`wasi:http` would hand every guest a way around it. If you need outbound
requests, import `host-http` and be granted it — do not re-enable `fetch`.

### Its manifest is its world, not its need

Step 2 above says a fresh Rust tool declares only `host-log`, because
`wit-bindgen` emits an import for what your code uses. **ComponentizeJS emits
an import for every interface in the world**, whether you call it or not. Two
manifests, both generated from the artifact, both for a guest that does
nothing but return a greeting:

```toml
# ext/tool-hello.manifest.toml    # ext/tool-hello-ts.manifest.toml
capabilities = [                  capabilities = [
    "host-log",                       "host-config",
]                                     "host-fs",
                                      "host-http",
                                      "host-log",
                                      "host-process",
                                  ]
```

The TypeScript twin claims filesystem access, process execution and outbound
HTTP. It uses none of them.

This is not a hole — the host grants capabilities from `config.yaml`, and the
manifest is checked *against* the grant rather than being one, so a guest that
declares `host-fs` and is granted nothing still gets nothing. What it costs is
the signal: **do not read a JavaScript guest's `capabilities` as a statement
of need**, yours or anyone's. Narrowing it is [#210](https://github.com/PromptPasture/jan-klod/issues/210).

### Testing, enabling and installing are unchanged

Test it with the same host suite, enable it with the same `config.yaml` key
(`tool: greet-ts:` resolves to `ext/tool-greet-ts.wasm` by the same naming
rule), and install it with the same signature requirement. The host does not
know which language your component was written in, which is the whole point.

## Where to look next

- [Contracts](../concepts/contracts.md) — the WIT interfaces, and the version rule your crate is built against.
- [Configuration](../concepts/configuration.md) — every key, including what each capability's grant looks like.
- [Security model](../concepts/security-model.md) — what your extension is and is not given, and the test that proves each one.
