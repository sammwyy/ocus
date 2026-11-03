use crate::settings::ThemeMode;
use crate::ui::{UiState, build};
use creamui::{AppBuilder, Theme, WindowOptions};

pub fn run_app() {
    let state = UiState::new();
    let builder = AppBuilder::new().system_theme().unwrap_or_else(|error| {
        eprintln!("Could not load system theme: {error}");
        AppBuilder::new()
    });
    let settings = state.settings.peek();
    builder
        .on_started(move |app| {
            state.connect(app.clone());
            let close = state.clone();
            let preview = state.clone();
            app.append_window(
                WindowOptions {
                    title: "Ocus".into(),
                    width: 1280,
                    height: 760,
                    decorations: true,
                    theme: if settings.theme == ThemeMode::System {
                        Theme::dark()
                    } else {
                        settings.theme()
                    },
                    ..Default::default()
                },
                settings.theme().colors.surface,
                move |window| {
                    if preview.settings.peek().theme != ThemeMode::System {
                        window.set_theme(preview.settings.peek().theme());
                    }
                    preview.connect_window(window.clone());
                    window.on_close_requested(move || {
                        if close.busy.peek() {
                            close
                                .status
                                .set("Wait for the current operation before closing".into());
                            return false;
                        }
                        if close.confirm_discard() {
                            if let Some(window) = close.settings_window() {
                                window.close();
                            }
                            close.pause();
                            close.pause_source();
                            true
                        } else {
                            false
                        }
                    });
                },
                move |viewport| build(&state, viewport),
            );
        })
        .run();
}
