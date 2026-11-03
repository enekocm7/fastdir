#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `fastdir scan ...`, `fastdir --help` etc. run headless: no window, no
    // single-instance hand-off, no update cleanup. Release builds are a GUI
    // subsystem program on Windows, so borrow the parent's console first.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if fastdir_cli::is_cli_invocation(&args) {
        fastdir_cli::attach_parent_console();
        std::process::exit(fastdir_cli::run(args));
    }
    // Remove anything a previous update left behind before doing anything else.
    fastdir_updater::cleanup();
    fastdir_ui::run();
}
