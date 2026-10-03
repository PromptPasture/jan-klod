# interceptor-task-router

select-model interceptor: classifies the request into a task type via a constrained llm-provider call, resolves the routing table (task -> provider/model) from host-config, and sets the model.

A `interceptor` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/interceptor-task-router.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/interceptor-task-router.manifest.toml`; they are not listed here.
