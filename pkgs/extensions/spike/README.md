# spike

The Slice 1a polyglot canary: a TinyGo component that exports the `spike` world's `complete` function and echoes the prompt, proving a non-Rust guest crosses the Component Model boundary. Built against [`wit/spike`](../../../wit/spike).

```sh
make -C pkgs/extensions spike-guest
```

The built component, `spike.wasm`, is committed and loaded by the polyglot test.
