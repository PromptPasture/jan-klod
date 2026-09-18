#!/bin/sh
# Self-test for scripts/ext-new.sh.
#
# Two claims, and they are different claims:
#
#   1. Every kind generates a crate naming the right world and the right trait,
#      and every kind with a TypeScript or Python template names what its
#      world expects. Cheap — no build — so it covers all nine templates on
#      every `make test`, where compiling them all would not be affordable
#      enough to run that often.
#
#   2. The committed `tool-hello` is still **exactly** what the generator emits.
#      That is the drift check. Without it the committed example slowly becomes
#      a crate somebody hand-edited, and the template it is supposed to
#      demonstrate is whatever the generator does now — two different things
#      with one name. The same shape `protocol.schema.json`'s drift test uses:
#      regenerate, compare, fail on a difference.
#
# Claim 2 is why `tool-hello` is committed at all. Claim 1 is why the other four
# kinds are not: a text check catches a broken template, and `make extensions`
# catches a broken *build* for the one kind that is a real member.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "ext-new-selftest: every kind generates its own world and trait"

check_kind() {
    kind="$1"
    world="$2"
    trait="$3"
    name="probe-$kind"
    EXT_ROOT="$WORK/kinds" sh "$ROOT/scripts/ext-new.sh" "$name" "$kind" >/dev/null

    lib="$WORK/kinds/$name/src/lib.rs"
    grep -q "world: \"$world\"" "$lib" || {
        echo "ext-new-selftest: $kind does not bind $world" >&2
        exit 1
    }
    grep -q "impl $trait for Component" "$lib" || {
        echo "ext-new-selftest: $kind does not implement $trait" >&2
        exit 1
    }
    # No `todo!()`: a template whose first build panics teaches the wrong thing
    # first, and this is the only check that says so without compiling.
    # Comments stripped first: the module doc *mentions* `todo!()` to say the
    # stubs do not use one, and a naive grep reads its own explanation as the
    # thing it forbids.
    ! grep -v '^[[:space:]]*//' "$lib" | grep -q 'todo!' || {
        echo "ext-new-selftest: $kind left a todo!() in the template" >&2
        exit 1
    }
    printf '  %s -> %s / %s\n' "$kind" "$world" "$trait"
}

check_kind provider        provider-world       LlmProvider
check_kind tool            tool-world           ToolCallable
check_kind interceptor     interceptor-world    Interceptor
check_kind registry-skills skill-registry-world SkillRegistry
check_kind registry-mcp    mcp-registry-world   McpRegistry

echo "ext-new-selftest: the TypeScript templates export what their world wants"

# The TS path is claim 1 only. There is no claim-2 twin for it: the committed
# `tool-hello-ts` greets by name so the gate can find its words in a
# transcript, and a generated stub that did that would be a template teaching
# a behaviour instead of a shape. `tool-hello` mirrors the generator;
# `tool-hello-ts` does not, on purpose.
check_ts_kind() {
    kind="$1"
    export_name="$2"
    name="probe-ts-$kind"
    EXT_ROOT="$WORK/ts" sh "$ROOT/scripts/ext-new.sh" "$name" "$kind" ts >/dev/null

    guest="$WORK/ts/$name/src/guest.ts"
    for want in "export const extensionLifecycle" "export const $export_name"; do
        grep -q "$want" "$guest" || {
            echo "ext-new-selftest: ts/$kind is missing \`$want\`" >&2
            exit 1
        }
    done
    # `throw` is the TypeScript `todo!()`: it makes the first invocation a
    # trap the host reports as a guest crash. Comments stripped for the same
    # reason as above.
    ! grep -v '^[[:space:]]*[/*]' "$guest" | grep -q 'throw ' || {
        echo "ext-new-selftest: ts/$kind left a throw in the template" >&2
        exit 1
    }
    # The generator substitutes the name; a leftover placeholder would ship a
    # tool the model sees as NAME_PLACEHOLDER.
    ! grep -q 'NAME_PLACEHOLDER' "$guest" || {
        echo "ext-new-selftest: ts/$kind did not substitute NAME_PLACEHOLDER" >&2
        exit 1
    }
    grep -q "\"name\": \"$name\"" "$WORK/ts/$name/package.json" || {
        echo "ext-new-selftest: ts/$kind package.json does not name $name" >&2
        exit 1
    }
    printf '  ts/%s -> %s\n' "$kind" "$export_name"
}

check_ts_kind tool        toolCallable
check_ts_kind interceptor interceptor

echo "ext-new-selftest: a kind with no TypeScript template is refused"
if EXT_ROOT="$WORK/ts" sh "$ROOT/scripts/ext-new.sh" probe-ts-provider provider ts >/dev/null 2>&1; then
    echo "ext-new-selftest: LANG=ts KIND=provider was ACCEPTED — there is no stub" >&2
    exit 1
fi

echo "ext-new-selftest: the Python templates implement what their world wants"

# Same claim as the TypeScript block, one step stronger: `componentize-py`
# generates bindings, so the stub names classes that must exist in the
# generated `wit_world` package. Checked as text here for the same reason the
# Rust kinds are — a build per kind is not affordable on every `make test` —
# and the componentize step is what proves it for real.
check_py_kind() {
    kind="$1"
    class_name="$2"
    name="probe-py-$kind"
    EXT_ROOT="$WORK/py" sh "$ROOT/scripts/ext-new.sh" "$name" "$kind" py >/dev/null

    app="$WORK/py/$name/app.py"
    for want in "class ExtensionLifecycle(" "class $class_name(" "import wit_world"; do
        grep -q "$want" "$app" || {
            echo "ext-new-selftest: py/$kind is missing \`$want\`" >&2
            exit 1
        }
    done
    # `raise` is the Python `todo!()`. `NotImplementedError` in particular is
    # what the generated `Protocol`s raise, so a stub that inherited without
    # overriding would trap on first call and look like a host bug.
    ! grep -v '^[[:space:]]*#' "$app" | grep -q 'raise ' || {
        echo "ext-new-selftest: py/$kind left a raise in the template" >&2
        exit 1
    }
    ! grep -q 'NAME_PLACEHOLDER' "$app" || {
        echo "ext-new-selftest: py/$kind did not substitute NAME_PLACEHOLDER" >&2
        exit 1
    }
    grep -q "^name = \"$name\"" "$WORK/py/$name/pyproject.toml" || {
        echo "ext-new-selftest: py/$kind pyproject.toml does not name $name" >&2
        exit 1
    }
    # Syntax, which the other two languages get from their compilers here and
    # Python does not get until componentize time.
    python3 -c "import ast,sys; ast.parse(open(sys.argv[1]).read())" "$app" || {
        echo "ext-new-selftest: py/$kind is not valid Python" >&2
        exit 1
    }
    printf '  py/%s -> %s\n' "$kind" "$class_name"
}

check_py_kind tool        ToolCallable
check_py_kind interceptor Interceptor

echo "ext-new-selftest: a kind with no Python template is refused"
if EXT_ROOT="$WORK/py" sh "$ROOT/scripts/ext-new.sh" probe-py-provider provider py >/dev/null 2>&1; then
    echo "ext-new-selftest: LANG=py KIND=provider was ACCEPTED — there is no stub" >&2
    exit 1
fi

echo "ext-new-selftest: an unknown kind is refused"
if EXT_ROOT="$WORK/kinds" sh "$ROOT/scripts/ext-new.sh" probe-agent agent >/dev/null 2>&1; then
    echo "ext-new-selftest: KIND=agent was ACCEPTED — there is no agent world" >&2
    exit 1
fi

echo "ext-new-selftest: the committed tool-hello is what the generator emits"
EXT_ROOT="$WORK/drift" sh "$ROOT/scripts/ext-new.sh" tool-hello tool >/dev/null
for file in Cargo.toml src/lib.rs; do
    if ! diff -u "$ROOT/src/extensions/tool-hello/$file" "$WORK/drift/tool-hello/$file"; then
        echo "ext-new-selftest: src/extensions/tool-hello/$file has drifted from ext-new.sh" >&2
        echo "  Regenerate it rather than editing it by hand:" >&2
        echo "    rm -r src/extensions/tool-hello && make ext-new NAME=tool-hello KIND=tool" >&2
        exit 1
    fi
done

echo "ext-new-selftest: 5 Rust, 2 TypeScript, 2 Python kinds, 3 refusals and 0 drift, as required"
