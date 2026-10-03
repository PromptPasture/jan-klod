# tool-find

find tool: bounded glob search of the workspace tree through host-fs; walks only what the jail exposes.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-find.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-find.manifest.toml`; they are not listed here.
