//! `jan-klod` core entrypoint: boot the runtime from `config.yaml`, resolve
//! the enabled extensions against the `ext/` directory, run their lifecycle, and
//! print the boot plan. All behaviour lives in the extensions it loads — this
//! binary is just the container.
//!
//! Usage: `jan-klod [config-path] [ext-dir]`
//!   config-path  path to config.yaml   (default: config.yaml)
//!   ext-dir      directory of *.wasm     (default: ext)

use std::process::ExitCode;

use jan_klod_core::Runtime;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().unwrap_or_else(|| "config.yaml".to_string());
    let ext_dir = args.next().unwrap_or_else(|| "ext".to_string());

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
