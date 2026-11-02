//! GPUI user interface for FastDir: the main window, the list and treemap
//! panels, and the update dialogs.

use fastdir_settings::{Settings, ThemePreference};
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

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
            let settings = Settings::load();
            let mode = match settings.theme {
                ThemePreference::Light => ThemeMode::Light,
                ThemePreference::Dark => ThemeMode::Dark,
                ThemePreference::System => ThemeMode::from(cx.window_appearance()),
            };
            Theme::change(mode, None, cx);
            app::apply_accent(cx);
            app::bind_keys(cx);

            let options = WindowOptions {
                window_bounds: Some(app::restored_window_bounds(&settings, cx)),
                window_min_size: Some(size(px(900.), px(600.))),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                window.set_window_title("FastDir");
                cx.new(|cx| app::FastDir::new(settings, window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
}
