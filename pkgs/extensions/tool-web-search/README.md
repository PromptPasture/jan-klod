# tool-web-search

web-search tool: search the web through a configured provider API, returning results as readable text.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-web-search.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-web-search.manifest.toml`; they are not listed here.
