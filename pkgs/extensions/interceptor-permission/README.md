# interceptor-permission

tool-call interceptor: a thin single-rule permission gate that asks the driver to confirm a potentially dangerous tool call, then blocks or proceeds on the answer.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-permission.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-permission.manifest.toml`; they are not listed here.
