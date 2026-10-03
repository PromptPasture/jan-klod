# tool-edit

edit tool: hash-anchored line edits of a workspace file through host-fs; stale anchors are rejected, never applied.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-edit.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-edit.manifest.toml`; they are not listed here.
