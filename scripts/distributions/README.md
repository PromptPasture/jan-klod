# Distributions

A distribution says what a jan-klod install is *for*. It is not a client
choice — `tui` versus `gui` is how you look at the runtime, and that is
orthogonal to what the runtime does.

Each directory here is one distribution, as data:

- `guests` — the components to stage, one name per line. Comments and blank
  lines are ignored. Every name must be one `make extensions` builds.
- `config.yaml` — the config the archive ships with.

`make bundle DIST=<name>` builds an archive from the pair. `make bundle` with
no `DIST` keeps the repository's own `config.yaml` and the full `ext/`.

**Not `dist/`.** That is `make bundle`'s output directory and it is
git-ignored — definitions put there would vanish on the next clean.

Every `config.yaml` here is checked by
`host/tests/it/docs_match_config.rs`: the README, the quickstart, the landing
page and the installer must each name a provider key that **every** shipped
config needs. Per config, not pooled — a getting-started page that names one
provider's key while a distribution needs another is wrong for whoever
installed that one, and pooling the keys would hide exactly that.
