# jk-session

The session store: an append-only event log, the projections derived from it, and the only crate in the tree that knows SQLite.

Used by [`jk-agent`](../agent) and [`jk-host`](../host). Depends on [`jk-protocol`](../protocol).

```sh
cargo test -p jk-session
```
