# Distributions

Distribution defines install purpose, not client.

Each directory is one distribution (data):

- `guests` — components to stage (one per line, comments ignored). Every name must be built by `make extensions`.
- `config.yaml` — shipped config.

`make bundle DIST=<name>` builds archive from the pair. No `DIST` → uses repo's `config.yaml` + full `ext/`.

**Not `dist/`** — that's `make bundle`'s git-ignored output directory (definitions there vanish on clean).

`pkgs/host/host/tests/it/docs_match_config.rs` checks every `config.yaml`: README, quickstart, landing page, installer must each name a provider key **all** shipped configs have (per-config, not pooled—pooling hides mismatches).
