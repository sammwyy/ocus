use crate::project::{Editor, Project};
use crate::ui::{BACKGROUND, build};
use creamui::{Signal, Theme, WindowOptions, run};
use std::cell::RefCell;
use std::rc::Rc;

pub fn run_app() {
    let editor = Rc::new(RefCell::new(Editor::new(Project::new("Untitled"))));
    let revision = Signal::new(0_u64);
    run(
        WindowOptions {
            title: "Ocus".into(),
            width: 1440,
            height: 900,
            decorations: true,
            theme: Theme::dark(),
            ..Default::default()
        },
        BACKGROUND,
        |_| {},
        move |viewport| {
            revision.get();
            build(&editor, &revision, viewport)
        },
    );
}
