# interceptor-intent-router

before-loop interceptor: layered intent router (language -> heuristics -> LLM classifier) that blocks the agentic loop for simple prompts and proceeds for agentic ones.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-intent-router.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-intent-router.manifest.toml`; they are not listed here.
