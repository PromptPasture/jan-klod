//! Wasmtime's built-in compile cache (#60), proven through the real boot
//! path: `Runtime::boot` twice against the same `config.yaml`/`ext/` skips
//! Cranelift the second time, and mutating a guest's bytes between boots
//! produces a fresh miss rather than a stale hit.
//!
//! `core/src/wasm_cache.rs`'s own unit tests exercise the same mechanism
//! directly against Wasmtime's API — with synthetic WAT components, so they
//! never skip — and are where the "changing the Wasmtime version" acceptance
//! line is covered (two crate versions cannot both be linked into one test
//! binary, so that test simulates the axis Wasmtime itself hashes for it).
//! This file is the same claim proven end-to-end through `Runtime::boot`,
//! with a real staged guest's real bytes.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use jan_klod_core::Runtime;

use crate::common;

/// A private copy of `tool-fs.wasm` (+ manifest) this file can mutate freely —
/// the shared `ext/` is read by other tests running concurrently, and one of
/// these tests corrupts its copy's bytes on purpose.
fn copy_tool_fs(dir: &std::path::Path) -> std::path::PathBuf {
    let real_ext = common::repo_root().join("ext");
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    std::fs::copy(real_ext.join("tool-fs.wasm"), ext.join("tool-fs.wasm")).unwrap();
    std::fs::copy(
        real_ext.join("tool-fs.manifest.toml"),
        ext.join("tool-fs.manifest.toml"),
    )
    .unwrap();
    ext
}

/// One enabled instance (`tool.fs`), so exactly one component is compiled per
/// boot and the cache's hit/miss counters have an unambiguous size to check
/// against.
fn config_for(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        "\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
    )
    .unwrap();
    path
}

/// Append a trailing custom wasm section: a standard, always-legal way to
/// change a component's bytes without touching what it imports or exports, so
/// the manifest cross-check `Runtime::boot` also runs still passes. Confirmed
/// locally against this exact file with `wasm-tools validate` and `wasmtime
/// compile` before this test was written, rather than assumed.
fn append_custom_section(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).expect("reads the staged component");
    let name = b"jk-cache-test";
    let payload = [0xAA_u8];
    let mut content = Vec::new();
    content.push(u8::try_from(name.len()).expect("a short fixed name fits in one byte"));
    content.extend_from_slice(name);
    content.extend_from_slice(&payload);
    bytes.push(0x00); // custom section id
    bytes.push(u8::try_from(content.len()).expect("this content is well under 128 bytes"));
    bytes.extend_from_slice(&content);
    std::fs::write(path, bytes).expect("writes the mutated component back");
}

#[test]
fn a_second_boot_against_the_same_config_dir_skips_cranelift() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-wasm-cache-boot-{}", std::process::id()));
    let _guard = common::TempDir(dir.clone());
    let ext = copy_tool_fs(&dir);
    let config = config_for(&dir);

    let first = Runtime::boot(&config, &ext).expect("cold boot compiles");
    assert_eq!(
        first.compile_cache_stats(),
        (0, 1),
        "a fresh cache directory has nothing to hit yet: {}",
        first.report()
    );

    // Same `config_dir` (`dir`) => the default cache dir
    // (`<config_dir>/wasmtime-cache`) resolves to the same place, so this is
    // genuinely "the second boot after a cold one", not two unrelated caches.
    let second = Runtime::boot(&config, &ext).expect("warm boot loads from the cache");
    assert_eq!(
        second.compile_cache_stats(),
        (1, 0),
        "the second boot must report the hit as a metric, not merely run faster: {}",
        second.report()
    );
    assert!(
        second.report().to_string().contains("1 hit(s), 0 miss(es)"),
        "the boot-plan report an operator actually sees must say so too: {}",
        second.report()
    );
}

#[test]
fn changing_a_guests_bytes_between_boots_is_a_miss_not_a_stale_hit() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-wasm-cache-bytes-{}", std::process::id()));
    let _guard = common::TempDir(dir.clone());
    let ext = copy_tool_fs(&dir);
    let config = config_for(&dir);

    Runtime::boot(&config, &ext).expect("cold boot compiles and populates the cache");
    let warm = Runtime::boot(&config, &ext).expect("warm boot hits");
    assert_eq!(
        warm.compile_cache_stats(),
        (1, 0),
        "confirms the cache is genuinely warm before the mutation below"
    );

    append_custom_section(&ext.join("tool-fs.wasm"));
    let after_change = Runtime::boot(&config, &ext).expect("still a valid component — still boots");
    assert_eq!(
        after_change.compile_cache_stats(),
        (0, 1),
        "different bytes at the same cache dir must not read back as a hit: {}",
        after_change.report()
    );
}
