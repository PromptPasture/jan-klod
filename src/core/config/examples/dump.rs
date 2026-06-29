//! Resolve a `jan-klod.yaml` and print the extension plan.
//!
//! Usage: `cargo run -p jan-klod-config --example dump -- [path]`
//! (defaults to `jan-klod.yaml` in the current directory).

use jan_klod_config::Config;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "jan-klod.yaml".to_string());

    let config = match Config::from_path(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    println!("# {path} — {} instance(s)\n", config.instances.len());
    for inst in &config.instances {
        let state = if inst.enabled { "enabled " } else { "disabled" };
        println!("[{state}] {:<22} -> {}", inst.id, inst.component_file());
    }
    println!("\n{} enabled.", config.enabled().count());
}
