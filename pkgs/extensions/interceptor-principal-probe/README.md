# interceptor-principal-probe

A guest that tests principal boundary: receives principal at before-loop, can route based on it, existing guests that ignore it continue to work.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-principal-probe.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-principal-probe.manifest.toml`; they are not listed here.
