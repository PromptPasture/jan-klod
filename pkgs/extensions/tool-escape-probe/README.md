# tool-escape-probe

A guest that tries to leave the sandbox: raw sockets, the host's stdin, the host's filesystem. Every attempt must fail; the tests assert that it does.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/tool-escape-probe.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/tool-escape-probe.manifest.toml`; they are not listed here.
