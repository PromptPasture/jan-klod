# jk-config

The loader for `config.yaml`: extensions are grouped by category, each entry is one instance, and the loader interprets `enabled` and hands everything else to the instance opaquely.

Used by [`jk-agent`](../agent) and [`jk-host`](../host). Depends on no other workspace crate.

```sh
cargo test -p jk-config
cargo run -p jk-config --features examples --example dump -- config.yaml   # print what a file parses to
```

The file format is documented in [docs/configuration.md](docs/configuration.md).
