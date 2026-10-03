# jk-session

The session store: an append-only event log, the projections derived from it, and the only crate in the tree that knows SQLite.

Used by [`jan-klod-core`](../core) and [`jan-klod-host`](../host). Depends on [`jan-klod-protocol`](../protocol).

```sh
cargo test -p jk-session
```
