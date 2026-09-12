//! Tauri's build step: reads `tauri.conf.json` and generates the context
//! `tauri::generate_context!()` expands to (the app identifier, the icon, the
//! window defaults).

fn main() {
    tauri_build::build();
}
