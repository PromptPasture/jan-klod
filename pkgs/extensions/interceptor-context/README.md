# interceptor-context

select-context interceptor: trims conversation history to the model's token budget (char/4 estimate, drop oldest, keep system + most recent). Summarisation is a later refinement behind the same seam.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-context.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-context.manifest.toml`; they are not listed here.
