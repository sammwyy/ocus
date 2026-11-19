use std::{path::Path, process::Command};

#[test]
fn version_exits_without_a_display() {
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_ocus"))
            .arg(flag)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .expect("run Ocus headlessly");
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let commit = env!("OCUS_COMMIT");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!(
                "ocus {} ({})",
                env!("CARGO_PKG_VERSION"),
                &commit[..commit.len().min(7)]
            )
        );
    }
}

#[test]
fn installer_metadata_has_assets_and_platform_formats() {
    let manifest: toml::Value = toml::from_str(include_str!("../Cargo.toml")).unwrap();
    let metadata = &manifest["package"]["metadata"];
    let packager = &metadata["packager"];
    assert_eq!(packager["product-name"].as_str(), Some("Ocus"));
    assert_eq!(packager["identifier"].as_str(), Some("dev.sammwy.ocus"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for icon in packager["icons"].as_array().unwrap() {
        assert!(root.join(icon.as_str().unwrap()).is_file());
    }
    assert!(
        root.join(packager["nsis"]["installer-icon"].as_str().unwrap())
            .is_file()
    );
    for resource in packager["resources"].as_array().unwrap() {
        let resource = resource.as_str().unwrap();
        if let Some(parent) = resource.strip_suffix("/*.dll") {
            if cfg!(windows) {
                assert!(
                    root.join(parent).is_dir(),
                    "build the release binary before testing Windows packaging"
                );
                assert!(
                    std::fs::read_dir(root.join(parent)).unwrap().any(|entry| {
                        entry
                            .unwrap()
                            .path()
                            .extension()
                            .is_some_and(|ext| ext == "dll")
                    }),
                    "Windows packages must contain runtime DLLs"
                );
            }
        } else {
            assert!(root.join(resource).exists(), "missing resource {resource}");
        }
    }
    for (platform, expected) in [
        ("linux", vec!["deb", "appimage"]),
        ("macos", vec!["app", "dmg"]),
        ("windows", vec!["nsis"]),
    ] {
        let formats = metadata["installer-formats"][platform].as_array().unwrap();
        assert_eq!(
            formats
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert_eq!(packager["formats"], metadata["installer-formats"]["linux"]);
}
