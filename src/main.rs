#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!(
            "ocus {} ({}; {}; {})",
            env!("CARGO_PKG_VERSION"),
            env!("OCUS_COMMIT_SHORT"),
            env!("OCUS_BRANCH"),
            env!("OCUS_CHANNEL")
        );
        return;
    }
    ocus::run_app();
}
