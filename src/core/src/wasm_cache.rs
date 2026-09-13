//! Wasmtime's own compiled-artefact cache, wired into the `Engine` every boot
//! builds — not a hand-rolled `.cwasm` cache of this repository's own.
//!
//! # Why the built-in cache, and not #60's fallback
//!
//! [#60](https://github.com/PromptPasture/jan-klod/issues/60) named the
//! fallback first and asked that the built-in cache
//! (`Config::cache_config_load*` in the issue's wording) be evaluated first,
//! and adopted "if it meets the need with a config file in the data dir".
//! Checked against Wasmtime 46.0.3 (the version this workspace's `Cargo.lock`
//! actually pins — see `Cargo.toml`'s `wasmtime = "46"`) via Context7's current
//! `docs.rs/wasmtime` snapshot and this workspace's own vendored source under
//! `~/.cargo/registry/src/.../wasmtime-46.0.3/`:
//!
//! * `Config::cache_config_load*` **no longer exists in 46** — the issue's
//!   named API was renamed. The current shape is `Cache::from_file` /
//!   `CacheConfig` (a struct with a builder, `with_directory` and friends) plus
//!   `Config::cache(Some(cache))`, confirmed by
//!   `wasmtime-46.0.3/src/config.rs`'s `pub use wasmtime_cache::{Cache,
//!   CacheConfig};` and `pub fn cache(&mut self, cache: Option<Cache>)`.
//! * It caches **components**, not only core modules — Wasmtime's own test
//!   suite proves this directly (`wasmtime-46.0.3/src/engine/serialization.rs`,
//!   `fn components_are_cached`): `Component::new` is a cache hit on a second
//!   call with the same `Engine`/bytes.
//! * The key already covers what #60 asked an explicit cache to key on, and
//!   more precisely than a hand-rolled one could without duplicating
//!   Wasmtime's own compiler internals. `HashedEngineCompileEnv::hash`
//!   (`wasmtime-46.0.3/src/compile/code_builder.rs`) hashes the compiler's
//!   target triple, its codegen flags and ISA flags, `Engine::tunables()`,
//!   `Engine::features()`, `Config::wmemcheck`, and — its own comment says so
//!   — `Config::module_version`, "to catch accidental bugs of reusing across
//!   crate versions". `module_version` defaults to
//!   `ModuleVersionStrategy::WasmtimeVersion` (`wasmtime-46.0.3/src/config.rs`,
//!   `impl Default for ModuleVersionStrategy`), which this workspace never
//!   overrides — so a Wasmtime upgrade changes the hash. The component's own
//!   bytes are a separate term in the same hashed state
//!   (`wasmtime-46.0.3/src/compile/runtime.rs`'s `compile_cached`), so
//!   changing a guest's bytes changes the key too. That is every term #60's
//!   own key proposal named (`sha256(component bytes) + wasmtime version +
//!   target triple + engine config hash`), computed by Wasmtime itself rather
//!   than re-derived here where a missed term would silently under-key a
//!   cache of *native code*.
//! * A corrupt or foreign artefact is a miss, not an error, by construction:
//!   `ModuleCacheEntryInner::get_data` (`wasmtime-internal-cache-46.0.3/src/
//!   lib.rs`) returns `None` on a read/decompress failure via `.ok()`, and
//!   `get_data_raw` falls through to `compute` (recompile) whenever
//!   `deserialize` — ultimately `Engine::load_code_bytes` — does not accept
//!   what came back. Exactly #60's requirement, already true of the code this
//!   module hands the bytes to.
//!
//! So the built-in cache meets the need, and this module stops there: no
//! `.cwasm` files, no key derivation, no `deserialize_file` call of our own.
//! What this module owns is strictly narrower: choosing *where* the cache
//! directory lives, making sure it is created user-private, and handing the
//! resulting [`Cache`] back to [`crate::Runtime`] so a boot can report the
//! hit/miss counters that prove a second boot skipped Cranelift.
//!
//! # The security property this module is responsible for
//!
//! A cache **hit** is `Engine::load_code_bytes` handed a file this process did
//! not just compile — the same trust `Component::deserialize`/
//! `deserialize_file` extends to an artefact from disk: it is accepted as
//! native code, not re-verified. Wasmtime's key (above) stops a *stale* or
//! *foreign-engine* artefact from being reused; it says nothing about who else
//! can *write into* the directory that key is read from. A world-writable
//! cache directory would let anything on the machine plant bytes this process
//! later executes as compiled code. [`ensure_private_dir`] is what closes that
//! gap: `0700` on unix, checked by
//! `wasm_cache::the_cache_directory_is_created_user_private_on_unix`.

use std::path::{Path, PathBuf};

use wasmtime::{Cache, CacheConfig, Config as EngineConfig, Engine};

use crate::CoreError;

/// Directory name used when `storage.cache-dir` names none: a sibling of
/// `storage.path`'s directory (or of `config.yaml` itself, when storage is the
/// default in-memory store) — "beside the `SQLite` db", per #60.
const DEFAULT_DIR_NAME: &str = "wasmtime-cache";

/// Resolve the compile-cache directory: `storage.cache-dir` if the agent
/// config names one (a relative path resolves against `config_dir`, the same
/// rule `storage.path` already follows), else `config_dir/wasmtime-cache`.
///
/// Deliberately **not** derived from `storage.path` itself (e.g. by stripping
/// the file name) — an in-memory store (no `storage.path` at all) is a
/// perfectly ordinary deployment, and the compile cache is independent of
/// whether the transcript persists. "Beside the `SQLite` db" is satisfied
/// because both default to the same `config_dir` when a relative `storage.path`
/// is given, without making the cache's existence depend on the store's.
pub fn resolve_cache_dir(config_dir: &Path, agent: &serde_json::Value) -> PathBuf {
    let configured = agent
        .get("storage")
        .and_then(|storage| storage.get("cache-dir"))
        .and_then(serde_json::Value::as_str);
    configured.map_or_else(
        || config_dir.join(DEFAULT_DIR_NAME),
        |dir| config_dir.join(dir),
    )
}

/// Create `dir` (and any missing parents) and make it user-private on unix.
///
/// Called before the directory is handed to Wasmtime: `CacheConfig::validate`
/// (invoked inside `Cache::new`) will itself `create_dir_all` a missing
/// directory, but it never restricts its permissions — this is the one thing
/// Wasmtime's own cache setup does not do on our behalf, and the one thing
/// #60 calls out as non-optional given what a hit trusts.
///
/// Not gated behind `#[cfg(unix)]` at the call site: on a platform with no
/// unix permission bits the directory is still created, just without the
/// chmod, which is the honest default rather than a silent no-op that looks
/// like it did something everywhere.
///
/// # Errors
/// Whatever `std::fs::create_dir_all`/`set_permissions` returns.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Build the `Engine` a boot uses, with Wasmtime's compile cache wired to
/// `config_dir`/`agent`'s resolved cache directory.
///
/// Returns the `Cache` handle alongside the `Engine` (not reachable back out
/// of it: `Config::cache`'s stored copy is `pub(crate)` inside `wasmtime`
/// itself) so a caller can read `cache_hits()`/`cache_misses()` after
/// compiling every enabled instance — [`crate::Runtime::compile_cache_stats`]
/// is that caller, and the boot report's line is what proves a second boot
/// skipped Cranelift.
///
/// # Errors
/// [`CoreError::Cache`] if the directory cannot be created/made private, or if
/// Wasmtime itself refuses the cache configuration or engine construction.
pub fn build_engine(
    config_dir: &Path,
    agent: &serde_json::Value,
) -> Result<(Engine, Cache), CoreError> {
    let dir = resolve_cache_dir(config_dir, agent);
    ensure_private_dir(&dir).map_err(|source| CoreError::Cache {
        path: dir.display().to_string(),
        message: source.to_string(),
    })?;
    // `CacheConfig::with_directory` requires an absolute path; the directory
    // now exists (just created, or already there), so canonicalizing cannot
    // fail for the reason it would on a directory that does not exist yet.
    let canonical = dir.canonicalize().map_err(|source| CoreError::Cache {
        path: dir.display().to_string(),
        message: source.to_string(),
    })?;

    let mut cache_config = CacheConfig::new();
    cache_config.with_directory(&canonical);
    let cache = Cache::new(cache_config).map_err(|source| CoreError::Cache {
        path: canonical.display().to_string(),
        message: source.to_string(),
    })?;

    let mut engine_config = EngineConfig::new();
    engine_config.cache(Some(cache.clone()));
    let engine = Engine::new(&engine_config).map_err(|source| CoreError::Cache {
        path: canonical.display().to_string(),
        message: source.to_string(),
    })?;

    Ok((engine, cache))
}

#[cfg(test)]
mod tests {
    use super::{build_engine, ensure_private_dir, resolve_cache_dir};
    use wasmtime::component::Component;
    use wasmtime::{Cache, CacheConfig, Config as EngineConfig, Engine, ModuleVersionStrategy};

    /// Two distinct, individually valid components — used to prove a byte
    /// change is a miss without needing a real staged guest (that variant is
    /// covered end-to-end, through a real `Runtime::boot`, by
    /// `host/tests/it/wasm_cache.rs`).
    const WAT_A: &str = "(component (core module (func)))";
    const WAT_B: &str = "(component (core module (func (param i32))))";

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn temp_dir(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("jk-wasm-cache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        TempDir(dir)
    }

    #[test]
    fn cache_dir_defaults_beside_config_dir_and_honours_an_override() {
        let config_dir = std::path::Path::new("/tmp/jk-example");
        assert_eq!(
            resolve_cache_dir(config_dir, &serde_json::json!({})),
            config_dir.join("wasmtime-cache"),
            "no storage.cache-dir: the default sits beside config.yaml"
        );
        assert_eq!(
            resolve_cache_dir(
                config_dir,
                &serde_json::json!({ "storage": { "cache-dir": "elsewhere/cache" } })
            ),
            config_dir.join("elsewhere/cache"),
            "a relative storage.cache-dir resolves against config_dir, like storage.path"
        );
    }

    #[test]
    fn the_cache_directory_is_created_user_private_on_unix() {
        let dir = temp_dir("perm");
        ensure_private_dir(&dir.0).expect("creates the directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir.0)
                .expect("reads the directory's metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, 0o700,
                "a cache hit is deserialized as native code — this directory must not \
                 be group- or world-writable"
            );
        }
    }

    #[test]
    fn a_cold_boot_misses_and_a_warm_one_against_the_same_dir_hits() {
        let dir = temp_dir("hit");
        let (engine, cache) =
            build_engine(&dir.0, &serde_json::json!({})).expect("builds an engine with a cache");
        Component::new(&engine, WAT_A).expect("compiles");
        assert_eq!(
            (cache.cache_hits(), cache.cache_misses()),
            (0, 1),
            "the first compile of unseen bytes is a miss"
        );

        // A second, independent `Engine`/`Cache` pointed at the same directory
        // — the shape of a real second boot — not a second call on the same
        // `Engine`, which would prove only that an in-process cache works.
        let (engine2, cache2) =
            build_engine(&dir.0, &serde_json::json!({})).expect("builds a second engine");
        Component::new(&engine2, WAT_A).expect("compiles from the cache");
        assert_eq!(
            (cache2.cache_hits(), cache2.cache_misses()),
            (1, 0),
            "the metric must show the hit directly — not an inference from a timing"
        );
    }

    #[test]
    fn different_bytes_at_the_same_cache_dir_do_not_share_an_entry() {
        let dir = temp_dir("bytes");
        let (engine, cache) =
            build_engine(&dir.0, &serde_json::json!({})).expect("builds an engine with a cache");
        Component::new(&engine, WAT_A).expect("compiles A");
        assert_eq!((cache.cache_hits(), cache.cache_misses()), (0, 1));

        let (engine2, cache2) =
            build_engine(&dir.0, &serde_json::json!({})).expect("builds a second engine");
        Component::new(&engine2, WAT_B).expect("compiles B");
        assert_eq!(
            (cache2.cache_hits(), cache2.cache_misses()),
            (0, 1),
            "different bytes at the same cache directory must not read back as a hit"
        );
    }

    /// Simulates a Wasmtime version change rather than performing one: two
    /// crate versions of `wasmtime` cannot both be linked into one test
    /// binary, so this exercises the exact field a real upgrade would also
    /// change — `Config::module_version`, which defaults to
    /// `ModuleVersionStrategy::WasmtimeVersion` and is hashed by
    /// `HashedEngineCompileEnv` specifically, per its own doc comment, "to
    /// catch accidental bugs of reusing across crate versions"
    /// (`wasmtime-46.0.3/src/compile/code_builder.rs`). This workspace never
    /// overrides `module_version`, so in production that term really is the
    /// crate version; here it is pinned to two different `Custom` strings to
    /// stand in for "before" and "after" an upgrade.
    #[test]
    fn a_module_version_change_the_same_shape_as_a_wasmtime_upgrade_is_a_miss() {
        let dir = temp_dir("version");
        let cache_dir = dir.0.join("cache");
        ensure_private_dir(&cache_dir).expect("creates the directory");
        let canonical = cache_dir.canonicalize().expect("canonicalizes");

        let mut config_a = EngineConfig::new();
        let mut cache_config_a = CacheConfig::new();
        cache_config_a.with_directory(&canonical);
        let cache_a = Cache::new(cache_config_a).expect("cache a");
        config_a
            .module_version(ModuleVersionStrategy::Custom("before-upgrade".to_string()))
            .expect("a short custom version string is accepted");
        config_a.cache(Some(cache_a.clone()));
        let engine_a = Engine::new(&config_a).expect("engine a");
        Component::new(&engine_a, WAT_A).expect("compiles under version a");
        assert_eq!((cache_a.cache_hits(), cache_a.cache_misses()), (0, 1));

        let mut config_b = EngineConfig::new();
        let mut cache_config_b = CacheConfig::new();
        cache_config_b.with_directory(&canonical);
        let cache_b = Cache::new(cache_config_b).expect("cache b");
        config_b
            .module_version(ModuleVersionStrategy::Custom("after-upgrade".to_string()))
            .expect("a short custom version string is accepted");
        config_b.cache(Some(cache_b.clone()));
        let engine_b = Engine::new(&config_b).expect("engine b");
        Component::new(&engine_b, WAT_A).expect("compiles under version b");
        assert_eq!(
            (cache_b.cache_hits(), cache_b.cache_misses()),
            (0, 1),
            "identical bytes under a different module_version — the same axis a real \
             Wasmtime upgrade changes — must not reuse version a's cached artefact"
        );
    }
}
