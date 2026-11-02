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
                        if close.dirty.peek() && !close.confirm_close.peek() {
                            close.confirm_close.set(true);
                            close
                                .status
                                .set("Unsaved changes · Close again to discard them".into());
                            false
                        } else {
                            true
                        }
                    });
                },
                move |viewport| build(&state, viewport),
            );
        })
        .run();
}
