# tool-git

git tool: read-only repository inspection through host-process, restricted to an allowlist of subcommands with repo-supplied code execution disabled.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-git.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-git.manifest.toml`; they are not listed here.
