use crate::project::Project;
use crate::ui::{BACKGROUND, build};
use creamui::{Theme, WindowOptions, run};

pub fn run_app() {
    let project = Project::new("Untitled");
    run(
        WindowOptions {
            title: "Ocus".into(),
            width: 1280,
            height: 800,
            theme: Theme::dark(),
            ..Default::default()
        },
        BACKGROUND,
        |_| {},
        move |viewport| build(&project, viewport),
    );
}
