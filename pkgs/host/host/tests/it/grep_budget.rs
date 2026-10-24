//! Benchmark of `tool-fs`'s tree-wide grep against native `rg` (#61). Ignored by default; run with
//! `cd src && cargo test -p jk-host --features jk-host/integration --test it grep_budget -- --ignored --nocapture`.
//! Fair only if both sides visit the same files (`rg --no-ignore --hidden`) and their hit counts match.

use std::time::{Duration, Instant};

use jk_protocol::intercept::ToolInvoker;
use jk_protocol::turn::ToolCall;
use jk_wasm::host_fs::Workspace;
use jk_wasm::host_process::ProcessRunner;
use jk_wasm::tool_host::{ToolExtension, ToolFleet};
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

/// Below `guest_fs::MAX_RESULTS` (500), so both sides see every file.
const COVERED_FILES: usize = 400;
/// #61's "~50k-file tree". Above every walk bound.
const LARGE_FILES: usize = 50_000;
/// Spread over directories, so the test measures tree walking, not a flat directory.
const PER_DIR: usize = 250;

const NEEDLE: &str = "ZZQQ_NEEDLE_ZZQQ";

/// Build `files` files, every tenth one carrying the needle.
fn build_tree(root: &std::path::Path, files: usize) -> usize {
    let filler = "let padding = 1; // ordinary source-looking filler\n".repeat(4);
    let mut needles = 0;
    for i in 0..files {
        let dir = root.join(format!("d{:04}", i / PER_DIR));
        if i % PER_DIR == 0 {
            std::fs::create_dir_all(&dir).expect("tree dir");
        }
        let mut body = filler.clone();
        if i % 10 == 0 {
            body.push_str(NEEDLE);
            body.push('\n');
            needles += 1;
        }
        std::fs::write(dir.join(format!("f{i:05}.rs")), &body).expect("tree file");
    }
    needles
}

/// Run twice, report the second — the first pays for a cold page cache.
fn time_it(mut run: impl FnMut() -> String) -> (Duration, String) {
    let warm = run();
    let started = Instant::now();
    let out = run();
    assert_eq!(
        hits(&warm),
        hits(&out),
        "the same search found a different number of lines twice"
    );
    (started.elapsed(), out)
}

/// Everything the two searches need, built once per scenario.
struct Bench {
    _guard: common::TempDir,
    root: std::path::PathBuf,
    fleet: ToolFleet,
}

fn setup(tag: &str, files: usize) -> Option<(Bench, usize)> {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        eprintln!("grep_budget: tool-fs.wasm is not staged — run `make extensions` first");
        return None;
    }
    if std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("grep_budget: `rg` is not on PATH, so there is nothing to compare against");
        return None;
    }

    let root = std::env::temp_dir().join(format!("jk-grep-{tag}-{}", std::process::id()));
    let guard = common::TempDir(root.clone());
    std::fs::create_dir_all(&root).expect("workspace");
    let built = Instant::now();
    let needles = build_tree(&root, files);
    eprintln!(
        "grep_budget[{tag}]: {files} files, {needles} carrying the needle, built in {:?}",
        built.elapsed()
    );

    let engine = Engine::default();
    let component = Component::from_file(&engine, common::repo_root().join("ext/tool-fs.wasm"))
        .expect("component compiles");
    let workspace = Workspace::open(&root).expect("workspace opens");
    let fs = ToolExtension::instantiate(
        &engine,
        "tool.fs",
        &component,
        Some(workspace),
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");
    Some((
        Bench {
            _guard: guard,
            root,
            fleet: ToolFleet::new(vec![fs]),
        },
        needles,
    ))
}

impl Bench {
    fn guest(&mut self) -> String {
        self.fleet
            .invoke(&ToolCall {
                id: "1".into(),
                name: "fs".into(),
                arguments: format!(r#"{{"op":"grep","pattern":"{NEEDLE}"}}"#),
            })
            .expect("fs dispatched")
            .content
    }

    fn native(&self) -> String {
        let out = std::process::Command::new("rg")
            .args(["--no-ignore", "--hidden", NEEDLE])
            .current_dir(&self.root)
            .output()
            .expect("rg runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn hits(output: &str) -> usize {
    output.lines().filter(|l| l.contains(NEEDLE)).count()
}

/// The speed comparison #61 asks for, on a tree both sides walk fully.
#[test]
#[ignore = "a benchmark — run it deliberately (#61)"]
fn native_grep_is_faster_per_file_but_not_by_much() {
    let Some((mut bench, needles)) = setup("covered", COVERED_FILES) else {
        return;
    };

    let (guest_time, guest_out) = time_it(|| bench.guest());
    let (native_time, native_out) = time_it(|| bench.native());

    assert_eq!(
        hits(&guest_out),
        needles,
        "the guest did not cover a tree below its own walk bound"
    );
    assert_eq!(
        hits(&native_out),
        needles,
        "ripgrep and the fixture disagree, so the fixture is wrong"
    );

    let ratio = guest_time.as_secs_f64() / native_time.as_secs_f64().max(f64::EPSILON);
    eprintln!("grep_budget[covered]: tool-fs {guest_time:?}   rg {native_time:?}   {ratio:.1}x");
    eprintln!("grep_budget[covered]: #61 closes as not-needed below 3x on this number");
}

/// The large-tree case, where the walk bound applies. The concern is not speed:
/// `guest_fs::MAX_RESULTS` caps the walk, so hits can look complete when they are
/// not. The `partial` marker is at the front of the output (#145) so a long result
/// cannot truncate it away.
#[test]
#[ignore = "a benchmark: builds a 50k-file tree, run it deliberately (#61)"]
fn a_tree_wide_grep_does_not_walk_a_large_tree() {
    let Some((mut bench, needles)) = setup("large", LARGE_FILES) else {
        return;
    };

    let started = Instant::now();
    let guest_out = bench.guest();
    let guest_time = started.elapsed();
    let started = Instant::now();
    let native_out = bench.native();
    let native_time = started.elapsed();

    let (guest_hits, native_hits) = (hits(&guest_out), hits(&native_out));
    eprintln!(
        "grep_budget[large]: tool-fs {guest_time:?} ({guest_hits} hits)   \
         rg {native_time:?} ({native_hits} hits)   of {needles} present"
    );

    assert_eq!(
        native_hits, needles,
        "ripgrep and the fixture disagree, so the fixture is wrong"
    );
    assert!(
        guest_hits < native_hits,
        "the walk bound did not bind at {LARGE_FILES} files — if guest-fs grew a \
         bigger budget, this test is the record that it used to be 500 and should \
         be updated rather than deleted"
    );
    assert!(
        guest_out.contains("partial"),
        "a bounded walk that does not say so is the failure mode worth guarding: \
         the hits would read as all of them"
    );
    assert!(
        guest_time < native_time,
        "the guest is faster here only because it does less work, which is the \
         whole point of this test"
    );
}
