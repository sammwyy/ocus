use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn profile_output_dir<'a>(out_dir: &'a Path, profile: &std::ffi::OsStr) -> Option<&'a Path> {
    out_dir
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(profile))
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::var("CARGO_MANIFEST_DIR").ok()?)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn embed_commit() {
    for variable in [
        "OCUS_CHANNEL",
        "GITHUB_REF_NAME",
        "GITHUB_REF_TYPE",
        "GITHUB_BASE_REF",
        "CI",
        "PROFILE",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    if let Some(head) = git_output(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(branch) = git_output(&["symbolic-ref", "-q", "HEAD"])
        && let Some(reference) = git_output(&["rev-parse", "--git-path", &branch])
    {
        println!("cargo:rerun-if-changed={reference}");
    }
    let commit = git_output(&["rev-parse", "--verify", "HEAD"])
        .filter(|hash| hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=OCUS_COMMIT={commit}");
    println!(
        "cargo:rustc-env=OCUS_COMMIT_SHORT={}",
        &commit[..commit.len().min(7)]
    );
    let branch = env::var("GITHUB_BASE_REF")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("GITHUB_REF_NAME").ok())
        .or_else(|| git_output(&["symbolic-ref", "--short", "-q", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    let channel = env::var("OCUS_CHANNEL")
        .ok()
        .filter(|s| matches!(s.as_str(), "stable" | "nightly" | "indev"))
        .unwrap_or_else(|| {
            if env::var("CI").as_deref() != Ok("true")
                || env::var("PROFILE").as_deref() != Ok("release")
            {
                return "indev".into();
            }
            if env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag") && branch.starts_with('v') {
                return "stable".into();
            }
            match branch.as_str() {
                "main" | "stable" => "stable",
                "dev" | "development" | "nightly" => "nightly",
                _ => "indev",
            }
            .into()
        });
    println!("cargo:rustc-env=OCUS_BRANCH={branch}");
    println!("cargo:rustc-env=OCUS_CHANNEL={channel}");
}

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

fn main() {
    embed_commit();
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
        println!("cargo:warning=FFmpeg runtime DLL directory not found");
        return;
    };

    let (Some(out_dir), Some(profile)) = (env::var_os("OUT_DIR"), env::var_os("PROFILE")) else {
        println!("cargo:warning=Cannot locate DLL destination: OUT_DIR or PROFILE is missing");
        return;
    };
    let out_dir = PathBuf::from(out_dir);
    let Some(profile_dir) = profile_output_dir(&out_dir, &profile) else {
        println!(
            "cargo:warning=Cannot locate DLL destination for profile {} in {}",
            profile.to_string_lossy(),
            out_dir.display()
        );
        return;
    };
    let entries = match fs::read_dir(&bin_dir) {
        Ok(entries) => entries,
        Err(error) => {
            println!("cargo:warning=Cannot read {}: {error}", bin_dir.display());
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                println!("cargo:warning=Cannot read DLL directory entry: {error}");
                continue;
            }
        };
        let name = entry.file_name();
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
        {
            let destination = profile_dir.join(name);
            if let Err(error) = fs::copy(entry.path(), &destination) {
                println!(
                    "cargo:warning=Cannot copy {} to {}: {error}",
                    entry.path().display(),
                    destination.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::profile_output_dir;
    use std::{ffi::OsStr, path::Path};

    #[test]
    fn finds_profile_in_classic_and_nested_build_layouts() {
        for out_dir in [
            "target/release/build/ocus-hash/out",
            "target/release/build/ocus/hash/out",
            "target/x86_64-pc-windows-msvc/release/build/ocus-hash/out",
            "target/x86_64-pc-windows-msvc/release/build/ocus/hash/out",
        ] {
            let out_dir = Path::new(out_dir);
            let expected = if out_dir.starts_with("target/x86_64-pc-windows-msvc") {
                Path::new("target/x86_64-pc-windows-msvc/release")
            } else {
                Path::new("target/release")
            };
            assert_eq!(
                profile_output_dir(out_dir, OsStr::new("release")),
                Some(expected)
            );
        }
    }

    #[test]
    fn finds_nearest_matching_profile() {
        assert_eq!(
            profile_output_dir(
                Path::new("release/target/debug/build/ocus/hash/out"),
                OsStr::new("debug")
            ),
            Some(Path::new("release/target/debug"))
        );
        assert_eq!(
            profile_output_dir(
                Path::new("release/target/release/build/ocus/hash/out"),
                OsStr::new("release")
            ),
            Some(Path::new("release/target/release"))
        );
    }

    #[test]
    fn missing_profile_has_no_destination() {
        for out_dir in ["", "target/debug/build/ocus/hash/out"] {
            assert_eq!(
                profile_output_dir(Path::new(out_dir), OsStr::new("release")),
                None
            );
        }
    }
}
