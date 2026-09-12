//! How far `tool-fs`'s tree-wide grep is behind a native one (#61).
//!
//! **A measurement, not a test**, so it is `#[ignore]`d: it builds a synthetic
//! tree of tens of thousands of files and would cost the gate minutes for an
//! assertion it does not make. Run it deliberately:
//!
//! ```console
//! cd src/core && cargo test -p jan-klod-host --features jan-klod-host/integration \
//!   --test it grep_budget -- --ignored --nocapture
//! ```
//!
//! # Why the harness is committed rather than thrown away
//!
//! #61's first bullet is a gate — *if the gap is under 3× the issue closes as
//! "not needed"* — so the number is the deliverable either way. If the gap is
//! real, this is also what produces the before/after the issue asks to be
//! recorded; if it is not, this is the evidence for closing. A timing nobody
//! can retake is a claim, not a measurement.
//!
//! # What this measured, and why the shape changed
//!
//! #61's premise is that the guest-side walk is *slow*: "on a large repository
//! that is seconds where `ripgrep` is milliseconds". On a 50 000-file tree it is
//! not slow — it is **14 ms against ripgrep's 835 ms**, because it does not walk
//! the tree. `guest_fs::MAX_RESULTS` bounds the walk at 500 files and
//! `MAX_VISITS` at 20 000 entries, and `fs::render` says so in the output:
//! `[partial: the file walk stopped at the result cap]`, which since #145 leads
//! the hits rather than trailing them.
//!
//! So there are two measurements here, and only the first is a speed comparison:
//!
//! * `native_grep_is_faster_per_file_but_not_by_much` runs both over a tree
//!   **below** the walk bound, where they genuinely do the same work.
//! * `a_tree_wide_grep_does_not_walk_a_large_tree` pins the bound itself, which
//!   is the thing a user actually meets.
//!
//! # Fairness, which is most of the work
//!
//! Three ways this comparison lies if it is written carelessly:
//!
//! * **Different amounts of work.** `rg` skips `.gitignore`d and hidden files by
//!   default; the guest-side walk does not. The tree below contains neither, and
//!   `--no-ignore` is passed anyway, so both visit the same files.
//! * **Counting the compile.** Loading and compiling the component is not what
//!   is being measured, so it happens before the clock starts.
//! * **A cold cache.** Each side runs twice and the second run is reported.
//!
//! And one that would invalidate it outright: if the two disagree on how many
//! lines matched, they did not do the same search, so the timings compare
//! nothing. That is asserted rather than assumed.

use std::time::{Duration, Instant};

use jan_klod_core::conductor::ToolInvoker;
use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::intercept::ToolCall;
use jan_klod_core::tool_host::{ToolExtension, ToolFleet};
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

/// A tree the guest walk covers whole: below `guest_fs::MAX_RESULTS` (500), so
/// both sides see every file and the timings compare like with like.
const COVERED_FILES: usize = 400;
/// #61's "~50k-file tree". Above every bound the walk has.
const LARGE_FILES: usize = 50_000;
/// Spread over directories, because a flat directory of 50k entries is a
/// different filesystem problem from a tree and not the one being measured.
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
    // Counts, not the strings: ripgrep walks in parallel and emits hits in
    // whatever order its threads finish, so two identical searches differ as
    // text while agreeing on every fact this cares about.
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

    // Compile and instantiate before any clock starts: this is startup, not
    // search.
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

/// The speed comparison #61 asks for, on a tree small enough that both sides
/// genuinely walk all of it.
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

/// The thing a user actually meets on a large repository — and it is not
/// slowness.
///
/// `guest_fs::MAX_RESULTS` bounds the walk at 500 files, so a "tree-wide" grep
/// over 50 000 files answers from the first 500 of them. It is *honest* about
/// it: `fs::render` emits `[partial: the file walk stopped at the result cap]`.
/// But a caller who reads only the hits gets an answer that looks complete and
/// is not, which is a different problem from the one #61 was filed about and a
/// much cheaper one to fix.
///
/// #145 fixed the cheap half by moving that marker to the **front** of the
/// output — `guest_fs::truncate` cuts the tail, so a trailing marker was
/// deleted outright by a result long enough to overflow the byte cap. The
/// `contains("partial")` assertion below held before and after; what changed is
/// that it now also holds for the large results where it used not to.
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
