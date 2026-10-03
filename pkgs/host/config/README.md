# jan-klod-config

The loader for `config.yaml`: extensions are grouped by category, each entry is one instance, and the loader interprets `enabled` and hands everything else to the instance opaquely.

Used by [`jan-klod-core`](../core) and [`jan-klod-host`](../host). Depends on no other workspace crate.

```sh
cargo test -p jan-klod-config
cargo run -p jan-klod-config --features examples --example dump -- config.yaml   # print what a file parses to
```

The file format is documented in [docs/configuration.md](docs/configuration.md).
