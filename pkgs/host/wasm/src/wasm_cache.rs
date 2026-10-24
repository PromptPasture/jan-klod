//! Wasmtime's compile cache; a hit loads unverified native code, so the directory is made `0700` on unix.

use std::path::{Path, PathBuf};

use wasmtime::{Cache, CacheConfig, Config as EngineConfig, Engine};

use crate::CoreError;

const DEFAULT_DIR_NAME: &str = "wasmtime-cache";

/// Independent of `storage.path`: in-memory stores have none.
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

/// Wasmtime's `CacheConfig::validate` creates a missing directory but never restricts its permissions.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Returns the `Cache` handle too, as it is not reachable back out of the engine.
pub fn build_engine(
    config_dir: &Path,
    agent: &serde_json::Value,
) -> Result<(Engine, Cache), CoreError> {
    let dir = resolve_cache_dir(config_dir, agent);
    ensure_private_dir(&dir).map_err(|source| CoreError::Cache {
        path: dir.display().to_string(),
        message: source.to_string(),
    })?;
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
