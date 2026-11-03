# Development guidance

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow and commit conventions,
[CODESTYLE.md](CODESTYLE.md) for source rules, and
[ARCHITECTURE.md](ARCHITECTURE.md) before changing module boundaries.
[ROADMAP.md](ROADMAP.md) is a plan, not a list of implemented features.

Organize code by editor responsibility: `app` starts and wires the window;
`project` owns saved state, its format, and edit history; `timeline` owns
track/clip data; `media` owns source references and, when implemented,
decoding; `ui` owns CreamUI widgets and transient interaction state. Project
edits must go through commands, not direct UI mutation. Keep decoding,
encoding, and filesystem work off the UI thread. Add modules only when they
have working code and tests; see [ARCHITECTURE.md](ARCHITECTURE.md).

Use the pinned GitHub revision of `creamui` in `Cargo.toml`, never a sibling
path dependency. The published GPU viewport and background stream APIs are
required for video preview; update the pinned commit only after verifying it
is available on GitHub and running the checks in both repositories.

From the repository root, run `cargo fmt --all --check`,
`cargo test --all-targets --locked`, and
`cargo clippy --all-targets --locked -- -D warnings`. UI rendering can be tested
without a display; `cargo run --locked` needs a desktop session.

Keep changes scoped to Ocus unless explicitly asked otherwise. Review the
working tree before editing; never rewrite history or push without a request.
