//! GPUI user interface for FastDir: the main window, the list and treemap
//! panels, and the update dialogs.

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
            Theme::change(ThemeMode::Dark, None, cx);
            app::apply_accent(cx);
            app::bind_keys(cx);

            let bounds = Bounds::centered(None, size(px(1440.), px(920.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.), px(600.))),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                window.set_window_title("FastDir");
                cx.new(|cx| app::FastDir::new(window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
}
