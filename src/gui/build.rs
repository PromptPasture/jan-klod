//! Tauri build step: reads `tauri.conf.json`, generates context for
//! `tauri::generate_context!()` (app id, icon, window defaults).

fn main() {
    tauri_build::build();
}
