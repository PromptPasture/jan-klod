//! Environment and distribution audit for setup.
//!
//! The `setup` subcommand reads the current environment and distribution state,
//! printing what would be configured without writing any files.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Run the setup audit for a given distribution.
///
/// # Errors
/// Returns an error string if config cannot be loaded, distribution not found, or ext dir inaccessible.
pub fn audit(distribution: Option<&str>, config_path: &str, ext_dir: &str) -> Result<(), String> {
    // Load config without requiring environment variable expansion
    let config_yaml =
        fs::read_to_string(config_path).map_err(|e| format!("Failed to read config: {e}"))?;

    let config_raw: serde_json::Value = serde_yaml_ng::from_str(&config_yaml)
        .map_err(|e| format!("Failed to parse config YAML: {e}"))?;

    // Determine the distribution to audit
    let dist_name = distribution.map_or_else(
        || infer_distribution_from_yaml(&config_raw).unwrap_or_else(|| "coding".to_string()),
        std::string::ToString::to_string,
    );

    println!("═══ Setup Audit for Distribution: {dist_name} ═══\n");

    // Read the guests file for the distribution
    let guests_path = format!("scripts/distributions/{dist_name}/guests");
    let guests = read_guests_file(&guests_path)?;

    // Verify that ext_dir exists
    let ext_path = Path::new(ext_dir);
    if !ext_path.exists() {
        eprintln!("✗ ext directory not found: {ext_dir}");
        return Err(format!("ext directory not found: {ext_dir}"));
    }

    // Check guests
    println!("Guests:");
    check_guests(&guests, ext_path);
    println!();

    // Check config keys
    println!("Configuration Keys:");
    check_config_keys_yaml(&config_raw);
    println!();

    // Check providers
    println!("Provider API Keys:");
    check_provider_keys_yaml(&config_raw);
    println!();

    // Check sandbox capability
    println!("Sandbox Capability:");
    check_sandbox_capability();
    println!();

    // Check registry trust
    println!("Registry Trust:");
    check_registry_trust_yaml(&config_raw);
    println!();

    // Check MCP servers
    println!("MCP Servers:");
    check_mcp_servers_yaml(&config_raw);
    println!();

    Ok(())
}

fn infer_distribution_from_yaml(config: &serde_json::Value) -> Option<String> {
    // Look for any enabled provider to infer the distribution
    if let Some(extensions) = config.get("extensions") {
        if let Some(ext_obj) = extensions.as_object() {
            for (category, instances) in ext_obj {
                if category == "provider" {
                    if let Some(instances_obj) = instances.as_object() {
                        for (_name, instance_config) in instances_obj {
                            if instance_config
                                .get("enabled")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false)
                            {
                                // Found an enabled provider
                                return Some("coding".to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn read_guests_file(path: &str) -> Result<Vec<String>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read guests file '{path}': {e}"))?;

    let guests = content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .map(|line| line.trim().to_string())
        .collect();

    Ok(guests)
}

fn check_guests(guests: &[String], ext_path: &Path) {
    for guest in guests {
        // Check if the guest exists in ext/ as a .wasm file
        // Guests are named like "provider-openai", so the file would be "provider-openai.wasm"
        let wasm_pattern = format!("{guest}.wasm");

        // Check for exact match
        let exact_path = ext_path.join(&wasm_pattern);
        if exact_path.exists() {
            println!("  ✓ {guest} in ext/");
        } else {
            // Check for wildcard matches (for versioned extensions)
            if let Ok(entries) = fs::read_dir(ext_path) {
                let found = entries.filter_map(std::result::Result::ok).any(|e| {
                    let name = e.file_name();
                    let name_str = name.to_string_lossy();
                    name_str.starts_with(&format!("{guest}-")) && name_str.ends_with(".wasm")
                });

                if found {
                    println!("  ✓ {guest} in ext/");
                } else {
                    println!("  ✗ {guest} not found in ext/");
                }
            } else {
                println!("  ✗ {guest} not found in ext/");
            }
        }
    }
}

fn check_config_keys_yaml(config: &serde_json::Value) {
    // Extract top-level keys (excluding 'extensions')
    if let serde_json::Value::Object(root) = config {
        for key in root.keys() {
            if key != "extensions" && key != "observability" {
                println!("  ✓ {key} configured");
            }
        }
        if root.len() <= 1 {
            println!("  (using all defaults)");
        }
    } else {
        println!("  (using all defaults)");
    }
}

fn check_provider_keys_yaml(config: &serde_json::Value) {
    // Known provider API keys mapping
    let provider_keys = vec![
        ("openai", "OPENAI_API_KEY"),
        ("anthropic", "ANTHROPIC_API_KEY"),
    ];

    if let Some(extensions) = config.get("extensions") {
        if let Some(providers) = extensions.get("provider") {
            if let Some(provider_obj) = providers.as_object() {
                for (provider_name, _config) in provider_obj {
                    for (prov, env_key) in &provider_keys {
                        if prov == provider_name {
                            if std::env::var(env_key).is_ok() {
                                println!("  ✓ {provider_name} available (${env_key} is set)");
                            } else {
                                println!(
                                    "  ⚠ {provider_name} enabled but {env_key} not in environment"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

fn check_sandbox_capability() {
    #[cfg(target_os = "linux")]
    {
        // On Linux, check for Landlock availability
        if is_landlock_available() {
            println!("  ✓ Landlock sandbox available");
        } else {
            println!("  ⚠ Landlock not available (kernel < 5.13)");
        }
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, sandbox_init is typically available
        println!("  ✓ Seatbelt sandbox available (macOS)");
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        println!("  ⚠ No sandbox implementation for this platform");
    }
}

#[allow(dead_code)]
fn is_landlock_available() -> bool {
    // Try to detect Landlock by checking /proc/sys/kernel/landlock
    // This is a simple heuristic; a full check would need to attempt ABI negotiation
    std::fs::metadata("/proc/sys/kernel/landlock").is_ok()
}

fn check_registry_trust_yaml(config: &serde_json::Value) {
    // Check if registry.trusted-keys are present
    if let Some(registry) = config.get("registry") {
        if let Some(trusted_keys) = registry.get("trusted-keys") {
            if let Some(keys) = trusted_keys.as_array() {
                println!("  ✓ {} trusted signing key(s) configured", keys.len());
            } else {
                println!("  ✗ registry.trusted-keys is not an array");
            }
        } else {
            println!("  ⚠ registry configured but no trusted-keys specified");
        }
    } else {
        println!("  ⚠ registry not configured");
    }
}

fn check_mcp_servers_yaml(config: &serde_json::Value) {
    // Check for registry-mcp instance and its configured servers
    if let Some(extensions) = config.get("extensions") {
        if let Some(registries) = extensions.get("registry") {
            if let Some(registry_obj) = registries.as_object() {
                if let Some(mcp_config) = registry_obj.get("mcp") {
                    // Check if servers are configured
                    if let Some(servers) = mcp_config.get("servers") {
                        if let Some(server_list) = servers.as_object() {
                            for (server_name, _) in server_list {
                                // Check if the command exists on PATH
                                if command_exists(server_name) {
                                    println!("  ✓ {server_name} found on PATH");
                                } else {
                                    println!("  ✗ {server_name} not found on PATH");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn command_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .is_ok_and(|output| output.status.success())
}
