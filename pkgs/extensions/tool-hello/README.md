# tool-hello

A tool extension. Replace this description with what it does.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-hello.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-hello.manifest.toml`; they are not listed here.
