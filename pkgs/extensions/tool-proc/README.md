# tool-proc

Start, read and stop a long-lived child the operator named.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-proc.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-proc.manifest.toml`; they are not listed here.
