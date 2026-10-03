# interceptor-system

system-prompt interceptor: prepends the standing instructions a turn is run under, at select-model so the context budget accounts for them.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-system.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-system.manifest.toml`; they are not listed here.
