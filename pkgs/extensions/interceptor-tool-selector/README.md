# interceptor-tool-selector

select-tools interceptor: a thin pass-through that exposes the active tool set to the loop; per-step narrowing is a later refinement.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-tool-selector.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-tool-selector.manifest.toml`; they are not listed here.
