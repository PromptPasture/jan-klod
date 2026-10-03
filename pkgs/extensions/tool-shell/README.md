# tool-shell

shell tool: run a command in the workspace through the bounded host-process substrate.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-shell.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-shell.manifest.toml`; they are not listed here.
