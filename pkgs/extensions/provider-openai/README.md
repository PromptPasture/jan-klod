# provider-openai

OpenAI-compatible llm-provider extension: chat completions over host-http.

A `provider` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/provider-openai.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/provider-openai.manifest.toml`; they are not listed here.
