# host-secrets-probe

A guest that tests host-secrets capability: denied when not granted, not-found when absent, value when present.

A guest component, built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/host-secrets-probe.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/host-secrets-probe.manifest.toml`; they are not listed here.
