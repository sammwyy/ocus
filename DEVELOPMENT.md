# Building and developing Ocus

These instructions build the **current** Ocus desktop application. The [roadmap](ROADMAP.md)
describes a video editor in progress, not features already available. Media files can
currently be probed and imported using FFmpeg, but video decoding, playback and
MP4 export are not yet implemented. The current `Cargo.toml` uses the
`ffmpeg-next = "=7.1.0"` bindings, so FFmpeg development headers and shared
libraries **are required** to build, even when running headless tests.

The repository pins the published `creamui = "=0.3.0"` crate. This version can
build the window, but does not expose the GPU viewport and split-pane APIs
needed for the planned compositor. Do not replace it with a sibling checkout;
upgrade to a published release when those APIs become available.

## Common prerequisites

- Install a Rust toolchain with [rustup](https://rustup.rs/). Rust **1.85 or
  newer** is required for edition 2024; the project is tested here with stable
  Rust 1.97.1. Install the `rustfmt` and `clippy` components.
- Install your platform's native C/C++ build tools, `pkg-config` and FFmpeg
  development libraries as described below. `ffmpeg-sys-next` also runs
  `bindgen`, so provide a compatible `libclang`.
- Allow Cargo to download dependencies from crates.io on the first build.
- Use a graphical desktop session for `cargo run`; build and tests can run
  headlessly.

Run from the repository root (the directory containing `Cargo.toml`):

```sh
rustup toolchain install stable
rustup component add rustfmt clippy
cargo build --locked
cargo run --locked
```

`--locked` uses the checked-in `Cargo.lock`. Do not substitute a local CreamUI
path dependency to work around a missing published feature.

## Linux

### Fedora (including Fedora 42)

Install the tools needed for the current Rust build:

```sh
sudo dnf install gcc pkgconf-pkg-config clang-devel
```

Enable RPM Fusion Free if it is not already configured, then install the
development package matching the RPM Fusion FFmpeg libraries:

```sh
sudo dnf install ffmpeg-devel
pkg-config --modversion libavformat libavcodec libavutil libswscale libswresample
```

On the Fedora 42 development machine, this check currently succeeds. Do not
mix the Fedora `ffmpeg-free-devel` package with RPM Fusion `ffmpeg-libs` if
their versions or library variants conflict.

### Ubuntu / Debian

Install a C compiler, `pkg-config`, FFmpeg and `libclang`:

```sh
sudo apt update
sudo apt install build-essential pkg-config libavformat-dev libavcodec-dev libavutil-dev libswscale-dev libswresample-dev libclang-dev
pkg-config --modversion libavformat libavcodec libavutil libswscale libswresample
```

Use a Wayland or X11 desktop session to launch the window. Depending on the
distribution, native display development libraries may also be needed if the
windowing dependencies report a missing system library at build time. These
instructions have not been tested on Ubuntu/Debian in this repository.

The native file pickers use the desktop's XDG portal backend on Linux. Ensure
an appropriate portal implementation is running; `zenity` provides a fallback
for file and confirmation dialogs on desktops without that backend.

## macOS

Install Xcode Command Line Tools, Rust with rustup, and FFmpeg via Homebrew:

```sh
xcode-select --install
brew install ffmpeg pkgconf llvm
export LIBCLANG_PATH="$(brew --prefix llvm)/lib"
rustup toolchain install stable
rustup component add rustfmt clippy
cargo build --locked
```

Run `cargo run --locked` from a logged-in graphical session. Verify the FFmpeg
libraries are discoverable:

```sh
pkg-config --modversion libavformat libavcodec libavutil libswscale libswresample
```

The macOS build and FFmpeg integration have not been verified on a Mac yet.

## Windows

Install Rust with rustup using the **MSVC** toolchain and install Visual Studio
Build Tools with the **Desktop development with C++** workload and a Windows
SDK. In PowerShell, from the repository root:

```powershell
rustup default stable-x86_64-pc-windows-msvc
rustup component add rustfmt clippy
cargo build --locked
cargo run --locked
```

The current `ffmpeg-next` binding requires FFmpeg headers, import libraries
and runtime DLLs for the same architecture and ABI as the Rust MSVC target
(for example, through the `ffmpeg:x64-windows` port in vcpkg). It also needs
`libclang` for bindgen and build-time FFmpeg discovery configured for the
installation. Do not assume installing an `ffmpeg.exe` alone supplies
development libraries. Windows
FFmpeg discovery, H.264/AAC encoder availability, and the Windows build have
not yet been verified in this repository.

## Checks and troubleshooting

From the repository root, run the same checks used in [CONTRIBUTING.md](CONTRIBUTING.md):

```sh
cargo fmt --all --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
```

- A missing `libavformat.pc` means the FFmpeg **development** package is
  missing or `PKG_CONFIG_PATH` does not include its `.pc` directory.
- A missing `libclang` when building the binding requires a matching
  Clang installation and, if needed, `LIBCLANG_PATH`.
- `cargo run --locked` requires a desktop/display server. Headless tests
  exercise the model and CreamUI painting without a window. On a Linux desktop,
  try `CUI_OVERRIDE_RENDER_BACKEND=cpu cargo run --locked` if the GPU backend
  cannot initialize.
- A successful build is not proof of video playback, export or support on an
  untested operating system. See [TODO.md](TODO.md) for remaining work.
