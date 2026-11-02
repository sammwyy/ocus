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

Use the published `creamui` crate from crates.io, never a sibling path
dependency. The pinned 0.3.0 release does not yet expose the GPU viewport and
split-pane APIs available in the neighboring checkout; do not use those APIs
until they are published. See [TODO.md](TODO.md) for that release dependency.

From the repository root, run `cargo fmt --all --check`,
`cargo test --all-targets --locked`, and
`cargo clippy --all-targets --locked -- -D warnings`. UI rendering can be tested
without a display; `cargo run --locked` needs a desktop session.

Keep changes scoped to Ocus unless explicitly asked otherwise. Review the
working tree before editing; never rewrite history or push without a request.
