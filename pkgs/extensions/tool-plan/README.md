# tool-plan

The session plan, as something the model can write down and read back.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-plan.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-plan.manifest.toml`; they are not listed here.
