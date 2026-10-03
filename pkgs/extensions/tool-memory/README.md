# tool-memory

Store and recall facts across sessions.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-memory.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-memory.manifest.toml`; they are not listed here.
