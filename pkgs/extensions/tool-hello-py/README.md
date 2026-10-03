# tool-hello-py

tool extension in Python: the polyglot twin of tool-hello, componentized with componentize-py against the same wit/.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions py-guest
```

The capabilities it needs are read from the built component into `ext/tool-hello-py.manifest.toml`; they are not listed here.
