#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        let commit = env!("OCUS_COMMIT");
        println!(
            "ocus {} ({})",
            env!("CARGO_PKG_VERSION"),
            &commit[..commit.len().min(7)]
        );
        return;
    }
    ocus::run_app();
}
