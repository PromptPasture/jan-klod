# tool-fs

fs tool: read / write / grep a workspace file through host-fs, dispatched by an `op` argument.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-fs.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-fs.manifest.toml`; they are not listed here.
