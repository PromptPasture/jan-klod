# interceptor-persona

personality-data interceptor: reads and injects per-turn persona text from configuration.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-persona.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-persona.manifest.toml`; they are not listed here.
