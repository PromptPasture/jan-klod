#!/bin/sh
# Generate a new extension crate, registered and ready to build.
#
# Usage: scripts/ext-new.sh <name> <kind>
#   kind ∈ provider | tool | interceptor | registry-skills | registry-mcp
#
# Writes src/extensions/<name>/, then registers the crate in the extensions
# workspace and in the Makefile's GUESTS — so `make extensions` picks it up with
# no second manual step, and `scripts/manifests.sh` generates its manifest from
# its real imports.
#
# ## Why shell rather than an xtask
#
# What differs between kinds is one `world:` line and one trait impl. A Rust
# xtask would need its own crate, its own build and its own place in the
# workspace to do string substitution that `sed` already does — and it would be
# the only build tool here that is not a Makefile target calling a script. If
# the per-kind table ever grows logic rather than text, revisit.
#
# ## Why the stubs compile
#
# A template whose first build fails teaches the wrong thing first. Every stub
# returns a real value rather than `todo!()`: an author deletes a body and writes
# theirs, instead of chasing a panic through a toolchain they have not learned
# yet.
set -eu

NAME="${1:-}"
KIND="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXT="$ROOT/src/extensions"

usage() {
    echo "usage: ext-new.sh <name> <kind>" >&2
    echo "  kind ∈ provider | tool | interceptor | registry-skills | registry-mcp" >&2
    exit 2
}

[ -n "$NAME" ] && [ -n "$KIND" ] || usage
case "$NAME" in
    *[!a-z0-9-]* | "" | -* ) echo "ext-new: name must be lowercase letters, digits and dashes: $NAME" >&2; exit 2 ;;
esac
[ -e "$EXT/$NAME" ] && { echo "ext-new: $EXT/$NAME already exists" >&2; exit 2; }

# The world each kind binds, the interface it exports beside
# extension-lifecycle, and the types that interface's stub names. This table
# *is* the difference between kinds; everything below it is identical for all
# five.
case "$KIND" in
    provider)
        WORLD="provider-world";       IFACE="llm_provider";   TRAIT="LlmProvider"
        TYPES="CompletionChunk, CompletionRequest, ProviderError, ProviderInfo, StreamHandle" ;;
    tool)
        WORLD="tool-world";           IFACE="tool_callable";  TRAIT="ToolCallable"
        TYPES="ToolError, ToolMeta" ;;
    interceptor)
        WORLD="interceptor-world";    IFACE="interceptor";    TRAIT="Interceptor"
        TYPES="Decision, InterceptInput, InterceptorError, Phase" ;;
    registry-skills)
        WORLD="skill-registry-world"; IFACE="skill_registry"; TRAIT="SkillRegistry"
        TYPES="SkillError, SkillInfo" ;;
    registry-mcp)
        WORLD="mcp-registry-world";   IFACE="mcp_registry";   TRAIT="McpRegistry"
        TYPES="McpError, ServerInfo, ToolInfo" ;;
    *) echo "ext-new: unknown kind: $KIND" >&2; usage ;;
esac

mkdir -p "$EXT/$NAME/src"

cat > "$EXT/$NAME/Cargo.toml" <<EOF
[package]
name = "$NAME"
version = "0.1.0"
edition.workspace = true
license.workspace = true
publish.workspace = true
description = "A $KIND extension. Replace this description with what it does."

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen.workspace = true

[lints]
workspace = true
EOF

# --- the exported interface's stub body, per kind ----------------------------
stub_tool() {
    cat <<'EOF'
    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "NAME_PLACEHOLDER".to_string(),
                description: "Describe what this tool does — the model reads this.".to_string(),
                // JSON Schema for `invoke`'s argument string.
                arguments_schema: r#"{"type":"object","properties":{}}"#.to_string(),
            }
        }

        fn invoke(_arguments: String) -> Result<String, ToolError> {
            Ok("replace me".to_string())
        }
    }
EOF
}

stub_provider() {
    cat <<'EOF'
    impl LlmProvider for Component {
        fn complete(_request: CompletionRequest) -> Result<StreamHandle, ProviderError> {
            Err(ProviderError::Unreachable)
        }

        fn next_chunk(_handle: StreamHandle) -> Option<CompletionChunk> {
            None
        }

        fn close_stream(_handle: StreamHandle) {}

        fn info() -> ProviderInfo {
            ProviderInfo {
                id: "NAME_PLACEHOLDER".to_string(),
                supported_models: Vec::new(),
            }
        }
    }
EOF
}

stub_interceptor() {
    cat <<'EOF'
    impl Interceptor for Component {
        fn intercept(_input: InterceptInput) -> Result<Decision, InterceptorError> {
            // `Proceed` changes nothing, which is the right default: an
            // interceptor that has not decided anything must not alter the turn.
            Ok(Decision::Proceed)
        }

        fn subscribed_phases() -> Vec<Phase> {
            // Nothing yet. Subscribe only to the phases you act on — the host
            // calls you for every one you name.
            Vec::new()
        }
    }
EOF
}

stub_registry_skills() {
    cat <<'EOF'
    impl SkillRegistry for Component {
        fn list_skills() -> Result<Vec<SkillInfo>, SkillError> {
            Ok(Vec::new())
        }

        fn get_skill(_name: String) -> Result<SkillInfo, SkillError> {
            Err(SkillError::NotFound)
        }

        fn invoke(_name: String, _arguments: String) -> Result<String, SkillError> {
            Err(SkillError::NotFound)
        }

        fn reload() -> Result<(), SkillError> {
            Ok(())
        }
    }
EOF
}

stub_registry_mcp() {
    cat <<'EOF'
    impl McpRegistry for Component {
        fn list_servers() -> Result<Vec<ServerInfo>, McpError> {
            Ok(Vec::new())
        }

        fn list_tools() -> Result<Vec<ToolInfo>, McpError> {
            Ok(Vec::new())
        }

        fn invoke_tool(_name: String, _arguments: String) -> Result<String, McpError> {
            Err(McpError::ToolNotFound)
        }

        fn reconnect(_server_id: String) -> Result<(), McpError> {
            Err(McpError::ServerNotFound)
        }
    }
EOF
}

case "$KIND" in
    provider)        STUB="$(stub_provider)" ;;
    tool)            STUB="$(stub_tool)" ;;
    interceptor)     STUB="$(stub_interceptor)" ;;
    registry-skills) STUB="$(stub_registry_skills)" ;;
    registry-mcp)    STUB="$(stub_registry_mcp)" ;;
esac
STUB="$(printf '%s' "$STUB" | sed "s/NAME_PLACEHOLDER/$NAME/g")"

cat > "$EXT/$NAME/src/lib.rs" <<EOF
//! \`$NAME\` — a $KIND extension.
//!
//! Generated by \`make ext-new NAME=$NAME KIND=$KIND\`. Replace the stubs below;
//! they return real values rather than \`todo!()\` so this compiles from the
//! first build.
//!
//! Component-Model glue only, so it compiles for \`wasm32\` only.

#[cfg(target_arch = "wasm32")]
mod component {
    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod bindings {
        wit_bindgen::generate!({
            world: "$WORLD",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    // Named, not a glob. \`clippy::wildcard-imports\` is denied workspace-wide,
    // so a \`::*\` here would make the generated crate fail the repository's own
    // lints before its author had written a line — and the trait is aliased
    // because wit-bindgen calls every exported interface's trait \`Guest\`,
    // including the lifecycle one above.
    use bindings::exports::jan_klod::interfaces::$IFACE::{Guest as $TRAIT, $TYPES};
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "$NAME", message, &[]);
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(
                LogLevel::Info,
                &format!("init id={} version={}", ctx.id, ctx.version),
            );
            Ok(())
        }
        fn start() -> Result<(), String> {
            Ok(())
        }
        fn stop() {}
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

$STUB

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
EOF

# --- registration ------------------------------------------------------------
#
# Both lists are flat text. Idempotent: re-registering an existing name would
# build it twice and `manifests.sh` would stage it twice, so each edit checks
# first.
if ! grep -q "^    \"$NAME\"," "$EXT/Cargo.toml"; then
    # After the last existing member, so the list stays where it was.
    awk -v name="$NAME" '
        /^]/ && !done { print "    \"" name "\","; done = 1 }
        { print }
    ' "$EXT/Cargo.toml" > "$EXT/Cargo.toml.tmp" && mv "$EXT/Cargo.toml.tmp" "$EXT/Cargo.toml"
fi

if ! grep -q "GUESTS :=.*[ =]$NAME\( \|$\)" "$EXT/Makefile"; then
    awk -v name="$NAME" '
        /^GUESTS :=/ && !done { print $0 " " name; done = 1; next }
        { print }
    ' "$EXT/Makefile" > "$EXT/Makefile.tmp" && mv "$EXT/Makefile.tmp" "$EXT/Makefile"
fi

# Format what was just written, rather than hoping the template happens to be
# canonical. Acceptance line 2 of #111 asks that a generated crate pass
# `fmt --check` **untouched**, and the import line's length depends on how many
# types a kind names — so no single hand-written layout satisfies every kind.
# Formatting here makes that guarantee hold for kinds nobody has added yet.
#
# After registration, because `-p` needs the crate to be a workspace member.
cargo fmt --manifest-path "$EXT/Cargo.toml" -p "$NAME"

echo "ext-new: wrote $EXT/$NAME ($KIND, $WORLD)"
echo "ext-new: registered in the extensions workspace and GUESTS"
echo "ext-new: next — \`make extensions\` builds it and generates its manifest"
