//! `jan-klod` core entrypoint: boot the runtime from `jan-klod.yaml`, resolve
//! the enabled extensions against the `ext/` directory, run their lifecycle, and
//! print the boot plan. All behaviour lives in the extensions it loads — this
//! binary is just the container.
//!
//! With `--turn "<prompt>"` it instead drives one agent turn end to end through
//! the Component Model ([`jan_klod_host::agent`]): complete the prompt through
//! the enabled provider, then persist it into the enabled store. That live path
//! needs network access and the provider's api-key env (e.g. `OPENAI_API_KEY`).
//!
//! Usage: `jan-klod [config-path] [ext-dir] [--turn "<prompt>"]`
//!   config-path  path to jan-klod.yaml   (default: jan-klod.yaml)
//!   ext-dir      directory of *.wasm     (default: ext)
//!   --turn TEXT  run one agent turn with TEXT as the prompt (live; else boot+lifecycle)

use std::process::ExitCode;

use jan_klod_core::Runtime;
use jan_klod_host::agent::{self, HttpFn};

fn main() -> ExitCode {
    // Split off `--turn <prompt>`; the rest are the positional config/ext args.
    let mut prompt = None;
    let mut positionals = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--turn" {
            prompt = Some(args.next().unwrap_or_default());
        } else {
            positionals.push(arg);
        }
    }
    let mut positionals = positionals.into_iter();
    let config_path = positionals.next().unwrap_or_else(|| "jan-klod.yaml".to_string());
    let ext_dir = positionals.next().unwrap_or_else(|| "ext".to_string());

    if let Some(prompt) = prompt {
        return run_turn(&config_path, &ext_dir, &prompt);
    }

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

/// Drive one agent turn against the live provider/store, with the real blocking
/// HTTP client wired in, and print the transcript.
fn run_turn(config_path: &str, ext_dir: &str, prompt: &str) -> ExitCode {
    let http: HttpFn = Box::new(jan_klod_core::http::fetch);
    match agent::run_turn(config_path, ext_dir, prompt, http) {
        Ok(turn) => {
            println!("prompt:   {}", turn.prompt);
            println!("response: {}", turn.response);
            println!("done:     {}", turn.done_reason);
            println!(
                "stored:   {}/{} -> id {} ({} bytes)",
                turn.namespace,
                turn.key,
                turn.stored_id,
                turn.stored_value.len()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("jan-klod: turn failed: {err}");
            ExitCode::FAILURE
        }
    }
}
