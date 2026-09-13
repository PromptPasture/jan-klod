---
type: guide
title: Writing an extension
description: From nothing to a working guest — generate a crate, build it, test it, and install it — and the one refusal that will otherwise stop you.
tags: [extensions, pdk, wit, component-model, wasm, guide]
created: 2026-09-12
updated: 2026-09-12
---

# Writing an extension

An extension is a WebAssembly component. The core loads it, provides the host
interfaces it declared, and calls its kind's exported interface. This page covers
generating, building, testing, and installing a guest.

The first three steps are straightforward. The fourth — installing — has a catch
explained in the final section.

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

## Where to look next

- [Contracts](../concepts/contracts.md) — the WIT interfaces, and the version rule your crate is built against.
- [Configuration](../concepts/configuration.md) — every key, including what each capability's grant looks like.
- [Security model](../concepts/security-model.md) — what your extension is and is not given, and the test that proves each one.
