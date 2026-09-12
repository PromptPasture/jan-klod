//! **The test template for a new extension — copy this file.**
//!
//! `make ext-new NAME=tool-yours KIND=tool` writes a crate that compiles. This
//! is the other half: a guest is not finished when it compiles, it is finished
//! when the host loads it and calls it. Discovering that pattern by reading the
//! rest of `src/core/host/tests/` is the afternoon the PDK exists to remove
//! ([#112](https://github.com/PromptPasture/jan-klod/issues/112)).
//!
//! # Copying this
//!
//! Three things are yours and everything else is harness:
//!
//! 1. `COMPONENT` — your guest's staged `.wasm`.
//! 2. `INSTANCE_ID` — the `category.name` the host would use, e.g. `tool.yours`.
//! 3. The assertions in [`the_generated_guest_answers_when_called`] — what your
//!    `invoke` returns, and what its `meta` advertises.
//!
//! Then add `mod your_module;` to `main.rs`. **Not a new file beside it**: that
//! file is one test binary on purpose, because `wasmtime` links statically and
//! every extra integration target is another multi-gigabyte link.
//!
//! # What it does not need
//!
//! No `Runtime`, no config, no provider. `ToolExtension::instantiate` loads a
//! component and runs its lifecycle directly, which is what you want while
//! writing a guest — a whole agent tells you less and takes longer to fail.
//! Loading through a booted runtime is covered separately by
//! `manifest.rs::a_generated_crate_boots`, which is where the manifest
//! cross-check happens.
//!
//! Offline. Skips (passes as a no-op) when the guest is not staged in `ext/`.

use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

/// Yours: the staged component this exercises.
///
/// `tool-hello` is what `make ext-new NAME=tool-hello KIND=tool` emits, kept
/// byte-identical to the generator's output by `scripts/ext-new-selftest.sh` —
/// so this file tests a *generated* guest rather than one that was generated
/// once and has been drifting since.
const COMPONENT: &str = "tool-hello.wasm";

/// Yours: the instance id the host would give it (`category.name`).
const INSTANCE_ID: &str = "tool.hello";

/// Harness: compile the staged component, or skip if it is not there.
fn component(engine: &Engine) -> Option<Component> {
    if !common::guests_staged(&[COMPONENT]) {
        return None;
    }
    let path = common::repo_root().join("ext").join(COMPONENT);
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

/// A generated guest loads, starts, and answers when it is called.
///
/// The assertions below are the ones to replace. Everything above them is the
/// same for any `tool-*` guest.
#[test]
fn the_generated_guest_answers_when_called() {
    let engine = Engine::default();
    let Some(component) = component(&engine) else {
        return;
    };

    // Instantiating runs `extension-lifecycle`'s `init` and `start`; a guest
    // that fails either does not get this far, so reaching the next line is
    // itself the lifecycle assertion.
    let mut guest = ToolExtension::instantiate(
        &engine,
        INSTANCE_ID,
        &component,
        // No workspace and no command runner: this guest was granted neither,
        // and handing it either would test a guest nobody will run.
        None,
        ProcessRunner::disabled(),
    )
    .expect("the generated guest instantiates and starts");

    // Yours: what your guest advertises to the model.
    let meta = guest.meta().expect("meta is readable");
    assert_eq!(meta.name, "tool-hello");
    assert!(
        !meta.description.is_empty(),
        "a tool with no description is one the model cannot choose: {meta:?}"
    );

    // Yours: what your `invoke` returns.
    let answer = guest.invoke("{}").expect("invoke succeeds");
    assert_eq!(
        answer, "replace me",
        "the generated stub answers; replace this assertion with yours"
    );
}
