#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Remove anything a previous update left behind before doing anything else.
    fastdir_updater::cleanup();
    fastdir_ui::run();
}
