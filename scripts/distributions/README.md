# Distributions

Distribution defines install purpose, not client. `tui`/`gui` is how you view the runtime (orthogonal to what it does).

Each directory is one distribution (data):

- `guests` — components to stage (one per line, comments ignored). Every name must be built by `make extensions`.
- `config.yaml` — shipped config.

`make bundle DIST=<name>` builds archive from the pair. No `DIST` → uses repo's `config.yaml` + full `ext/`.

**Window is a second axis, not a fourth directory.** When Tauri landed ([#143] asked `gui/` beside the other three), this held: `gui/` would be `coding/`'s guests copied with one extra binary, forcing two lists in step. So `DIST` defines purpose, `GUI=1` ships the window:

```sh
make bundle DIST=coding GUI=1     # -> jan-klod-<v>-<os>-<arch>-coding-gui.tar.gz
```

`scripts/install.sh --gui` is paired with it; `docs_match_config::the_release_builds_a_gui_archive_if_the_installer_offers_one` ensures they stay synchronized.

[#143]: https://github.com/PromptPasture/jan-klod/issues/143

**Not `dist/`** — that's `make bundle`'s git-ignored output directory (definitions there vanish on clean).

`host/tests/it/docs_match_config.rs` checks every `config.yaml`: README, quickstart, landing page, installer must each name a provider key **all** shipped configs have (per-config, not pooled—pooling hides mismatches).
