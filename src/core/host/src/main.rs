//! `jan-klod` core entrypoint: boot the runtime from `config.yaml`, resolve the
//! enabled extensions against the `ext/` directory, and either print the boot plan
//! or serve the loop over the host-side REST surface. All behaviour lives in the
//! extensions it loads — this binary is just the container.
//!
//! Usage:
//!   `jan-klod [config-path] [ext-dir]`            — boot + print the plan
//!   `jan-klod serve [config-path] [ext-dir] [bind]` — boot + serve REST turns
//!   `jan-klod serve --bind <addr>`                  — serve, resolving paths
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
        Some("ask") => ask(&args[1..]),
        _ => boot_plan(&args),
    }
}

/// One question, one answer, on stdout.
///
/// The surface a self-hosted agent is most obviously missing: no server, no
/// client, nothing to leave running — `jan-klod-gateway ask "what does this repo
/// do?"` in a shell or a CI step. Two documents described this subcommand before
/// it existed, in text written to justify withholding stdin from guests; the
/// claim was invented and then half-believed two days later, which is a good
/// argument for `every_documented_command_exists` below it.
///
/// Confirmations are asked on the terminal, because there is one. Running headless
/// and letting the prompt default would deny every write and command — correct,
/// and useless: the whole fleet past `fs:read` would be unavailable in the surface
/// most likely to be scripted.
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
        eprint!("  [{}] (Enter = {}): ", prompt.options.join("/"), prompt.default_answer);
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

    // Live host-http, bounded by the egress policy: outbound calls reach public
    // destinations plus the endpoints config names, and nothing else. Handing a
    // guest `jan_klod_core::http::fetch` directly would let it reach this
    // gateway's own port, the cloud metadata service, and the LAN.
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

    let server = match tiny_http::Server::http(&bind) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("jan-klod: cannot bind {bind}: {err}");
            return ExitCode::FAILURE;
        }
    };
    // A secret belongs in the environment, not in a file people paste into issues.
    let token = std::env::var("JAN_KLOD_TOKEN").ok().filter(|t| !t.trim().is_empty());
    if token.is_some() {
        println!("jan-klod: requiring a bearer token (JAN_KLOD_TOKEN); /health stays open");
    }
    if let Some(warning) = exposure_warning(&bind, token.is_some()) {
        eprintln!("{warning}");
    }
    println!("jan-klod: serving on http://{bind} — POST {{\"session\":\"…\",\"message\":\"…\"}}");

    if let Err(err) = jan_klod_core::serve::serve_authed(&server, &mut agent, token.as_deref()) {
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

    // Bridge the telegram poller's fetch to the real host-http client. This one is
    // the *host's* own call to api.telegram.org, not a guest's, so it uses the
    // plain client — which now applies the public-only rule anyway. The read
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

/// A warning when `bind` exposes the surface with nothing guarding it.
///
/// Silent once a token is set: the point is to flag an *unguarded* exposure, and
/// a warning that persists after the reader has done the thing it asked for is
/// one they learn to ignore.
///
/// **The surface has no authentication.** Anyone who can reach it can start a
/// turn, and the agent behind it reads and writes a workspace and — where the
/// deployment allows it — runs commands. That is fine on `127.0.0.1`, which is
/// the default and what the TUI connects to. On a routable address it is an open
/// door, and the config file used to imply otherwise by advertising an `api-key:`
/// nothing ever read.
///
/// A warning rather than a refusal: binding elsewhere is legitimate behind a
/// reverse proxy that does authenticate, and a runtime that refuses a documented
/// address would be its own kind of lie. It should not be *quiet*, though.
fn exposure_warning(bind: &str, has_token: bool) -> Option<String> {
    if has_token {
        return None;
    }
    let host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost"
        || host == "::1"
        || host.strip_prefix("127.").is_some_and(|rest| rest.contains('.'));
    if loopback {
        return None;
    }
    Some(
        [
            format!("jan-klod: WARNING — binding to {bind}, which is not loopback."),
            "jan-klod: nothing guards this surface. Anyone who can reach it can drive".to_string(),
            "jan-klod: the agent, which reads and writes your workspace.".to_string(),
            "jan-klod: Set JAN_KLOD_TOKEN to require a bearer token, or bind 127.0.0.1.".to_string(),
        ]
        .join("\n"),
    )
}

/// Split `serve`'s arguments into positionals and an optional `--bind <addr>`.
///
/// The flag exists so a caller can name the address *without* claiming the
/// config and ext slots. Those are what [`arg_or`] resolves against the installed
/// data directory, and naming them explicitly — which the UI had to do to reach
/// the third positional — defeated that resolution: an installed jan-klod
/// launched from the user's own repository looked for `./config.yaml` and failed
/// to boot. The positional form still works for anyone naming all three.
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
/// The positional form is `serve [config] [ext] [addr]`, so slot 2 is legitimately
/// an address; only the first two are paths. Returns the message to print, or
/// `None` when the arguments are fine.
fn misplaced_address(positional: &[String]) -> Option<String> {
    let offender = positional.iter().take(2).find(|arg| looks_like_an_address(arg))?;
    Some(format!(
        "jan-klod: `{offender}` looks like an address, not a path. Use \
         `--bind {offender}` — a positional argument names a config file, and the \
         positional form only works inside a checkout anyway."
    ))
}

/// Whether a positional argument looks like a bind address rather than a path.
///
/// `serve 127.0.0.1:8787` is the obvious thing to type, and it was read as a
/// config path — so the error was `config file not found: 127.0.0.1:8787`, which
/// tells the reader nothing about the flag they omitted. The README recommended
/// the positional form, so it actively invited the mistake.
///
/// Deliberately narrow: `host:port` with a numeric port, or a bare `:port`. A real
/// path can contain a colon, so this must not claim one is an address unless the
/// tail is digits.
fn looks_like_an_address(arg: &str) -> bool {
    let Some((host, port)) = arg.rsplit_once(':') else { return false };
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
    args.get(index).cloned().unwrap_or_else(|| resolve_default(default))
}

#[cfg(test)]
mod tests {
    use super::split_serve_args;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn loopback_binds_are_not_warned_about() {
        for quiet in ["127.0.0.1:8787", "localhost:8787", "[::1]:8787", "127.1.2.3:9"] {
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
        for loud in ["0.0.0.0:8787", "192.168.1.10:8787", "[::]:8787", "10.0.0.5:80"] {
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
        assert!(positional.is_empty(), "no positionals claimed: {positional:?}");
        assert_eq!(bind.as_deref(), Some("127.0.0.1:9000"));
    }

    #[test]
    fn an_address_in_a_path_slot_names_the_flag() {
        use super::misplaced_address;
        // The obvious thing to type, previously reported as a missing config file.
        for typo in ["127.0.0.1:8787", "localhost:8787", "[::1]:8787", ":8787"] {
            let message = misplaced_address(&args([typo].as_ref()))
                .unwrap_or_else(|| panic!("{typo} should be recognised as an address"));
            assert!(message.contains("--bind"), "the message names the flag: {message}");
        }
        // Slot 2 *is* the address in the positional form, so it must not trip.
        assert!(misplaced_address(&args(["c.yaml", "ext", "1.2.3.4:1"].as_ref())).is_none());
        // And a real path is left alone, colon or not.
        for path in ["config.yaml", "ext", "/srv/jan-klod/config.yaml", "notes:2024/config.yaml"] {
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
        assert_eq!(bind, None, "no flag, so the third positional is the address");
    }

    #[test]
    fn a_bind_address_is_never_mistaken_for_a_path() {
        // Filtering by *value* would drop a positional that happened to equal the
        // address; consuming the token after the flag cannot.
        let (positional, bind) =
            split_serve_args(&args(["c.yaml", "--bind", "c.yaml"].as_ref()));
        assert_eq!(positional, args(["c.yaml"].as_ref()), "the config path survives");
        assert_eq!(bind.as_deref(), Some("c.yaml"));
    }

    #[test]
    fn a_dangling_flag_falls_back_to_the_default_address() {
        let (positional, bind) = split_serve_args(&args(["--bind"].as_ref()));
        assert!(positional.is_empty());
        assert_eq!(bind, None, "nothing followed it, so the caller gets the default");
    }
}
