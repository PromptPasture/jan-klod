//! `jan-klod` core entrypoint; `--help` lists subcommands and defaults.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use jk_agent::Runtime;
use jk_config::Config;
use jk_session::event_log::{verify_chain, ChainStatus};
use jk_session::store::Store;
use jk_wasm::ext_index;
use jk_wasm::route::HttpFn;
use tracing::Level;
use tracing_subscriber::fmt::time::SystemTime;

/// Installed data dir relative to the gateway binary: `<prefix>/bin/jan-klod-gateway` to `<prefix>/share/jan-klod/`.
const INSTALLED_DATA: &str = "../share/jan-klod";

const USAGE: &str = "\
Usage: jan-klod-gateway [command] [args]

Commands:
  (none) [config] [ext]         boot and print the plan
  serve [config] [ext] [bind]   serve REST turns (also: serve --bind <addr>)
  rpc [config] [ext]            serve JSON-RPC on stdio
  telegram                      run the Telegram channel
  setup [distribution]          audit the install for a distribution
  verify [config] [ext]         check the install (also: verify --chain [session])
  ask <prompt>                  answer one prompt on stdout
  ext list|search|install|remove  manage extensions
  mcp                           serve MCP on stdio
  acp                           serve ACP on stdio

Options:
  -V, --version                 print the version
  -h, --help                    print this help
";

/// The working directory wins (a coding agent works in *your* repository), else the installed copy.
fn resolve_default(name: &str) -> String {
    if Path::new(name).exists() {
        return name.to_string();
    }
    installed_candidates(name)
        .into_iter()
        .find(|path| path.exists())
        .map_or_else(
            || name.to_string(),
            |path| path.to_string_lossy().into_owned(),
        )
}

/// `config.yaml` in the XDG config dir, anything else in the XDG data dir, then beside the binary.
fn installed_candidates(name: &str) -> Vec<PathBuf> {
    let xdg = if name == "config.yaml" {
        xdg_home("XDG_CONFIG_HOME", ".config")
    } else {
        xdg_home("XDG_DATA_HOME", ".local/share")
    };
    [xdg, beside_exe()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(name))
        .collect()
}

fn beside_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(INSTALLED_DATA)))
}

/// `$<var>/jan-klod`, else `$HOME/<fallback>/jan-klod`; a relative `$<var>` is ignored, as the spec requires.
fn xdg_home(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))?;
    Some(base.join("jan-klod"))
}

/// A relative `storage.path` resolves against an installed config's XDG state dir, else the config's own dir; a store earlier installs left in the data dir moves there.
fn store_dir(config_path: &str) -> io::Result<PathBuf> {
    let config_dir = Path::new(config_path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let installed = installed_candidates("config.yaml")
        .iter()
        .any(|candidate| candidate == Path::new(config_path));
    let state = match xdg_home("XDG_STATE_HOME", ".local/state") {
        Some(state) if installed => state,
        _ => return Ok(config_dir),
    };
    std::fs::create_dir_all(&state)?;

    let relative = Config::from_path(config_path)
        .ok()
        .and_then(|config| {
            config
                .agent
                .get("storage")?
                .get("path")?
                .as_str()
                .map(PathBuf::from)
        })
        .filter(|path| path.is_relative());
    let Some(relative) = relative else {
        return Ok(state);
    };
    let current = state.join(&relative);
    let legacy_dir = [xdg_home("XDG_DATA_HOME", ".local/share"), beside_exe()]
        .into_iter()
        .flatten()
        .find(|dir| dir.join(&relative).is_file());
    if let (Some(legacy_dir), false) = (legacy_dir, current.exists()) {
        let legacy = legacy_dir.join(&relative);
        if let Some(parent) = current.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) = std::fs::rename(&legacy, &current) {
            eprintln!(
                "jan-klod: keeping the session store at {}: cannot move it to {}: {err}",
                legacy.display(),
                current.display()
            );
            return Ok(legacy_dir);
        }
    }
    Ok(state)
}

fn boot(config_path: &str, ext_dir: &str) -> Result<Runtime, jk_wasm::CoreError> {
    let store_dir = store_dir(config_path).map_err(|err| jk_wasm::CoreError::Store {
        message: format!("cannot prepare the session store directory: {err}"),
    })?;
    Runtime::boot_with_store_dir(config_path, ext_dir, &store_dir)
}

fn init_tracing(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_path(config_path)?;

    match config.observability.output_format {
        jk_config::OutputFormat::Json => {
            tracing_subscriber::fmt()
                .json()
                .with_writer(io::stderr)
                .with_timer(SystemTime)
                .with_max_level(Level::DEBUG)
                .with_target(true)
                .init();
        }
        jk_config::OutputFormat::Human => {
            tracing_subscriber::fmt()
                .pretty()
                .with_writer(io::stderr)
                .with_timer(SystemTime)
                .with_max_level(Level::DEBUG)
                .with_target(true)
                .init();
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("rpc") => rpc(&args[1..]),
        Some(jk_wasm::sandbox_landlock::CONFINE_SUBCOMMAND) => confine(&args[1..]),
        Some("telegram") => telegram(&args[1..]),
        Some("setup") => setup(&args[1..]),
        Some("verify") => verify(&args[1..]),
        Some("ask") => ask(&args[1..]),
        Some("ext") => ext(&args[1..]),
        Some("mcp") => mcp(),
        Some("acp") => acp(),
        Some("--version" | "-V") => {
            println!("jan-klod-gateway {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => boot_plan(&args),
    }
}

fn acp() -> ExitCode {
    let config_path = resolve_default("config.yaml");
    let ext_dir = resolve_default("ext");

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let runtime = match boot(&config_path, &ext_dir) {
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
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
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
    match jk_host::acp::serve(input, output, &mut agent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jan-klod: acp: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Stdout is frames only (logs go to stderr); the driver is headless because stdin carries frames.
fn mcp() -> ExitCode {
    let config_path = resolve_default("config.yaml");
    let ext_dir = resolve_default("ext");

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let runtime = match boot(&config_path, &ext_dir) {
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
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
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
    match jk_host::mcp::serve(input, &mut output, &mut agent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jan-klod: mcp: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `install` verifies before it copies: the build pipeline is outside the runtime sandbox, so whatever lands in `ext/` runs with host privileges.
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

fn configured_index() -> Result<(String, Vec<ext_index::Entry>), ext_index::IndexError> {
    let config_path = resolve_default("config.yaml");
    let source = ext_index::url_from_config_path(std::path::Path::new(&config_path))?;
    let http = jk_wasm::ext::policy_bound_http();
    let entries = ext_index::load(&source, &http)?;
    Ok((source, entries))
}

/// The capability line gets its own line: a table-truncated list gets skimmed.
fn show_entry(entry: &ext_index::Entry) {
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
        println!("  needs nothing");
    } else {
        println!("  may use {}", entry.capabilities.join(", "));
    }
    if !entry.description.is_empty() {
        println!("  {}", entry.description);
    }
}

fn ext_search(args: &[String]) -> ExitCode {
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

/// Fetches use the policy-bound client (redirects re-checked per hop); `.wasm` detection matches [`jk_wasm::ext::install`].
fn install_from_source(
    dir: &str,
    source: &str,
    checks: &jk_wasm::ext::Checks,
) -> Result<jk_wasm::ext::Installed, ext_index::IndexError> {
    let dir = std::path::Path::new(dir);
    if jk_wasm::ext::looks_remote(source) {
        let http = jk_wasm::ext::policy_bound_http();
        return Ok(jk_wasm::ext::install_from_url(dir, source, checks, &http)?);
    }
    let path = std::path::Path::new(source);
    if path.extension().and_then(|ext| ext.to_str()) == Some("wasm") {
        return Ok(jk_wasm::ext::install(dir, path, checks)?);
    }
    let (index_source, entries) = configured_index()?;
    let http = jk_wasm::ext::policy_bound_http();
    ext_index::install(dir, &entries, source, &index_source, checks, &http)
}

fn ext_install(args: &[String]) -> ExitCode {
    let mut sha256: Option<String> = None;
    let mut allow_unsigned = false;
    let mut positional: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
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

    let config_path = resolve_default("config.yaml");
    let mut checks =
        match jk_wasm::ext::Checks::from_config_path(std::path::Path::new(&config_path)) {
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
            match &installed.declaration {
                jk_wasm::ext::Declaration::Present(manifest)
                    if manifest.capabilities.is_empty() =>
                {
                    println!("installed {} into {dir}/ — needs nothing", installed.name);
                }
                jk_wasm::ext::Declaration::Present(manifest) => {
                    println!(
                        "installed {} into {dir}/ — may use {}",
                        installed.name,
                        manifest.capabilities.join(", ")
                    );
                }
                jk_wasm::ext::Declaration::Absent | jk_wasm::ext::Declaration::Broken(_) => {
                    println!(
                        "installed {} into {dir}/, but its manifest no longer reads",
                        installed.name
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
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

fn ext_list(args: &[String]) -> ExitCode {
    let remote = args.iter().any(|arg| arg == "--remote");
    let positional: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .cloned()
        .collect();
    if remote {
        return ext_search(&[]);
    }

    let dir = resolve_default(&arg_or(&positional, 0, "ext"));
    let staged = match jk_wasm::ext::list(std::path::Path::new(&dir)) {
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
            jk_wasm::ext::Declaration::Present(manifest) => {
                let capabilities = if manifest.capabilities.is_empty() {
                    "needs nothing".to_owned()
                } else {
                    manifest.capabilities.join(", ")
                };
                println!(
                    "{:<width$}  {}  api {}  {}",
                    component.name, manifest.version, manifest.api_version, capabilities
                );
            }
            jk_wasm::ext::Declaration::Absent => {
                println!(
                    "{:<width$}  no manifest — the next boot will refuse it",
                    component.name
                );
            }
            jk_wasm::ext::Declaration::Broken(reason) => {
                println!("{:<width$}  unusable manifest: {reason}", component.name);
            }
        }
    }
    ExitCode::SUCCESS
}

fn ext_remove(args: &[String]) -> ExitCode {
    let Some(name) = args.first() else {
        eprintln!("usage: jan-klod-gateway ext remove <name> [ext-dir]");
        return ExitCode::FAILURE;
    };
    let dir = resolve_default(&arg_or(args, 1, "ext"));
    match jk_wasm::ext::remove(std::path::Path::new(&dir), name) {
        Ok(removed) => {
            match removed {
                jk_wasm::ext::Removed::Both => {
                    println!("removed {name} and its manifest from {dir}/");
                }
                jk_wasm::ext::Removed::ComponentOnly => {
                    println!("removed {name} from {dir}/ — it had no manifest");
                }
                jk_wasm::ext::Removed::ManifestOnly => {
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

/// Confirmations are asked on the terminal; headless defaults would deny every write and command.
fn ask(args: &[String]) -> ExitCode {
    let question = args.join(" ");
    if question.trim().is_empty() {
        eprintln!("usage: jan-klod-gateway ask <question>");
        return ExitCode::FAILURE;
    }
    let config_path = resolve_default("config.yaml");
    let ext_dir = resolve_default("ext");

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let runtime = match boot(&config_path, &ext_dir) {
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
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
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
        jk_agent::conductor::RunResult::Answered { text, .. } => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        jk_agent::conductor::RunResult::Failed(message) => {
            eprintln!("jan-klod: {message}");
            ExitCode::FAILURE
        }
    }
}

struct TerminalDriver;

impl jk_protocol::turn::Driver for TerminalDriver {
    fn ask(&mut self, prompt: &jk_protocol::turn::UserPrompt) -> String {
        eprintln!("\n  ? {}", prompt.question);
        eprint!(
            "  [{}] (Enter = {}): ",
            prompt.options.join("/"),
            prompt.default_answer
        );
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut typed = String::new();
        match std::io::stdin().read_line(&mut typed) {
            Ok(0) | Err(_) => prompt.default_answer.clone(),
            Ok(_) if typed.trim().is_empty() => prompt.default_answer.clone(),
            Ok(_) => typed.trim().to_string(),
        }
    }
}

/// Exits non-zero when a component is missing; otherwise the runtime skips it and starts less capable than it claims.
fn verify(args: &[String]) -> ExitCode {
    let live = args.iter().any(|arg| arg == "--live");
    let has_chain = args.iter().any(|arg| arg == "--chain");
    let chain_session = if has_chain {
        args.iter()
            .position(|arg| arg == "--chain")
            .and_then(|pos| args.get(pos + 1))
            .filter(|arg| !arg.starts_with("--"))
            .map(std::string::ToString::to_string)
    } else {
        None
    };

    let paths: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .cloned()
        .collect();
    let config_path = arg_or(&paths, 0, "config.yaml");
    let ext_dir = arg_or(&paths, 1, "ext");

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let runtime = match boot(&config_path, &ext_dir) {
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
        .filter(|ext| matches!(ext.state, jk_wasm::LoadState::Missing(_)))
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

    match runtime.start_all_eager() {
        Ok(started) => {
            println!("verified: {} extension(s) start cleanly", started.len());

            if has_chain {
                let chain_result = verify_event_chain(&config_path, chain_session);
                return chain_result;
            }

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

#[allow(clippy::too_many_lines)]
fn verify_event_chain(config_path: &str, session_id: Option<String>) -> ExitCode {
    let config = match Config::from_path(config_path) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("jan-klod: config error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let store_dir = match store_dir(config_path) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("jan-klod: cannot prepare the session store directory: {err}");
            return ExitCode::FAILURE;
        }
    };
    let store_path = config
        .agent
        .get("storage")
        .and_then(|storage| storage.get("path"))
        .and_then(serde_json::Value::as_str)
        .map(|path| store_dir.join(path));

    let store = match store_path {
        Some(path) => match Store::open(&path) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("jan-klod: failed to open store: {err}");
                return ExitCode::FAILURE;
            }
        },
        None => match Store::open_in_memory() {
            Ok(s) => s,
            Err(err) => {
                eprintln!("jan-klod: failed to open store: {err}");
                return ExitCode::FAILURE;
            }
        },
    };

    if let Some(session) = session_id {
        match verify_chain(&store, &session) {
            Ok(verification) => match &verification.status {
                ChainStatus::Intact => {
                    println!("session {session}: chain intact");
                    ExitCode::SUCCESS
                }
                ChainStatus::Broken { seq, kind, reason } => {
                    println!("session {session}: chain broken at seq={seq}, kind={kind}: {reason}");
                    ExitCode::FAILURE
                }
            },
            Err(err) => {
                eprintln!("jan-klod: {err}");
                ExitCode::FAILURE
            }
        }
    } else {
        match get_recent_sessions(&store, 10) {
            Ok(sessions) => {
                if sessions.is_empty() {
                    println!("no sessions found");
                    return ExitCode::SUCCESS;
                }

                let mut broken = Vec::new();
                for session in &sessions {
                    match verify_chain(&store, session) {
                        Ok(verification) => {
                            if let ChainStatus::Broken { seq, kind, reason } = verification.status {
                                broken.push((session.clone(), seq, kind, reason));
                            }
                        }
                        Err(err) => {
                            eprintln!("jan-klod: error verifying session {session}: {err}");
                            return ExitCode::FAILURE;
                        }
                    }
                }

                if broken.is_empty() {
                    println!("{} sessions checked, all intact", sessions.len());
                    ExitCode::SUCCESS
                } else {
                    println!(
                        "{} of {} sessions broken here:",
                        broken.len(),
                        sessions.len()
                    );
                    for (session, seq, kind, reason) in broken {
                        println!("  {session}: chain broken at seq={seq}, kind={kind}: {reason}");
                    }
                    ExitCode::FAILURE
                }
            }
            Err(err) => {
                eprintln!("jan-klod: {err}");
                ExitCode::FAILURE
            }
        }
    }
}

fn get_recent_sessions(store: &Store, limit: usize) -> Result<Vec<String>, String> {
    let all_sessions = store
        .event_sessions()
        .map_err(|e| format!("failed to list sessions: {e}"))?;

    if all_sessions.is_empty() {
        return Ok(Vec::new());
    }

    let mut sessions_with_max_ts: Vec<(String, u64)> = all_sessions
        .into_iter()
        .filter_map(|session| {
            store.session_events(&session).ok().and_then(|events| {
                events
                    .iter()
                    .map(|e| e.ts)
                    .max()
                    .map(|max_ts| (session, max_ts))
            })
        })
        .collect();

    sessions_with_max_ts.sort_by_key(|a| std::cmp::Reverse(a.1));

    Ok(sessions_with_max_ts
        .into_iter()
        .take(limit)
        .map(|(session, _)| session)
        .collect())
}

/// `verify`'s checks are offline and miss a wrong API key, dead endpoint, bad model name, or egress refusing `base-url`.
fn verify_live(runtime: &Runtime) -> ExitCode {
    let policy = runtime.egress_policy();
    let factory = move || -> HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
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
    let outcome = agent.run("verify", "Reply with the single word: ok");
    let elapsed = started.elapsed();
    match outcome {
        jk_agent::conductor::RunResult::Answered { text, .. } => {
            let reply = text.trim();
            let shown: String = reply.chars().take(60).collect();
            println!("live: the model answered in {elapsed:?} — {shown:?}");
            ExitCode::SUCCESS
        }
        jk_agent::conductor::RunResult::Failed(message) => {
            eprintln!("live: {message}");
            ExitCode::FAILURE
        }
    }
}

fn boot_plan(args: &[String]) -> ExitCode {
    let config_path = arg_or(args, 0, "config.yaml");
    let ext_dir = arg_or(args, 1, "ext");

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let runtime = match boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    println!("{}", runtime.report());

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

/// Stdout is the protocol: nothing else may be written there; diagnostics go to stderr and guests get `inherit_stderr`, not `inherit_stdio`.
fn rpc(args: &[String]) -> ExitCode {
    let config_path = arg_or(args, 0, "config.yaml");
    let ext_dir = arg_or(args, 1, "ext");

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let runtime = match boot(&config_path, &ext_dir) {
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
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
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
    match jk_host::rpc::serve(input, std::io::stdout(), &mut agent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jan-klod: rpc loop failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// A subcommand because `pre_exec` is `unsafe`; exits `126` if it cannot confine or exec, distinguishable from the command's own code.
#[cfg(target_os = "linux")]
fn confine(args: &[String]) -> ExitCode {
    use jk_wasm::sandbox_landlock;

    const CANNOT_EXECUTE: u8 = 126;

    let plan = match sandbox_landlock::parse_confine_args(args.to_vec()) {
        Ok(plan) => plan,
        Err(err) => {
            eprintln!("jan-klod: {err}");
            return ExitCode::from(CANNOT_EXECUTE);
        }
    };
    if let Err(reason) = sandbox_landlock::apply(&plan) {
        eprintln!("jan-klod: {reason}");
        return ExitCode::from(CANNOT_EXECUTE);
    }
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]);
    let err = std::os::unix::process::CommandExt::exec(&mut command);
    eprintln!(
        "jan-klod: cannot execute {:?}: {err}",
        plan.command[0].to_string_lossy()
    );
    ExitCode::from(CANNOT_EXECUTE)
}

/// Reachable only by hand, since `sandbox::host_backend` hands out Landlock on Linux alone.
#[cfg(not(target_os = "linux"))]
fn confine(_args: &[String]) -> ExitCode {
    eprintln!(
        "jan-klod: `confine` applies the Linux command sandbox, and this build is for {}",
        std::env::consts::OS
    );
    ExitCode::FAILURE
}

/// Reads `serve.principals`, else `operator: ${JAN_KLOD_TOKEN}` if set, else an empty map (unauthenticated surface).
fn load_principals(config_path: &str) -> Result<std::collections::HashMap<String, String>, String> {
    use jk_config::Config;

    let config =
        Config::from_path(config_path).map_err(|e| format!("failed to load config: {e}"))?;

    if let Some(serve) = config.agent.get("serve") {
        if let Some(principals_obj) = serve.get("principals") {
            if let Some(map) = principals_obj.as_object() {
                let mut principals = std::collections::HashMap::new();
                for (name, value) in map {
                    let token = match value {
                        serde_json::Value::String(s) => {
                            if s.starts_with("${") && s.ends_with('}') {
                                let var_name = &s[2..s.len() - 1];
                                std::env::var(var_name).map_err(|_| {
                                    format!(
                                        "principal {name}: unset environment variable {var_name}"
                                    )
                                })?
                            } else {
                                s.clone()
                            }
                        }
                        _ => return Err(format!("principal {name}: value must be a string")),
                    };
                    principals.insert(name.clone(), token);
                }
                return Ok(principals);
            }
        }
    }

    if let Ok(token) = std::env::var("JAN_KLOD_TOKEN") {
        if !token.trim().is_empty() {
            let mut principals = std::collections::HashMap::new();
            principals.insert("operator".to_string(), token);
            return Ok(principals);
        }
    }

    Ok(std::collections::HashMap::new())
}

fn serve(args: &[String]) -> ExitCode {
    let (positional, flagged_bind) = split_serve_args(args);
    if let Some(message) = misplaced_address(&positional) {
        eprintln!("{message}");
        return ExitCode::FAILURE;
    }
    let config_path = arg_or(&positional, 0, "config.yaml");
    let ext_dir = arg_or(&positional, 1, "ext");
    let bind = flagged_bind.unwrap_or_else(|| arg(&positional, 2, "127.0.0.1:8787"));

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let runtime = match boot(&config_path, &ext_dir) {
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
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let agents = jk_host::sessions::Agents::new(runtime, std::sync::Arc::new(factory));

    let surface = match jk_host::serve::Surface::bind(&bind) {
        Ok(surface) => surface,
        Err(err) => {
            eprintln!("jan-klod: cannot bind {bind}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let principals = match load_principals(&config_path) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("jan-klod: {err}");
            return ExitCode::FAILURE;
        }
    };
    if !principals.is_empty() {
        println!(
            "jan-klod: requiring authentication ({} principal(s)); /health stays open",
            principals.len()
        );
    }
    if let Some(warning) = exposure_warning(&bind, !principals.is_empty()) {
        eprintln!("{warning}");
    }
    println!("jan-klod: serving on http://{bind} — POST {{\"session\":\"…\",\"message\":\"…\"}}");

    if let Err(err) = surface.serve_forever(&agents, principals) {
        eprintln!("jan-klod: serve loop failed: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn telegram(args: &[String]) -> ExitCode {
    const MAX_CONSECUTIVE_ERRORS: u32 = 10;
    let config_path = arg_or(args, 0, "config.yaml");
    let ext_dir = arg_or(args, 1, "ext");

    let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") else {
        eprintln!("jan-klod: set TELEGRAM_BOT_TOKEN to run the telegram bot");
        return ExitCode::FAILURE;
    };

    if let Err(err) = init_tracing(&config_path) {
        eprintln!("jan-klod: failed to initialize observability: {err}");
        return ExitCode::FAILURE;
    }

    let allowed_senders_opt = match Config::from_path(&config_path) {
        Ok(config) => Some(
            config
                .agent
                .get("telegram")
                .and_then(|telegram_value| telegram_value.get("allowed-senders"))
                .and_then(|senders_value| senders_value.as_array())
                .map(|senders_array| {
                    senders_array
                        .iter()
                        .filter_map(serde_json::Value::as_i64)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        ),
        Err(err) => {
            eprintln!("jan-klod: config load failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match boot(&config_path, &ext_dir) {
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
            jk_wasm::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let fetch = |method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>| {
        let owned: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        match jk_wasm::http::fetch(method, url, &owned, body, 40_000) {
            Ok(response) => Ok(response.body),
            Err(err) => Err(format!("{err:?}")),
        }
    };

    println!("jan-klod: telegram bot polling (Ctrl-C to stop)");
    let mut offset = 0;
    let mut consecutive_errors: u32 = 0;
    loop {
        match jk_host::telegram::poll_once(
            &mut agent,
            &fetch,
            &token,
            offset,
            allowed_senders_opt.as_deref(),
        ) {
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

fn setup(args: &[String]) -> ExitCode {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("Usage: jan-klod-gateway setup [distribution] [config-path] [ext-dir]");
        println!();
        println!("Audit the environment and distribution state without writing files.");
        println!();
        println!("Arguments:");
        println!("  distribution   Name of the distribution (e.g., coding, minimal) [optional]");
        println!("  config-path    Path to the configuration file (default: config.yaml)");
        println!("  ext-dir        Path to the extensions directory (default: ext)");
        println!();
        println!("Options:");
        println!("  -h, --help     Show this help message");
        return ExitCode::SUCCESS;
    }

    let non_flag_args: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();

    let distribution = non_flag_args.first().map(|s| s.as_str());
    let config_path = if distribution.is_some() {
        non_flag_args.get(1).map_or("config.yaml", |s| s.as_str())
    } else {
        non_flag_args.first().map_or("config.yaml", |s| s.as_str())
    };
    let ext_dir = if distribution.is_some() {
        non_flag_args.get(2).map_or("ext", |s| s.as_str())
    } else {
        non_flag_args.get(1).map_or("ext", |s| s.as_str())
    };

    match crate::setup::audit(distribution, config_path, ext_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jan-klod: setup audit failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn arg(args: &[String], index: usize, default: &str) -> String {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

/// Silent once a token is set; a warning rather than a refusal because an authenticating reverse proxy is legitimate.
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

/// In `serve [config] [ext] [addr]` only the first two are paths.
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

/// Deliberately narrow, since a real path can contain a colon.
fn looks_like_an_address(arg: &str) -> bool {
    let Some((host, port)) = arg.rsplit_once(':') else {
        return false;
    };
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    host.is_empty()
        || host == "localhost"
        || host.ends_with(']')
        || host.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Falls back to the installed copy next to the binary rather than a bare relative name.
fn arg_or(args: &[String], index: usize, default: &str) -> String {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| resolve_default(default))
}

mod setup;

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
        assert!(
            super::exposure_warning("0.0.0.0:8787", true).is_none(),
            "a guarded surface is not an unguarded one"
        );
    }

    #[test]
    fn the_bind_flag_does_not_consume_a_positional_slot() {
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
        for typo in ["127.0.0.1:8787", "localhost:8787", "[::1]:8787", ":8787"] {
            let message = misplaced_address(&args([typo].as_ref()))
                .unwrap_or_else(|| panic!("{typo} should be recognised as an address"));
            assert!(
                message.contains("--bind"),
                "the message names the flag: {message}"
            );
        }
        assert!(misplaced_address(&args(["c.yaml", "ext", "1.2.3.4:1"].as_ref())).is_none());
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
        assert_eq!(positional, [] as [std::string::String; 0]);
        assert_eq!(
            bind, None,
            "nothing followed it, so the caller gets the default"
        );
    }
}
