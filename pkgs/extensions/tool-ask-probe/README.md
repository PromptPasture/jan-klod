# tool-ask-probe

A tool that asks the user before answering. Test instrument for the tool-askable path.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-ask-probe.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-ask-probe.manifest.toml`; they are not listed here.
