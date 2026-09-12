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

**The window is a second axis, not a fourth directory.** The rule at the top of
this page was tested when the Tauri shell landed ([#143] asked for a `gui`
distribution beside the other three). It held: a `gui/` here would have been
`coding/`'s guest list copied into a directory whose only real difference is one
extra binary, leaving two lists to keep in step. So `DIST` still says what an
install is *for*, and `GUI=1` says whether a window ships:

```sh
make bundle DIST=coding GUI=1     # -> jan-klod-<v>-<os>-<arch>-coding-gui.tar.gz
```

`scripts/install.sh --gui` is the other end of it, and
`docs_match_config::the_release_builds_a_gui_archive_if_the_installer_offers_one`
fails the build if one exists without the other.

[#143]: https://github.com/PromptPasture/jan-klod/issues/143

**Not `dist/`.** That is `make bundle`'s output directory and it is
git-ignored — definitions put there would vanish on the next clean.

Every `config.yaml` here is checked by
`host/tests/it/docs_match_config.rs`: the README, the quickstart, the landing
page and the installer must each name a provider key that **every** shipped
config needs. Per config, not pooled — a getting-started page that names one
provider's key while a distribution needs another is wrong for whoever
installed that one, and pooling the keys would hide exactly that.
