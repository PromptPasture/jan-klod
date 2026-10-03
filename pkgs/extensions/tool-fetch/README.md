# tool-fetch

fetch tool: retrieve a URL through host-http as readable text, with an SSRF guard on the model-supplied address.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-fetch.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-fetch.manifest.toml`; they are not listed here.
