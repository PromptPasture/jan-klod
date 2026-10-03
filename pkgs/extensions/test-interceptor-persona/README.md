# test-interceptor-persona

test guest for interceptor-persona: verifies persona configuration loading and injection.

A guest component, built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/test-interceptor-persona.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/test-interceptor-persona.manifest.toml`; they are not listed here.
