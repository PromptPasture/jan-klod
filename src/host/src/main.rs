//! `jan-klod` core entrypoint: boot the runtime from `config.yaml`, resolve the
//! enabled extensions against the `ext/` directory, and either print the boot plan
//! or serve the loop over the host-side REST surface. All behaviour lives in the
//! extensions it loads — this binary is just the container.
//!
//! Usage:
//!   `jan-klod [config-path] [ext-dir]`            — boot + print the plan
//!   `jan-klod serve [config-path] [ext-dir] [bind]` — boot + serve REST turns
//!   `jan-klod serve --bind <addr>`                  — serve, resolving paths
//!   `jan-klod rpc [config-path] [ext-dir]`        — boot + serve JSON-RPC on stdio
//!   `jan-klod verify [config-path] [ext-dir]`     — check the install, then exit
//!
//!   config-path  path to config.yaml   (default: config.yaml)
//!   ext-dir      directory of *.wasm    (default: ext)
//!   bind         host:port to listen on (default: 127.0.0.1:8787)

use std::path::PathBuf;
use std::process::ExitCode;

use jan_klod_core::ext_index;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

/// Where the installed copies of `config.yaml` and `ext/` live, relative to the
/// gateway binary: `<prefix>/bin/jan-klod-gateway` → `<prefix>/share/jan-klod/`.
const INSTALLED_DATA: &str = "../share/jan-klod";

/// Resolve a default config/ext path.
///
/// The working directory wins, so running inside a checkout uses that checkout.
/// Otherwise the copy installed alongside the binary is used — a coding agent's
/// whole point is to `cd` into *your* repository, not one that happens to have
/// `config.yaml`/`ext/` in it.
fn resolve_default(name: &str) -> String {
    let in_cwd = PathBuf::from(name);
    if in_cwd.exists() {
        return name.to_string();
    }
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(INSTALLED_DATA).join(name)));
    match installed {
        Some(path) if path.exists() => path.to_string_lossy().into_owned(),
        // Neither exists: keep the plain name so the error names what was looked
        // for rather than an absolute path the user never typed.
        _ => name.to_string(),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("rpc") => rpc(&args[1..]),
        // Internal: how the Linux sandbox confines a command. Deliberately
        // undocumented — an operator has no reason to run it, and the docs check
        // requires documented commands to exist, not the reverse.
        Some(jan_klod_core::sandbox_landlock::CONFINE_SUBCOMMAND) => confine(&args[1..]),
        Some("telegram") => telegram(&args[1..]),
        Some("verify") => verify(&args[1..]),
        Some("ask") => ask(&args[1..]),
        Some("ext") => ext(&args[1..]),
        Some("mcp") => mcp(),
        Some("acp") => acp(),
        _ => boot_plan(&args),
    }
}

/// Serve the Agent Client Protocol on stdin/stdout, agent side.
///
/// Boots the same runtime `ask` does, because `session/prompt` runs a real turn
/// and streams `session/update` notifications as it goes.
///
/// The pipe is not yet *read* during a turn. It has to be once the agent asks
/// the editor for a permission answer — ACP's agent→client direction — and that
/// is where `rpc`'s reader-thread shape comes in.
fn acp() -> ExitCode {
    let config_path = resolve_default("config.yaml");
    let ext_dir = resolve_default("ext");
    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let input = std::io::BufReader::new(std::io::stdin());
    let output = std::io::stdout();
    match jan_klod_host::acp::serve(input, output, &mut agent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jan-klod: acp: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Serve the Model Context Protocol on stdin/stdout.
///
/// Boots the same runtime `ask` does, because `tools/call ask` runs a real
/// turn. The driver is **headless by construction** — see
/// `jan_klod_core::HeadlessDriver`: stdin here carries protocol frames, so
/// anything that prompted would read a frame as an answer.
///
/// Frames on stdout and nothing else, per the MCP stdio transport — which is
/// the same rule `rpc` follows, so logs and errors go to stderr.
fn mcp() -> ExitCode {
    let config_path = resolve_default("config.yaml");
    let ext_dir = resolve_default("ext");
    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let input = std::io::BufReader::new(std::io::stdin());
    let mut output = std::io::stdout();
    match jan_klod_host::mcp::serve(input, &mut output, &mut agent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jan-klod: mcp: {err}");
            ExitCode::FAILURE
        }
    }
}

/// What is staged in `ext/`, and unmaking it.
///
/// `install` verifies before it copies — digest, signature over both files,
/// component validity, manifest consistency — because the build pipeline is not
/// behind the runtime sandbox, so whatever lands in `ext/` decides what runs
/// with the host's privileges. A subcommand that copied a file in and called
/// that installing would be `cp` with a longer name.
fn ext(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("list") => ext_list(&args[1..]),
        Some("search") => ext_search(&args[1..]),
        Some("remove") => ext_remove(&args[1..]),
        Some("install") => ext_install(&args[1..]),
        _ => {
            eprintln!("usage: jan-klod-gateway ext list [ext-dir] [--remote]");
            eprintln!("       jan-klod-gateway ext search [term]");
            eprintln!(
                "       jan-klod-gateway ext install <name|path.wasm|url> [ext-dir] \
                 [--sha256 <hex>] [--allow-unsigned]"
            );
            eprintln!("       jan-klod-gateway ext remove <name> [ext-dir]");
            ExitCode::FAILURE
        }
    }
}

/// The index `registry.url` names, and where it came from.
///
/// Both are returned because every refusal below wants to say *which* index it
/// was talking about: "nothing named tool-fs" is a different message when the
/// index is a local fixture than when it is a URL, and the operator is the one
/// who has to tell them apart.
fn configured_index() -> Result<(String, Vec<ext_index::Entry>), ext_index::IndexError> {
    let config_path = resolve_default("config.yaml");
    let source = ext_index::url_from_config_path(std::path::Path::new(&config_path))?;
    // The host's own policy-bound client, so a redirect to a private address
    // ends the fetch rather than being followed on trust. A `registry.url` that
    // is not http(s) is a path, and nothing is fetched at all.
    let http = jan_klod_core::ext::policy_bound_http();
    let entries = ext_index::load(&source, &http)?;
    Ok((source, entries))
}

/// Print one index entry: what it is, then **what it asks the host for**.
///
/// The capability line gets its own line rather than a column, and that is the
/// point of the command rather than a formatting preference: until an
/// interactive Configurator exists this is the capability view, and a
/// capability list truncated into a table is one an operator skims past.
fn show_entry(entry: &ext_index::Entry) {
    // Whether anything vouches for these bytes, said out loud. A blank column
    // here would read as "no information"; `unsigned` is information.
    let provenance = if entry.signature.is_empty() {
        "unsigned"
    } else {
        "signed"
    };
    println!(
        "{}  {}  api {}  {}  {} bytes  {provenance}",
        entry.name, entry.version, entry.api_version, entry.kind, entry.size
    );
    if entry.capabilities.is_empty() {
        // "needs nothing" is a claim the manifest makes, printed as one — the
        // same wording `ext list` uses for a staged component.
        println!("  needs nothing");
    } else {
        println!("  may use {}", entry.capabilities.join(", "));
    }
    if !entry.description.is_empty() {
        println!("  {}", entry.description);
    }
}

/// Search the registry index for components whose name, kind or description
/// matches, and print what each one asks for.
fn ext_search(args: &[String]) -> ExitCode {
    // No term is every entry rather than an error: `ext search` with nothing to
    // filter by is the same question `ext list --remote` asks.
    let term = args.first().map_or("", String::as_str);
    let (source, entries) = match configured_index() {
        Ok(index) => index,
        Err(err) => {
            eprintln!("jan-klod: {err}");
            return ExitCode::FAILURE;
        }
    };
    let hits = ext_index::search(&entries, term);
    if hits.is_empty() {
        // Which index was searched, and how big it was. "No results" from an
        // index that turned out to be empty is a different problem from a term
        // that matched nothing, and the operator cannot see the difference.
        println!(
            "nothing matching {term:?} in the index at {source} ({} entries)",
            entries.len()
        );
        return ExitCode::SUCCESS;
    }
    for entry in hits {
        show_entry(entry);
    }
    ExitCode::SUCCESS
}

/// Install from whichever of the three kinds of source this is.
///
/// Told apart by shape: a URL fetches, a `.wasm` is a path, and anything else
/// is a **name** resolved through the registry index.
///
/// The `.wasm` test is `Path::extension`, the same rule
/// [`jan_klod_core::ext::install`] applies to decide a path is a component —
/// two different spellings of it would eventually disagree about a file, and
/// this one decides which of three code paths runs.
///
/// The name case is last because it is the one that cannot be settled locally:
/// `tool-fs` might be a typo for a path, and the index's refusal ("nothing
/// named tool-fs in the index at …, try `ext search`") is the more useful of
/// the two messages either way.
///
/// Every fetch uses the host's own policy-bound client, so a redirect is
/// re-checked per hop rather than followed on trust.
fn install_from_source(
    dir: &str,
    source: &str,
    checks: &jan_klod_core::ext::Checks,
) -> Result<jan_klod_core::ext::Installed, ext_index::IndexError> {
    let dir = std::path::Path::new(dir);
    if jan_klod_core::ext::looks_remote(source) {
        let http = jan_klod_core::ext::policy_bound_http();
        return Ok(jan_klod_core::ext::install_from_url(
            dir, source, checks, &http,
        )?);
    }
    let path = std::path::Path::new(source);
    if path.extension().and_then(|ext| ext.to_str()) == Some("wasm") {
        return Ok(jan_klod_core::ext::install(dir, path, checks)?);
    }
    let (index_source, entries) = configured_index()?;
    let http = jan_klod_core::ext::policy_bound_http();
    ext_index::install(dir, &entries, source, &index_source, checks, &http)
}

/// Install a component after checking it, or refuse and change nothing.
fn ext_install(args: &[String]) -> ExitCode {
    // `--sha256 <hex>`, pulled out before the positionals so a digest cannot be
    // mistaken for the extension directory.
    let mut sha256: Option<String> = None;
    let mut allow_unsigned = false;
    let mut positional: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        // `--sha256=<hex>` and `--sha256 <hex>` both: a digest is pasted, and
        // whichever form the paste came with should work.
        if let Some(inline) = arg.strip_prefix("--sha256=") {
            sha256 = Some(inline.to_owned());
        } else if arg == "--sha256" {
            let Some(value) = rest.next() else {
                eprintln!("jan-klod: --sha256 needs a digest");
                return ExitCode::FAILURE;
            };
            sha256 = Some(value.clone());
        } else if arg == "--allow-unsigned" {
            allow_unsigned = true;
        } else if arg.starts_with("--") {
            // Refused rather than ignored: a mistyped `--sha265` that were
            // silently dropped would install without the check the operator
            // believed they had asked for.
            eprintln!("jan-klod: unknown flag {arg}");
            return ExitCode::FAILURE;
        } else {
            positional.push(arg.clone());
        }
    }

    let Some(source) = positional.first() else {
        eprintln!(
            "usage: jan-klod-gateway ext install <name|path.wasm|url> [ext-dir] \
             [--sha256 <hex>] [--allow-unsigned]"
        );
        return ExitCode::FAILURE;
    };
    let dir = resolve_default(&arg_or(&positional, 1, "ext"));

    // The trusted keys are a *grant*, so they come from `config.yaml` rather
    // than from a flag: a key accepted on the command line would let whoever
    // supplies the component also supply the key that vouches for it.
    let config_path = resolve_default("config.yaml");
    let mut checks =
        match jan_klod_core::ext::Checks::from_config_path(std::path::Path::new(&config_path)) {
            Ok(checks) => checks,
            Err(err) => {
                eprintln!("jan-klod: {err}");
                return ExitCode::FAILURE;
            }
        };
    checks.sha256 = sha256;
    checks.allow_unsigned = allow_unsigned;

    match install_from_source(&dir, source, &checks) {
        Ok(installed) => {
            // What it may ask the host for, at the moment it is installed —
            // the one time an operator is certainly looking.
            match &installed.declaration {
                jan_klod_core::ext::Declaration::Present(manifest)
                    if manifest.capabilities.is_empty() =>
                {
                    println!("installed {} into {dir}/ — needs nothing", installed.name);
                }
                jan_klod_core::ext::Declaration::Present(manifest) => {
                    println!(
                        "installed {} into {dir}/ — may use {}",
                        installed.name,
                        manifest.capabilities.join(", ")
                    );
                }
                // `install` refuses both of these, so reaching here would mean
                // the manifest changed under us between landing and reading.
                jan_klod_core::ext::Declaration::Absent
                | jan_klod_core::ext::Declaration::Broken(_) => {
                    println!(
                        "installed {} into {dir}/, but its manifest no longer reads",
                        installed.name
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            // The chain, not just the head: each refusal has its own message
            // and the cause underneath it is what says which file was at fault.
            eprintln!("jan-klod: {err}");
            let mut cause = std::error::Error::source(&err);
            while let Some(inner) = cause {
                eprintln!("  caused by: {inner}");
                cause = inner.source();
            }
            ExitCode::FAILURE
        }
    }
}

/// Each staged component with the capabilities its manifest declares — or,
/// with `--remote`, everything the configured registry index publishes.
fn ext_list(args: &[String]) -> ExitCode {
    // Pulled out before the positional, so `--remote` cannot be mistaken for an
    // extension directory.
    let remote = args.iter().any(|arg| arg == "--remote");
    let positional: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .cloned()
        .collect();
    if remote {
        // The same traversal `ext search` does with nothing to filter by —
        // listing a registry and searching it with an empty term are one
        // question, and two implementations of it would be two answers.
        return ext_search(&[]);
    }

    let dir = resolve_default(&arg_or(&positional, 0, "ext"));
    let staged = match jan_klod_core::ext::list(std::path::Path::new(&dir)) {
        Ok(staged) => staged,
        Err(err) => {
            eprintln!("jan-klod: {err}");
            return ExitCode::FAILURE;
        }
    };
    if staged.is_empty() {
        println!("nothing staged in {dir}/");
        return ExitCode::SUCCESS;
    }
    let width = staged.iter().map(|s| s.name.len()).max().unwrap_or(0);
    for component in &staged {
        match &component.declaration {
            jan_klod_core::ext::Declaration::Present(manifest) => {
                let capabilities = if manifest.capabilities.is_empty() {
                    // "needs nothing" is a claim the manifest makes, so it is
                    // printed as one rather than as a blank column.
                    "needs nothing".to_owned()
                } else {
                    manifest.capabilities.join(", ")
                };
                println!(
                    "{:<width$}  {}  api {}  {}",
                    component.name, manifest.version, manifest.api_version, capabilities
                );
            }
            jan_klod_core::ext::Declaration::Absent => {
                println!(
                    "{:<width$}  no manifest — the next boot will refuse it",
                    component.name
                );
            }
            jan_klod_core::ext::Declaration::Broken(reason) => {
                println!("{:<width$}  unusable manifest: {reason}", component.name);
            }
        }
    }
    ExitCode::SUCCESS
}

/// Delete a component and its manifest together.
fn ext_remove(args: &[String]) -> ExitCode {
    let Some(name) = args.first() else {
        eprintln!("usage: jan-klod-gateway ext remove <name> [ext-dir]");
        return ExitCode::FAILURE;
    };
    let dir = resolve_default(&arg_or(args, 1, "ext"));
    match jan_klod_core::ext::remove(std::path::Path::new(&dir), name) {
        Ok(removed) => {
            // Which files went, not just "done": removing a manifest with no
            // component beside it is worth saying out loud, because it means
            // `ext/` had an orphan declaration.
            match removed {
                jan_klod_core::ext::Removed::Both => {
                    println!("removed {name} and its manifest from {dir}/");
                }
                jan_klod_core::ext::Removed::ComponentOnly => {
                    println!("removed {name} from {dir}/ — it had no manifest");
                }
                jan_klod_core::ext::Removed::ManifestOnly => {
                    println!("removed an orphan manifest for {name} from {dir}/");
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("jan-klod: {err}");
            ExitCode::FAILURE
        }
    }
}

/// One question, one answer, on stdout — no server, no client, nothing to leave
/// running: `jan-klod-gateway ask "what does this repo do?"` in a shell or CI
/// step.
///
/// Confirmations are asked on the terminal, because there is one. Running
/// headless and taking every prompt's default would deny every write and
/// command, making the whole fleet past `fs:read` unavailable here.
fn ask(args: &[String]) -> ExitCode {
    let question = args.join(" ");
    if question.trim().is_empty() {
        eprintln!("usage: jan-klod-gateway ask <question>");
        return ExitCode::FAILURE;
    }
    let config_path = resolve_default("config.yaml");
    let ext_dir = resolve_default("ext");
    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut driver = TerminalDriver;
    match agent.run_with_driver(&mut driver, "ask", &question) {
        jan_klod_core::conductor::RunResult::Answered { text, .. } => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        // Not `{other:?}`: the Debug of a Rust enum is not a diagnosis. A turn
        // that fails has a message written for a person; print that.
        jan_klod_core::conductor::RunResult::Failed(message) => {
            eprintln!("jan-klod: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Asks the person at the keyboard, which is the only reason `ask` can use tools
/// that write.
struct TerminalDriver;

impl jan_klod_core::intercept::Driver for TerminalDriver {
    fn ask(&mut self, prompt: &jan_klod_core::intercept::UserPrompt) -> String {
        // The question goes to stderr so `ask`'s stdout stays the answer and
        // nothing else — a script can pipe it without stripping prompts out.
        eprintln!("\n  ? {}", prompt.question);
        eprint!(
            "  [{}] (Enter = {}): ",
            prompt.options.join("/"),
            prompt.default_answer
        );
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut typed = String::new();
        match std::io::stdin().read_line(&mut typed) {
            // EOF (a pipe, a CI step) is not an approval.
            Ok(0) | Err(_) => prompt.default_answer.clone(),
            Ok(_) if typed.trim().is_empty() => prompt.default_answer.clone(),
            Ok(_) => typed.trim().to_string(),
        }
    }
}

/// Check that an install is complete: every extension the config enables resolves
/// to a component that is present and instantiates. Prints the plan and exits
/// non-zero if anything is missing or fails to start.
///
/// Otherwise a missing component is a silent degradation — the runtime skips
/// what it cannot find and starts happily, merely less capable than it claims.
fn verify(args: &[String]) -> ExitCode {
    // `--live` is opt-in because it spends a request: the offline checks are free
    // and should stay runnable in a build step, while asking a paid endpoint to say
    // one word is a thing someone should choose to do.
    let live = args.iter().any(|arg| arg == "--live");
    let paths: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .cloned()
        .collect();
    let config_path = arg_or(&paths, 0, "config.yaml");
    let ext_dir = arg_or(&paths, 1, "ext");

    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    println!("{}", runtime.report());

    let missing: Vec<&str> = runtime
        .extensions()
        .iter()
        .filter(|ext| matches!(ext.state, jan_klod_core::LoadState::Missing(_)))
        .map(|ext| ext.instance.id.as_str())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "jan-klod: {} enabled extension(s) missing from {ext_dir}/: {}",
            missing.len(),
            missing.join(", ")
        );
        return ExitCode::FAILURE;
    }

    // Present is not the same as working: a component that cannot be instantiated
    // or refuses to start would fail at the first turn instead of here.
    //
    // `start_all_eager`, not `start_all`: since #59, `tool-*`/`registry-*` are
    // instantiated on first use rather than at boot, and `start_all` (the
    // boot-plan path) reflects that. `verify` must not — proving a lazy
    // category *would* instantiate is the entire reason this command exists.
    match runtime.start_all_eager() {
        Ok(started) => {
            println!("verified: {} extension(s) start cleanly", started.len());
            if live {
                return verify_live(&runtime);
            }
            println!("(add --live to also ask the model one question)");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("jan-klod: start failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Ask the configured model one question, and report what happened.
///
/// Everything `verify` checked before this was offline (components resolve,
/// instantiate, start), which says nothing about a wrong API key, a dead
/// endpoint, a bad model name, or egress refusing the `base-url` — the things
/// that actually go wrong on a first run.
fn verify_live(runtime: &Runtime) -> ExitCode {
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let started = std::time::Instant::now();
    // Headless: an unanswered confirmation takes its default, which is a refusal.
    // Nothing here needs a tool, and a check that could be talked into running one
    // would be a strange thing to put in a diagnostic.
    let outcome = agent.run("verify", "Reply with the single word: ok");
    let elapsed = started.elapsed();
    match outcome {
        jan_klod_core::conductor::RunResult::Answered { text, .. } => {
            let reply = text.trim();
            let shown: String = reply.chars().take(60).collect();
            println!("live: the model answered in {elapsed:?} — {shown:?}");
            ExitCode::SUCCESS
        }
        jan_klod_core::conductor::RunResult::Failed(message) => {
            eprintln!("live: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Boot the runtime and print the plan, running each present component's
/// lifecycle. This is the default (no subcommand) mode.
fn boot_plan(args: &[String]) -> ExitCode {
    let config_path = arg_or(args, 0, "config.yaml");
    let ext_dir = arg_or(args, 1, "ext");

    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    println!("{}", runtime.report());

    // `start_all`, not `start_all_eager`: this is the boot-plan path, and #59
    // made it lazy for `tool-*`/`registry-*` — the report above already marks
    // which loaded instances that applies to. `started` below is providers and
    // interceptors only; use `verify` to force everything and prove a lazy one
    // actually instantiates.
    match runtime.start_all() {
        Ok(started) if started.is_empty() => {
            println!("no components started (none present in {ext_dir}/)");
        }
        Ok(started) => println!("started: {}", started.join(", ")),
        Err(err) => {
            eprintln!("jan-klod: start failed: {err}");
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}

/// Serve the client protocol over stdin/stdout: newline-delimited JSON-RPC, one
/// frame per line, for a client that spawned this process. No port, no token,
/// nothing left running — the client owns the process.
///
/// **Stdout is the protocol.** Nothing else may be written there: not the boot
/// plan `serve` prints, not a warning, not a progress line, because a client
/// splitting the stream on newlines would read it as a frame. Everything
/// diagnostic goes to stderr, which is where the runtime already writes every
/// log line it and its guests make — and guests are given `inherit_stderr`
/// rather than `inherit_stdio`, so a component cannot reach this stream either.
fn rpc(args: &[String]) -> ExitCode {
    let config_path = arg_or(args, 0, "config.yaml");
    let ext_dir = arg_or(args, 1, "ext");

    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    // Same bounded egress as `serve`: guests reach public destinations and the
    // endpoints config names.
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    // `BufReader::new(stdin())`, not `stdin().lock()`: the transport hands the
    // reader to a thread so a mid-turn cancel can be read, and a `StdinLock`
    // holds a `MutexGuard`, which is not `Send`.
    let input = std::io::BufReader::new(std::io::stdin());
    match jan_klod_host::rpc::serve(input, std::io::stdout(), &mut agent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jan-klod: rpc loop failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Restrict this process with Landlock, then become the command.
///
/// The Linux half of the command sandbox. It exists as a subcommand because
/// Landlock restricts *the calling process*, and applying it between `fork` and
/// `exec` means `pre_exec`, which is `unsafe` — forbidden by this workspace's
/// lints. Restricting ourselves and then calling the **safe** `exec` reaches the
/// same place: the command replaces this process and inherits the restriction.
/// See `jan_klod_core::sandbox_landlock` for the whole argument.
///
/// Exits `126` when it cannot confine or cannot exec, following the shell's
/// "found but not executable" convention, so a wrapper failure is not mistaken
/// for the command's own exit code.
#[cfg(target_os = "linux")]
fn confine(args: &[String]) -> ExitCode {
    use jan_klod_core::sandbox_landlock;

    const CANNOT_EXECUTE: u8 = 126;

    let plan = match sandbox_landlock::parse_confine_args(args.to_vec()) {
        Ok(plan) => plan,
        Err(err) => {
            eprintln!("jan-klod: {err}");
            return ExitCode::from(CANNOT_EXECUTE);
        }
    };
    // Before the command exists, not after: a confinement that failed must not
    // be followed by running the command anyway.
    if let Err(reason) = sandbox_landlock::apply(&plan) {
        eprintln!("jan-klod: {reason}");
        return ExitCode::from(CANNOT_EXECUTE);
    }
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]);
    // Returns only on failure — on success this process *is* the command.
    let err = std::os::unix::process::CommandExt::exec(&mut command);
    eprintln!(
        "jan-klod: cannot execute {:?}: {err}",
        plan.command[0].to_string_lossy()
    );
    ExitCode::from(CANNOT_EXECUTE)
}

/// The same subcommand where there is no Landlock to apply.
///
/// Reachable only by running it by hand: `sandbox::host_backend` hands out the
/// Landlock backend on Linux alone, so nothing on another platform produces a
/// command that calls this.
#[cfg(not(target_os = "linux"))]
fn confine(_args: &[String]) -> ExitCode {
    eprintln!(
        "jan-klod: `confine` applies the Linux command sandbox, and this build is for {}",
        std::env::consts::OS
    );
    ExitCode::FAILURE
}

/// Boot the agent and serve turns over the host-side REST surface until killed.
fn serve(args: &[String]) -> ExitCode {
    let (positional, flagged_bind) = split_serve_args(args);
    if let Some(message) = misplaced_address(&positional) {
        eprintln!("{message}");
        return ExitCode::FAILURE;
    }
    let config_path = arg_or(&positional, 0, "config.yaml");
    let ext_dir = arg_or(&positional, 1, "ext");
    let bind = flagged_bind.unwrap_or_else(|| arg(&positional, 2, "127.0.0.1:8787"));

    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Bounded by the egress policy: guests reach public destinations plus the
    // endpoints config names, not this gateway's own port, the cloud metadata
    // service, or the LAN.
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    // One agent per session, built on first use (#229). Nothing is built
    // here: a gateway that boots with no clients has nothing to serve
    // yet, and the first request pays 3.5 ms for the session it names.
    let agents = std::sync::Arc::new(jan_klod_host::sessions::Agents::new(
        std::sync::Arc::new(runtime),
        std::sync::Arc::new(factory),
    ));

    let surface = match jan_klod_host::serve::Surface::bind(&bind) {
        Ok(surface) => surface,
        Err(err) => {
            eprintln!("jan-klod: cannot bind {bind}: {err}");
            return ExitCode::FAILURE;
        }
    };
    // A secret belongs in the environment, not in a file people paste into issues.
    let token = std::env::var("JAN_KLOD_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());
    if token.is_some() {
        println!("jan-klod: requiring a bearer token (JAN_KLOD_TOKEN); /health stays open");
    }
    if let Some(warning) = exposure_warning(&bind, token.is_some()) {
        eprintln!("{warning}");
    }
    println!("jan-klod: serving on http://{bind} — POST {{\"session\":\"…\",\"message\":\"…\"}}");

    if let Err(err) = surface.serve_forever(&agents, token.as_deref()) {
        eprintln!("jan-klod: serve loop failed: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Boot the agent and drive it from a Telegram bot (long-poll) until killed. The
/// bot token comes from the `TELEGRAM_BOT_TOKEN` environment variable.
fn telegram(args: &[String]) -> ExitCode {
    const MAX_CONSECUTIVE_ERRORS: u32 = 10;
    let config_path = arg_or(args, 0, "config.yaml");
    let ext_dir = arg_or(args, 1, "ext");

    let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") else {
        eprintln!("jan-klod: set TELEGRAM_BOT_TOKEN to run the telegram bot");
        return ExitCode::FAILURE;
    };

    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    // Same bounded egress as `serve`: a guest reaches public destinations and the
    // endpoints config names.
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    // The host's own call to api.telegram.org, not a guest's, so it uses the
    // plain client. The read timeout must exceed the server-side long-poll window.
    let fetch = |method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>| {
        let owned: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        match jan_klod_core::http::fetch(method, url, &owned, body, 40_000) {
            Ok(response) => Ok(response.body),
            Err(err) => Err(format!("{err:?}")),
        }
    };

    println!("jan-klod: telegram bot polling (Ctrl-C to stop)");
    let mut offset = 0;
    let mut consecutive_errors: u32 = 0;
    loop {
        match jan_klod_host::telegram::poll_once(&mut agent, &fetch, &token, offset) {
            Ok(next) => {
                consecutive_errors = 0;
                offset = next;
            }
            Err(err) => {
                consecutive_errors += 1;
                eprintln!("jan-klod: telegram poll error ({consecutive_errors}/{MAX_CONSECUTIVE_ERRORS}): {err}");
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    eprintln!(
                        "jan-klod: {MAX_CONSECUTIVE_ERRORS} consecutive poll failures — \
                         check TELEGRAM_BOT_TOKEN and network connectivity"
                    );
                    return ExitCode::FAILURE;
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
    }
}

/// Positional arg `index` (0-based within the subcommand's args), or `default`.
fn arg(args: &[String], index: usize, default: &str) -> String {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

/// A warning when `bind` exposes the surface with nothing guarding it.
///
/// Silent once a token is set — a warning that persists after the reader has
/// done what it asked is one they learn to ignore.
///
/// The surface has no authentication: anyone who can reach it can start a turn,
/// and the agent behind it reads/writes a workspace and, where allowed, runs
/// commands. Fine on `127.0.0.1` (the default); an open door on a routable
/// address. A warning rather than a refusal, since binding elsewhere behind an
/// authenticating reverse proxy is legitimate.
fn exposure_warning(bind: &str, has_token: bool) -> Option<String> {
    if has_token {
        return None;
    }
    let host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost"
        || host == "::1"
        || host
            .strip_prefix("127.")
            .is_some_and(|rest| rest.contains('.'));
    if loopback {
        return None;
    }
    Some(
        [
            format!("jan-klod: WARNING — binding to {bind}, which is not loopback."),
            "jan-klod: nothing guards this surface. Anyone who can reach it can drive".to_string(),
            "jan-klod: the agent, which reads and writes your workspace.".to_string(),
            "jan-klod: Set JAN_KLOD_TOKEN to require a bearer token, or bind 127.0.0.1."
                .to_string(),
        ]
        .join("\n"),
    )
}

/// Split `serve`'s arguments into positionals and an optional `--bind <addr>`.
///
/// The flag lets a caller name the address without claiming the config/ext
/// slots, which [`arg_or`] otherwise resolves against the installed data
/// directory. The positional form still works for anyone naming all three.
fn split_serve_args(args: &[String]) -> (Vec<String>, Option<String>) {
    let mut positional = Vec::new();
    let mut bind = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--bind" {
            bind = rest.next().cloned();
        } else {
            positional.push(arg.clone());
        }
    }
    (positional, bind)
}

/// Refuse a positional that is plainly an address in a *path* slot.
///
/// The positional form is `serve [config] [ext] [addr]`, so only the first two
/// slots are paths; slot 2 is legitimately an address. Returns the message to
/// print, or `None` when the arguments are fine.
fn misplaced_address(positional: &[String]) -> Option<String> {
    let offender = positional
        .iter()
        .take(2)
        .find(|arg| looks_like_an_address(arg))?;
    Some(format!(
        "jan-klod: `{offender}` looks like an address, not a path. Use \
         `--bind {offender}` — a positional argument names a config file, and the \
         positional form only works inside a checkout anyway."
    ))
}

/// Whether a positional argument looks like a bind address rather than a path.
///
/// Deliberately narrow: `host:port` with a numeric port, or a bare `:port`. A
/// real path can contain a colon, so this must not claim one is an address
/// unless the tail is digits.
fn looks_like_an_address(arg: &str) -> bool {
    let Some((host, port)) = arg.rsplit_once(':') else {
        return false;
    };
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // `[::1]:8787`, `localhost:8787`, `:8787` — but not `notes:2024` in a path,
    // which has no dot and is not a known host spelling.
    host.is_empty()
        || host == "localhost"
        || host.ends_with(']')
        || host.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Like [`arg`], but a missing config/ext argument falls back to the installed
/// copy next to the binary rather than to a bare relative name.
fn arg_or(args: &[String], index: usize, default: &str) -> String {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| resolve_default(default))
}

#[cfg(test)]
mod tests {
    use super::split_serve_args;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn loopback_binds_are_not_warned_about() {
        for quiet in [
            "127.0.0.1:8787",
            "localhost:8787",
            "[::1]:8787",
            "127.1.2.3:9",
        ] {
            assert!(
                super::exposure_warning(quiet, false).is_none(),
                "{quiet} is loopback and needs no warning"
            );
        }
    }

    #[test]
    fn a_reachable_bind_says_the_surface_is_unauthenticated() {
        // The one that matters: 0.0.0.0 is what someone types when they want to
        // reach it from another machine, which is exactly when they need telling.
        for loud in [
            "0.0.0.0:8787",
            "192.168.1.10:8787",
            "[::]:8787",
            "10.0.0.5:80",
        ] {
            let warning = super::exposure_warning(loud, false)
                .unwrap_or_else(|| panic!("{loud} is reachable and must warn"));
            assert!(warning.contains("nothing guards this surface"), "{warning}");
        }
    }

    #[test]
    fn setting_a_token_silences_the_exposure_warning() {
        // A warning that survives doing what it asked is one people learn to skip.
        assert!(
            super::exposure_warning("0.0.0.0:8787", true).is_none(),
            "a guarded surface is not an unguarded one"
        );
    }

    #[test]
    fn the_bind_flag_does_not_consume_a_positional_slot() {
        // The whole point: config and ext stay unset, so they resolve against the
        // installed data directory rather than the user's working directory.
        let (positional, bind) = split_serve_args(&args(["--bind", "127.0.0.1:9000"].as_ref()));
        assert!(
            positional.is_empty(),
            "no positionals claimed: {positional:?}"
        );
        assert_eq!(bind.as_deref(), Some("127.0.0.1:9000"));
    }

    #[test]
    fn an_address_in_a_path_slot_names_the_flag() {
        use super::misplaced_address;
        // The obvious thing to type, previously reported as a missing config file.
        for typo in ["127.0.0.1:8787", "localhost:8787", "[::1]:8787", ":8787"] {
            let message = misplaced_address(&args([typo].as_ref()))
                .unwrap_or_else(|| panic!("{typo} should be recognised as an address"));
            assert!(
                message.contains("--bind"),
                "the message names the flag: {message}"
            );
        }
        // Slot 2 *is* the address in the positional form, so it must not trip.
        assert!(misplaced_address(&args(["c.yaml", "ext", "1.2.3.4:1"].as_ref())).is_none());
        // And a real path is left alone, colon or not.
        for path in [
            "config.yaml",
            "ext",
            "/srv/jan-klod/config.yaml",
            "notes:2024/config.yaml",
        ] {
            assert!(
                misplaced_address(&args([path].as_ref())).is_none(),
                "{path} is a path"
            );
        }
    }

    #[test]
    fn the_positional_form_still_names_all_three() {
        let (positional, bind) = split_serve_args(&args(["c.yaml", "e", "1.2.3.4:1"].as_ref()));
        assert_eq!(positional, args(["c.yaml", "e", "1.2.3.4:1"].as_ref()));
        assert_eq!(
            bind, None,
            "no flag, so the third positional is the address"
        );
    }

    #[test]
    fn a_bind_address_is_never_mistaken_for_a_path() {
        // Filtering by *value* would drop a positional that happened to equal the
        // address; consuming the token after the flag cannot.
        let (positional, bind) = split_serve_args(&args(["c.yaml", "--bind", "c.yaml"].as_ref()));
        assert_eq!(
            positional,
            args(["c.yaml"].as_ref()),
            "the config path survives"
        );
        assert_eq!(bind.as_deref(), Some("c.yaml"));
    }

    #[test]
    fn a_dangling_flag_falls_back_to_the_default_address() {
        let (positional, bind) = split_serve_args(&args(["--bind"].as_ref()));
        assert!(positional.is_empty());
        assert_eq!(
            bind, None,
            "nothing followed it, so the caller gets the default"
        );
    }
}
