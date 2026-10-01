//! Tray app. No console. `pairflow host` / `pairflow join` stay on the CLI binary.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    if let Err(err) = pairflow::gui_app::run() {
        eprintln!("pairflow: {err}");
        std::process::exit(1);
    }
}
