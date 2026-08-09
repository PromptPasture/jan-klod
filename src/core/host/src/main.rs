//! `jan-klod` core entrypoint: boot the runtime from `config.yaml`, resolve the
//! enabled extensions against the `ext/` directory, and either print the boot plan
//! or serve the loop over the host-side REST surface. All behaviour lives in the
//! extensions it loads — this binary is just the container.
//!
//! Usage:
//!   `jan-klod [config-path] [ext-dir]`            — boot + print the plan
//!   `jan-klod serve [config-path] [ext-dir] [bind]` — boot + serve REST turns
//!   `jan-klod verify [config-path] [ext-dir]`     — check the install, then exit
//!
//!   config-path  path to config.yaml   (default: config.yaml)
//!   ext-dir      directory of *.wasm    (default: ext)
//!   bind         host:port to listen on (default: 127.0.0.1:8787)

use std::path::PathBuf;
use std::process::ExitCode;

use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

/// Where the installed copies of `config.yaml` and `ext/` live, relative to the
/// gateway binary: `<prefix>/bin/jan-klod-gateway` → `<prefix>/share/jan-klod/`.
const INSTALLED_DATA: &str = "../share/jan-klod";

/// Resolve a default config/ext path.
///
/// The working directory wins, so running inside a checkout uses that checkout.
/// Otherwise the copy installed alongside the binary is used — without this, an
/// installed jan-klod only worked when launched from a directory that happened to
/// contain a `config.yaml` and an `ext/`, which for a coding agent is never: the
/// whole point is to `cd` into *your* repository and run it there.
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
        Some("telegram") => telegram(&args[1..]),
        Some("verify") => verify(&args[1..]),
        _ => boot_plan(&args),
    }
}

/// Check that an install is complete: every extension the config enables resolves
/// to a component that is present and instantiates. Prints the plan and exits
/// non-zero if anything is missing or fails to start.
///
/// This exists because "the component is not there" is otherwise a *silent*
/// degradation — the runtime skips what it cannot find, so a bundle assembled
/// without a guest, or a config naming one nobody built, starts happily and is
/// merely less capable than it claims. `bundle.sh` runs this against the assembled
/// bundle so a broken one cannot be released, and a user can run it to answer
/// "why is that tool not working?".
fn verify(args: &[String]) -> ExitCode {
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
    match runtime.start_all() {
        Ok(started) => {
            println!("verified: {} extension(s) start cleanly", started.len());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("jan-klod: start failed: {err}");
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

/// Boot the agent and serve turns over the host-side REST surface until killed.
fn serve(args: &[String]) -> ExitCode {
    let config_path = arg_or(args, 0, "config.yaml");
    let ext_dir = arg_or(args, 1, "ext");
    let bind = arg(args, 2, "127.0.0.1:8787");

    let runtime = match Runtime::boot(&config_path, &ext_dir) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("jan-klod: boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Live host-http: the provider's outbound calls hit the real network.
    let factory = || -> HttpFn { Box::new(jan_klod_core::http::fetch) };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let server = match tiny_http::Server::http(&bind) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("jan-klod: cannot bind {bind}: {err}");
            return ExitCode::FAILURE;
        }
    };
    println!("jan-klod: serving on http://{bind} — POST {{\"session\":\"…\",\"message\":\"…\"}}");

    if let Err(err) = jan_klod_core::serve::serve(&server, &mut agent) {
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
    let factory = || -> HttpFn { Box::new(jan_klod_core::http::fetch) };
    let mut agent = match runtime.build_agent(&factory) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("jan-klod: agent boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Bridge the telegram poller's fetch to the real host-http client. The read
    // timeout must exceed the server-side long-poll window.
    let fetch = |method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>| {
        let owned: Vec<(String, String)> =
            headers.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        match jan_klod_core::http::fetch(method, url, &owned, body, 40_000) {
            Ok(response) => Ok(response.body),
            Err(err) => Err(format!("{err:?}")),
        }
    };

    println!("jan-klod: telegram bot polling (Ctrl-C to stop)");
    let mut offset = 0;
    let mut consecutive_errors: u32 = 0;
    loop {
        match jan_klod_core::telegram::poll_once(&mut agent, &fetch, &token, offset) {
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
    args.get(index).cloned().unwrap_or_else(|| default.to_string())
}

/// Like [`arg`], but a missing config/ext argument falls back to the installed
/// copy next to the binary rather than to a bare relative name.
fn arg_or(args: &[String], index: usize, default: &str) -> String {
    args.get(index).cloned().unwrap_or_else(|| resolve_default(default))
}
