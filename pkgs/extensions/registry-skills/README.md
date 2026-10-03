# registry-skills

Skill catalog: discovers named skills from .agents/skills/ and exposes them as tools.

A `registry` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/registry-skills.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/registry-skills.manifest.toml`; they are not listed here.
