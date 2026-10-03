# jan-klod-protocol

The versioned wire contract every client shares: named commands, named events, and the committed JSON Schema (`schema/protocol.schema.json`) generated from the types and checked for drift by `tests/wire.rs`.

Used by [`jan-klod-core`](../core), [`jan-klod-host`](../host), [`jk-session`](../session) and the terminal client. Depends on `serde` and `serde_json` only.

```sh
cargo test -p jan-klod-protocol
```

Rank and rules: [contracts](../../../docs/concepts/contracts.md).
