# tool-proc-probe

Phase 7 host-process probe: a tool-callable guest that runs a command through host-process and returns its stdout, exercising the exec substrate across the Component-Model boundary.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-proc-probe.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-proc-probe.manifest.toml`; they are not listed here.
