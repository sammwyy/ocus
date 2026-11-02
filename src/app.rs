use crate::ui::{BACKGROUND, UiState, build};
use creamui::{AppBuilder, Theme, WindowOptions};

pub fn run_app() {
    let state = UiState::new();
    AppBuilder::new()
        .on_started(move |app| {
            state.connect(app.clone());
            let close = state.clone();
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
                    window.on_close_requested(move || {
                        if close.busy.peek() {
                            close
                                .status
                                .set("Wait for the current operation before closing".into());
                            return false;
                        }
                        close.confirm_discard()
                    });
                },
                move |viewport| build(&state, viewport),
            );
        })
        .run();
}
