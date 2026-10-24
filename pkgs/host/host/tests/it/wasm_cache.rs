//! The Wasmtime compile cache (#60) through `Runtime::boot`: a second boot skips
//! Cranelift, and changed guest bytes miss instead of hitting a stale entry.
//! Skips when the guest is not staged in `ext/`.

use jk_agent::Runtime;

use crate::common;

/// Private copy of `tool-fs.wasm` and its manifest, so a test can mutate it without racing others.
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

/// One enabled instance, so each boot compiles exactly one component and the hit/miss counts are unambiguous.
fn config_for(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        "\nextensions:\n  tool:\n    fs:\n      enabled: true\n",
    )
    .unwrap();
    path
}

/// Appends a custom wasm section: changes the bytes without touching imports or exports,
/// so the manifest cross-check still passes.
fn append_custom_section(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).expect("reads the staged component");
    let name = b"jk-cache-test";
    let payload = [0xAA_u8];
    let mut content = Vec::new();
    content.push(u8::try_from(name.len()).expect("a short fixed name fits in one byte"));
    content.extend_from_slice(name);
    content.extend_from_slice(&payload);
    bytes.push(0x00);
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
