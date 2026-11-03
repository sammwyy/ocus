use crate::ui::{BACKGROUND, UiState, build};
use creamui::{AppBuilder, Theme, WindowOptions};

pub fn run_app() {
    let state = UiState::new();
    AppBuilder::new()
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
                    theme: Theme::dark(),
                    ..Default::default()
                },
                BACKGROUND,
                move |window| {
                    preview.connect_window(window.clone());
                    window.on_close_requested(move || {
                        if close.busy.peek() {
                            close
                                .status
                                .set("Wait for the current operation before closing".into());
                            return false;
                        }
                        if close.confirm_discard() {
                            close.pause();
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
