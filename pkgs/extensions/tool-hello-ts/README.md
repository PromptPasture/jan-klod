# tool-hello-ts

tool extension in TypeScript: the polyglot twin of tool-hello, componentized with jco against the same wit/.

A `tool` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions ts-guest
```

The capabilities it needs are read from the built component into `ext/tool-hello-ts.manifest.toml`; they are not listed here.
