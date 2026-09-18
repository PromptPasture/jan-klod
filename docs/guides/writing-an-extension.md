---
type: guide
title: Writing an extension
description: From nothing to a working guest — generate a crate, build it, test it, and install it — in Rust, TypeScript or Python, and the one refusal that will otherwise stop you.
tags: [extensions, pdk, wit, component-model, wasm, typescript, python, guide]
created: 2026-09-12
updated: 2026-09-18
---

# Writing an extension

An extension is a WebAssembly component. The core loads it, provides the host
interfaces it declared, and calls its kind's exported interface. This page covers
generating, building, testing, and installing a guest.

The first three steps are straightforward. The fourth — installing — has a catch
explained in the final section.

The steps below are Rust, which every first-party extension uses.
[TypeScript](#in-typescript) and [Python](#in-python) work against the same
WIT — read the language's section **before** you pick it, because both cost
megabytes per component and a toolchain this repository does not install for
you. The numbers are in [Meet the cost first](#meet-the-cost-first).

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

`LANG=ts` and `LANG=py` generate the same kinds in TypeScript or Python
instead — see [In TypeScript](#in-typescript) and [In Python](#in-python).

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

## Meet the cost first

All three languages, measured on one machine, back to back:

| | Rust (`tool-hello`) | TypeScript | Python |
|---|---|---|---|
| component | 54,633 bytes | **12.7 MB** | **18.5 MB** |
| toolchain | the cargo toolchain you already have | **326 MB** of `node_modules` (`jco`) | **50 MB** (`componentize-py`) |
| its gate test, run alone | under 4 s | **21.0 s, 23.8 s** | **23.9 s, 25.4 s** |

Neither non-Rust component is large because of anything in it. The 12.7 MB is
the StarlingMonkey JavaScript engine and the 18.5 MB is CPython; both are the
price of the language. For scale, all twenty Rust components together are
3.5 MB. Note the two costs run opposite ways: Python ships the bigger
artefact from the far smaller toolchain.

**Where the time goes, and where it does not.** Not staging: `Runtime::boot`
loads the instances `config.yaml` enables, not the contents of `ext/`
(`src/core/src/lib.rs:324`), so a staged component nothing enables costs
nothing. The cost is Cranelift compiling the component at boot in the one
test that *does* enable it, against a compile cache that is cold because each
test boots into a fresh temp config directory.

Read those two timing columns as "about the same". Two runs each, alternating,
is enough to say Python is a little slower and not enough to say how much.
Inside a full `make gate` the same test reports roughly **59 s**, because
nextest runs it alongside 800 others — that number measures contention, not
your guest.

That is why neither toolchain is in `make setup`, why each build skips itself
where its toolchain is absent, and why nothing in `ext/` is committed.

## In TypeScript

The Component Model is language-neutral and this repository proves it rather
than asserting it: `src/extensions/tool-hello-ts` is the TypeScript twin of
`tool-hello`, built against the same `wit/`, dispatched by the same fleet, with
no host-side special case. If you want the proof rather than the instructions,
`src/host/tests/it/polyglot_ts.rs` runs a turn through it.

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

## In Python

`src/extensions/tool-hello-py` is the third twin of `tool-hello`, built with
`componentize-py` against the same `wit/`. `src/host/tests/it/polyglot_py.rs`
runs a turn through it. The costs are in [the table above](#meet-the-cost-first) —
the largest component of the three, from the smallest toolchain.

### 1. Install the toolchain

```console
$ pipx install componentize-py==0.25.1
```

One tool, not two: `componentize-py` generates its own bindings, so there is
no bundler step. Pinned in `versions.mk`, and not in `make setup` for the same
reason `jco` is not.

### 2. Generate it

```console
$ make ext-new NAME=tool-greet-py KIND=tool LANG=py
```

`LANG=py` supports `tool` and `interceptor`, the same two as TypeScript and
for the same reason.

You get exactly two hand-written files: `app.py` and `pyproject.toml`
(which exists so the manifest generator can read a version and description,
the way it reads `Cargo.toml` and `package.json`).

### 3. Build it

```console
$ make -C src/extensions py-guest
```

`make extensions` runs this too, and skips with what to install when
`componentize-py` is absent. The world comes from the name prefix, as with
TypeScript — `tool-*` builds against `tool-world`.

### Unlike TypeScript, there are bindings

`componentize-py bindings` writes a `wit_world` package beside your guest, and
your classes implement its `Protocol`s:

```python
from wit_world.exports import ToolCallable as ToolCallableProtocol
from wit_world.exports.tool_callable import ToolMeta


class ToolCallable(ToolCallableProtocol):
    def meta(self) -> ToolMeta: ...
```

That is closer to `wit-bindgen` than to `jco`: the generated classes *are* the
contract rather than a transcription of it, so a shape mismatch is caught
against generated code instead of at componentize time.

Two consequences worth knowing before you start:

- **`wit_world` does not exist until you build.** It is gitignored, regenerated
  every run so it cannot go stale against `wit/`, and your editor will not
  resolve those imports until `make -C src/extensions py-guest` has run once.
- **Names are snake_case, not lowerCamelCase.** `componentize-py` lowers
  `arguments-schema` to `arguments_schema` where `jco` gives
  `argumentsSchema`, and a variant case becomes `Decision_Proceed`. Each
  toolchain picks its own convention; the generated package is the authority.

### It imports raw WASI, and that is fine

CPython's standard library needs `wasi:filesystem`, `wasi:sockets` and
`wasi:cli` to initialise, so every Python component imports them — and unlike
the TypeScript guest's `wasi:http`, there is no flag to drop them.

Nothing is reachable through them. Those interfaces are linked for every
guest and refused at the context: no preopens are configured and every socket
address is denied. `docs/concepts/security-model.md` carries a row per
refusal naming the `sandbox_boundary.rs` test that proves it, against
`tool-escape-probe`, which reaches for them deliberately.

What this does mean is that your manifest is a weaker signal here than in
Rust — see [the manifest note above](#its-manifest-is-its-world-not-its-need),
which applies to `componentize-py` exactly as it does to ComponentizeJS.

### Testing, enabling and installing are unchanged

Same host suite, same `config.yaml` key (`tool: greet-py:` resolves to
`ext/tool-greet-py.wasm`), same signature requirement on install.

## Where to look next

- [Contracts](../concepts/contracts.md) — the WIT interfaces, and the version rule your crate is built against.
- [Configuration](../concepts/configuration.md) — every key, including what each capability's grant looks like.
- [Security model](../concepts/security-model.md) — what your extension is and is not given, and the test that proves each one.
