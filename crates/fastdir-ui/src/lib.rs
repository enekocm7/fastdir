//! GPUI user interface for FastDir: the main window, the list and treemap
//! panels, and the update dialogs.

use fastdir_settings::{Settings, ThemePreference};
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;
use std::path::PathBuf;

pub mod app;
pub mod treemap_view;

/// Starts the FastDir application and blocks until the last window closes.
///
/// The binary crate only calls this; all GPUI code lives in `fastdir-ui`.
pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            let mut settings = Settings::load();
            #[cfg(windows)]
            {
                let actual = !matches!(
                    fastdir_core::integration::context_menu_state(),
                    fastdir_core::integration::ContextMenuState::Absent
                );
                if settings.integration.context_menu != actual {
                    settings.integration.context_menu = actual;
                    settings.save();
                }
            }
            let mode = match settings.appearance.theme {
                ThemePreference::Light => ThemeMode::Light,
                ThemePreference::Dark => ThemeMode::Dark,
                ThemePreference::System => ThemeMode::from(cx.window_appearance()),
            };
            Theme::change(mode, None, cx);
            app::apply_accent(cx);
            app::apply_ui_scale(settings.appearance.ui_scale, cx);
            app::bind_keys(cx);

            let options = WindowOptions {
                window_bounds: Some(app::restored_window_bounds(&settings, cx)),
                window_min_size: Some(size(px(900.), px(600.))),
                ..TitleBar::window_options()
            };
            let initial_path = cli_open_path();
            let primary_listener = if settings.integration.single_instance {
                match fastdir_core::integration::try_claim(
                    fastdir_core::integration::SINGLE_INSTANCE_PORT,
                ) {
                    fastdir_core::integration::Claim::Primary(listener) => Some(listener),
                    fastdir_core::integration::Claim::Unavailable => {
                        // Another FastDir owns the port: hand it the folder and get out of the way.
                        // If forwarding fails (a stranger holds the port), degrade to a normal start.
                        if fastdir_core::integration::forward_open_request(
                            fastdir_core::integration::SINGLE_INSTANCE_PORT,
                            initial_path.as_deref(),
                        )
                        .is_ok()
                        {
                            std::process::exit(0);
                        }
                        None
                    }
                }
            } else {
                None
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                window.set_window_title("FastDir");
                let mut primary_listener = primary_listener;
                let listener = primary_listener.take();
                cx.new(|cx| app::FastDir::new(settings, initial_path, listener, window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
}

/// `fastdir <path>` argument: a folder to scan or a saved `.json` scan to
/// open.
fn cli_open_path() -> Option<PathBuf> {
    std::env::args_os().nth(1).and_then(|arg| {
        let mut arg = arg.to_string_lossy().trim().trim_matches('"').to_string();
        if arg.ends_with(':') {
            arg.push(std::path::MAIN_SEPARATOR);
        }
        let path = PathBuf::from(arg);
        let is_json = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        (path.is_dir() || is_json).then_some(path)
    })
}
