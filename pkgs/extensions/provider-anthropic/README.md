# provider-anthropic

Anthropic Messages API llm-provider extension: native Claude completions over host-http.

A `provider` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/provider-anthropic.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/provider-anthropic.manifest.toml`; they are not listed here.
