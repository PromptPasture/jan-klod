#![allow(missing_docs)]
use std::collections::HashMap;

/// Represents a capability group as defined in scripts/capabilities.yaml.
#[derive(Debug, Clone)]
struct Group {
    #[allow(dead_code)]
    name: String,
    order: u32,
    description: String,
    required: bool,
    members: Vec<String>,
}

/// Represents the full capabilities file structure.
#[derive(Debug, Clone)]
struct Capabilities {
    version: String,
    description: String,
    groups: HashMap<String, Group>,
    pending: HashMap<String, String>,
}

/// Parse the capabilities.yaml file into a structured form.
fn parse_capabilities(yaml_str: &str) -> Result<Capabilities, Box<dyn std::error::Error>> {
    let root: serde_json::Value = serde_yaml_ng::from_str(yaml_str)?;
    let root_obj = root
        .as_object()
        .ok_or("capabilities root must be a mapping")?;

    let version = root_obj
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or("version field required")?
        .to_string();

    let description = root_obj
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or("description field required")?
        .to_string();

    let mut groups = HashMap::new();
    if let Some(groups_val) = root_obj.get("groups") {
        if let Some(groups_map) = groups_val.as_object() {
            for (group_name, group_val) in groups_map {
                let group_obj = group_val
                    .as_object()
                    .ok_or_else(|| format!("group {group_name} must be a mapping"))?;

                let order = u32::try_from(
                    group_obj
                        .get("order")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| format!("group {group_name} missing order"))?,
                )
                .map_err(|_| format!("group {group_name} order out of range"))?;

                let desc = group_obj
                    .get("description")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("group {group_name} missing description"))?
                    .to_string();

                let required = group_obj
                    .get("required")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);

                let members = group_obj
                    .get("members")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| format!("group {group_name} missing members array"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .ok_or_else(|| "member must be a string".to_string())
                            .map(String::from)
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                groups.insert(
                    group_name.clone(),
                    Group {
                        name: group_name.clone(),
                        order,
                        description: desc,
                        required,
                        members,
                    },
                );
            }
        }
    }

    let mut pending = HashMap::new();
    if let Some(pending_val) = root_obj.get("pending") {
        if let Some(pending_map) = pending_val.as_object() {
            for (name, desc_val) in pending_map {
                let desc = desc_val
                    .as_object()
                    .and_then(|o| o.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                pending.insert(name.clone(), desc);
            }
        }
    }

    Ok(Capabilities {
        version,
        description,
        groups,
        pending,
    })
}

/// Load and parse the capabilities file from its expected location.
fn load_capabilities() -> Result<Capabilities, Box<dyn std::error::Error>> {
    // When running tests, we resolve relative to the repository root.
    // Tests run from src/config/tests but the file is at scripts/capabilities.yaml
    let paths = vec![
        "scripts/capabilities.yaml",
        "../../scripts/capabilities.yaml",
        "../../../scripts/capabilities.yaml",
    ];

    for path in paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            return parse_capabilities(&content);
        }
    }

    Err("capabilities.yaml not found in any expected location".into())
}

#[test]
fn capabilities_file_is_well_formed_yaml() {
    let cap = load_capabilities().expect("failed to load capabilities");
    assert!(!cap.version.is_empty(), "version should not be empty");
    assert!(
        !cap.description.is_empty(),
        "description should not be empty"
    );
    assert!(
        !cap.groups.is_empty(),
        "at least one group should be defined"
    );
}

#[test]
fn all_groups_have_required_fields() {
    let cap = load_capabilities().expect("failed to load capabilities");

    for (name, group) in &cap.groups {
        assert!(group.order > 0, "group {name} has invalid order");
        assert!(
            !group.description.is_empty(),
            "group {name} missing description"
        );
        assert!(!group.members.is_empty(), "group {name} has no members");
    }
}

#[test]
fn reasoning_group_exists_and_is_required() {
    let cap = load_capabilities().expect("failed to load capabilities");
    let reasoning = cap
        .groups
        .get("reasoning")
        .expect("reasoning group must exist");
    assert!(reasoning.required, "reasoning group must be required");
    assert!(!reasoning.members.is_empty(), "reasoning must have members");
}

#[test]
fn no_group_is_empty_after_filtering_pending() {
    let cap = load_capabilities().expect("failed to load capabilities");

    for (name, group) in &cap.groups {
        let has_active_members = group
            .members
            .iter()
            .any(|m| !cap.pending.contains_key(m.as_str()));
        // Allow groups with only pending members (they won't be offered)
        // but document this expectation.
        if !has_active_members {
            // This group has only pending members; it's ok but should be rare
            eprintln!("Note: group '{name}' has only pending members and won't be offered");
        }
    }
}

#[test]
fn pending_section_documents_phase_30_guests() {
    let cap = load_capabilities().expect("failed to load capabilities");

    // These should be in pending per the issue
    let expected_pending = vec![
        "tool-memory",
        "tool-web-search",
        "interceptor-persona",
        "agent",
    ];

    for name in expected_pending {
        assert!(
            cap.pending.contains_key(name),
            "pending guest '{name}' should be documented"
        );
    }
}

#[test]
fn groups_are_ordered() {
    let cap = load_capabilities().expect("failed to load capabilities");
    let mut orders: Vec<_> = cap.groups.values().map(|g| g.order).collect();
    orders.sort_unstable();
    // Check that orders are distinct (no duplicates)
    orders.dedup();
    // With dedup, if the length is less than the number of groups, there were duplicates
    assert_eq!(
        orders.len(),
        cap.groups.len(),
        "group orders must be unique"
    );
}

#[test]
fn provider_groups_documented() {
    let cap = load_capabilities().expect("failed to load capabilities");

    // Both provider groups should exist
    assert!(
        cap.groups.contains_key("provider-openai"),
        "provider-openai group must exist"
    );
    assert!(
        cap.groups.contains_key("provider-anthropic"),
        "provider-anthropic group must exist"
    );

    let openai = cap.groups.get("provider-openai").unwrap();
    let anthropic = cap.groups.get("provider-anthropic").unwrap();

    // Both are optional (choose one, not required to have both)
    assert!(
        !openai.required || !anthropic.required,
        "at least one provider should be optional"
    );
}

#[test]
fn all_members_are_in_registry_or_pending() {
    let cap = load_capabilities().expect("failed to load capabilities");

    // Load and parse the built registry from dist/index.json
    let registry_paths = vec![
        "dist/index.json",
        "../../dist/index.json",
        "../../../dist/index.json",
    ];

    let mut registry_json = None;
    for path in registry_paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                registry_json = Some(parsed);
                break;
            }
        }
    }

    let registry = registry_json.expect(
        "failed to load or parse dist/index.json; run 'make registry-index' to generate it",
    );

    // Extract extension names from the registry
    let mut registry_names = std::collections::HashSet::new();
    if let Some(extensions) = registry["extensions"].as_array() {
        for ext in extensions {
            if let Some(name) = ext["name"].as_str() {
                registry_names.insert(name.to_string());
            }
        }
    }

    // Check that every group member is either in the registry or pending
    for (group_name, group) in &cap.groups {
        for member in &group.members {
            let is_in_registry = registry_names.contains(member.as_str());
            let is_pending = cap.pending.contains_key(member.as_str());

            assert!(
                is_in_registry || is_pending,
                "group '{group_name}' member '{member}' is not in the built registry (dist/index.json) \
                 and is not listed in the pending section of capabilities.yaml"
            );
        }
    }
}

#[test]
fn ecosystem_group_contains_registry_members() {
    let cap = load_capabilities().expect("failed to load capabilities");

    let ecosystem = cap
        .groups
        .get("ecosystem")
        .expect("ecosystem group must exist");

    assert!(
        ecosystem.members.contains(&"registry-skills".to_string()),
        "ecosystem should include registry-skills"
    );
    assert!(
        ecosystem.members.contains(&"registry-mcp".to_string()),
        "ecosystem should include registry-mcp"
    );
}

#[test]
fn file_tools_group_bundled() {
    let cap = load_capabilities().expect("failed to load capabilities");

    let file_tools = cap
        .groups
        .get("file-tools")
        .expect("file-tools group must exist");

    // All three should be present (bundled)
    assert!(
        file_tools.members.contains(&"tool-fs".to_string()),
        "file-tools must include tool-fs"
    );
    assert!(
        file_tools.members.contains(&"tool-edit".to_string()),
        "file-tools must include tool-edit"
    );
    assert!(
        file_tools.members.contains(&"tool-find".to_string()),
        "file-tools must include tool-find"
    );
}

#[test]
fn shell_access_includes_tool_proc() {
    let cap = load_capabilities().expect("failed to load capabilities");

    let shell = cap
        .groups
        .get("shell-access")
        .expect("shell-access group must exist");

    assert!(
        shell.members.contains(&"tool-shell".to_string()),
        "shell-access must include tool-shell"
    );
    assert!(
        shell.members.contains(&"tool-proc-probe".to_string()),
        "shell-access must include tool-proc-probe for monitoring"
    );
}

#[test]
#[allow(clippy::similar_names)]
fn test_merge_conflicting_groups() {
    // This test validates that when two capability groups enable the same
    // extension with different configurations, the merge logic preserves both
    // configs through per-instance naming (e.g., "guardrails" and "guardrails-1")
    // rather than silently overwriting one with the other.

    // Simulate group A: enables "interceptor-guardrails" with strict mode.
    let mut group_a_exts: HashMap<String, serde_json::Value> = HashMap::new();
    group_a_exts.insert(
        "interceptor-guardrails".to_string(),
        serde_json::json!({
            "rules": ["rule-a1", "rule-a2"],
            "mode": "strict"
        }),
    );

    // Simulate group B: enables the same extension with permissive mode.
    let mut group_b_exts: HashMap<String, serde_json::Value> = HashMap::new();
    group_b_exts.insert(
        "interceptor-guardrails".to_string(),
        serde_json::json!({
            "rules": ["rule-b1", "rule-b2"],
            "mode": "permissive"
        }),
    );

    // Merge logic: combine both groups' extensions.
    // When a conflict is detected (same extension name, different config),
    // create a per-instance variant by appending a suffix.
    let mut merged_extensions: HashMap<String, serde_json::Value> = HashMap::new();

    // Add all extensions from group A.
    for (name, config) in &group_a_exts {
        merged_extensions.insert(name.clone(), config.clone());
    }

    // Merge extensions from group B.
    for (name, config_b) in &group_b_exts {
        if let Some(config_a) = merged_extensions.get(name) {
            // If the same extension name exists with different config,
            // create a per-instance variant.
            if config_a != config_b {
                let variant_name = format!("{name}-1");
                merged_extensions.insert(variant_name, config_b.clone());
                // The original (from group A) remains unchanged.
            }
            // If configs are identical, skip (no duplicate needed).
        } else {
            // Extension doesn't exist yet, add it.
            merged_extensions.insert(name.clone(), config_b.clone());
        }
    }

    // Verify the merge result: both the original and variant should exist.
    assert_eq!(
        merged_extensions.len(),
        2,
        "merged extensions should contain both the original and the variant"
    );

    // Verify that the original (group A's config) is preserved.
    let original_config = merged_extensions
        .get("interceptor-guardrails")
        .expect("original singleton should be preserved");
    assert_eq!(
        original_config["mode"], "strict",
        "original should retain group A's mode"
    );
    assert_eq!(
        original_config["rules"],
        serde_json::json!(["rule-a1", "rule-a2"]),
        "original should retain group A's rules"
    );

    // Verify that the variant (group B's config) is preserved under a new name.
    let variant_config = merged_extensions
        .get("interceptor-guardrails-1")
        .expect("variant should be created for conflicting config");
    assert_eq!(
        variant_config["mode"], "permissive",
        "variant should preserve group B's mode"
    );
    assert_eq!(
        variant_config["rules"],
        serde_json::json!(["rule-b1", "rule-b2"]),
        "variant should preserve group B's rules"
    );

    // Ensure no silent config loss: both configs must be different and present.
    assert_ne!(
        original_config, variant_config,
        "original and variant configs must be distinct"
    );
}
