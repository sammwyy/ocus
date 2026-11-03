use std::{env, fs, path::PathBuf};

fn ffmpeg_bin_dir() -> Option<PathBuf> {
    if let Ok(dir) = env::var("FFMPEG_DIR") {
        let bin = PathBuf::from(dir).join("bin");
        if bin.is_dir() {
            return Some(bin);
        }
    }
    if let Ok(root) = env::var("VCPKG_ROOT") {
        let bin = PathBuf::from(root)
            .join("installed")
            .join("x64-windows")
            .join("bin");
        if bin.is_dir() {
            return Some(bin);
        }
    }
    None
}

fn is_ffmpeg_dll(name: &str) -> bool {
    name.ends_with(".dll")
        && (name.starts_with("av")
            || name.starts_with("swresample")
            || name.starts_with("swscale")
            || name.starts_with("pkgconf"))
}

fn main() {
    #[cfg(windows)]
    winres::WindowsResource::new()
        .set_icon("assets/ocus.ico")
        .compile()
        .expect("failed to embed Ocus icon");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-changed=assets/ocus.ico");

    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let Some(bin_dir) = ffmpeg_bin_dir() else {
        return;
    };

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };

    for entry in fs::read_dir(bin_dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_ffmpeg_dll(name) {
            let _ = fs::copy(entry.path(), profile_dir.join(name));
        }
    }
}
