use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct Remote {
    tag_name: Option<String>,
    sha: Option<String>,
}

fn compare(channel: &str, version: &str, commit: &str, body: &str) -> Result<String, String> {
    let remote: Remote = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if channel == "stable" {
        let tag = remote
            .tag_name
            .ok_or("GitHub response has no release tag")?;
        let latest = semver::Version::parse(tag.strip_prefix('v').unwrap_or(&tag))
            .map_err(|e| format!("Invalid release version: {e}"))?;
        let current = semver::Version::parse(version).map_err(|e| e.to_string())?;
        return Ok(if latest > current {
            format!(
                "Update available: {tag} (installed {version}). Visit https://github.com/sammwyy/ocus/releases"
            )
        } else {
            format!("Up to date: {version} (latest release {tag}).")
        });
    }
    let sha = remote
        .sha
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("GitHub response has no valid commit SHA")?;
    let short = &sha[..7];
    Ok(if channel == "indev" {
        format!("Development build; remote nightly head: {short}. Updates are not compared.")
    } else if sha == commit {
        format!("Up to date: nightly {short}.")
    } else {
        format!(
            "Update available: nightly {short} (installed {}).",
            &commit[..commit.len().min(7)]
        )
    })
}

fn request_error(channel: &str, status: Option<u16>, detail: &str) -> String {
    match status {
        Some(404) if channel == "stable" => "No stable release published yet.".into(),
        Some(403 | 429) => "GitHub rate limit reached, try again later.".into(),
        _ => format!("Could not reach GitHub: {detail}"),
    }
}

pub fn check() -> Result<String, String> {
    let channel = env!("OCUS_CHANNEL");
    let endpoint = if channel == "stable" {
        "releases/latest"
    } else {
        "commits/nightly"
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let body = agent
        .get(format!(
            "https://api.github.com/repos/sammwyy/ocus/{endpoint}"
        ))
        .header("User-Agent", "Ocus update checker")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| {
            let status = match &e {
                ureq::Error::StatusCode(status) => Some(*status),
                _ => None,
            };
            request_error(channel, status, &e.to_string())
        })?
        .body_mut()
        .read_to_string()
        .map_err(|e| request_error(channel, None, &e.to_string()))?;
    compare(
        channel,
        env!("CARGO_PKG_VERSION"),
        env!("OCUS_COMMIT"),
        &body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_errors_are_friendly() {
        assert_eq!(
            request_error("stable", Some(404), "Not Found"),
            "No stable release published yet."
        );
        for channel in ["stable", "nightly", "indev"] {
            for status in [403, 429] {
                assert_eq!(
                    request_error(channel, Some(status), "limited"),
                    "GitHub rate limit reached, try again later."
                );
            }
            assert_eq!(
                request_error(channel, Some(500), "server error"),
                "Could not reach GitHub: server error"
            );
            assert_eq!(
                request_error(channel, None, "connection failed"),
                "Could not reach GitHub: connection failed"
            );
        }
        assert_eq!(
            request_error("nightly", Some(404), "Not Found"),
            "Could not reach GitHub: Not Found"
        );
    }

    #[test]
    fn release_versions_use_semver_order() {
        for (remote, available) in [
            ("v0.10.0", true),
            ("v0.2.0", false),
            ("v0.1.9", false),
            ("v0.2.0-rc.1", false),
        ] {
            let body = format!(r#"{{"tag_name":"{remote}"}}"#);
            assert_eq!(
                compare("stable", "0.2.0", "", &body)
                    .unwrap()
                    .starts_with("Update available"),
                available
            );
        }
    }

    #[test]
    fn nightly_and_development_comparisons() {
        let sha = "a".repeat(40);
        let body = format!(r#"{{"sha":"{sha}"}}"#);
        assert!(
            compare("nightly", "0.1.0", &sha, &body)
                .unwrap()
                .starts_with("Up to date")
        );
        assert!(
            compare("nightly", "0.1.0", "unknown", &body)
                .unwrap()
                .starts_with("Update available")
        );
        assert!(
            compare("indev", "0.1.0", "unknown", &body)
                .unwrap()
                .starts_with("Development build")
        );
    }

    #[test]
    fn invalid_and_empty_responses_are_errors() {
        for body in [
            "",
            "not json",
            "{}",
            r#"{"sha":"short","tag_name":"invalid"}"#,
        ] {
            assert!(compare("stable", "0.1.0", "", body).is_err());
            assert!(compare("nightly", "0.1.0", "", body).is_err());
        }
    }

    #[test]
    #[ignore = "requires GitHub network access"]
    fn github_update_check() {
        println!(
            "{}: {}",
            env!("OCUS_CHANNEL"),
            check().unwrap_or_else(|error| error)
        );
    }
}
