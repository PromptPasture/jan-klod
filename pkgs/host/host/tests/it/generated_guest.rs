//! Template for a new extension: copy it and replace `COMPONENT`, `INSTANCE_ID`, and the assertions.

use jk_wasm::host_process::ProcessRunner;
use jk_wasm::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

const COMPONENT: &str = "tool-hello.wasm";

const INSTANCE_ID: &str = "tool.hello";

fn component(engine: &Engine) -> Option<Component> {
    if !common::guests_staged(&[COMPONENT]) {
        return None;
    }
    let path = common::repo_root().join("ext").join(COMPONENT);
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

#[test]
fn the_generated_guest_answers_when_called() {
    let engine = Engine::default();
    let Some(component) = component(&engine) else {
        return;
    };

    let mut guest = ToolExtension::instantiate(
        &engine,
        INSTANCE_ID,
        &component,
        None,
        ProcessRunner::disabled(),
    )
    .expect("the generated guest instantiates and starts");

    let meta = guest.meta().expect("meta is readable");
    assert_eq!(meta.name, "tool-hello");
    assert!(
        !meta.description.is_empty(),
        "a tool with no description is one the model cannot choose: {meta:?}"
    );

    let answer = guest.invoke("{}").expect("invoke succeeds");
    assert_eq!(
        answer, "replace me",
        "the generated stub answers; replace this assertion with yours"
    );
}
