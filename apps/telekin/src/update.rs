//! Finding out whether a newer viewer exists.
//!
//! One HTTPS GET of a small JSON manifest, on a thread of its own, at start.
//! It never installs anything: the installers already know how to upgrade in
//! place (Inno Setup on Windows, `apt` on Ubuntu, dragging a .app on macOS),
//! so the honest job here is to *say* that a newer one is available and hand
//! over the download, not to replace a running binary underneath the user.
//!
//! A robot LAN often has no route to the internet. That is not an error worth
//! a banner — the check quietly reports "could not check" and the app carries
//! on exactly as before.

use std::sync::{Arc, Mutex};

use serde::Deserialize;

/// The version this binary was built as.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Where the manifest lives by default: the `latest.json` attached to the
/// newest GitHub release. GitHub redirects `releases/latest/download/<asset>`
/// to whichever release is current, so this URL never has to change.
pub const DEFAULT_MANIFEST: &str =
    "https://github.com/phuwanat-vg/telekin/releases/latest/download/latest.json";

/// Environment variable that points the check somewhere else — a lab's own
/// web server, or a file server with no internet behind it.
pub const MANIFEST_ENV: &str = "TELEKIN_UPDATE_URL";

/// How long to wait for the manifest before giving up. Short, because on an
/// air-gapped network the DNS lookup itself is what fails, and nobody should
/// watch a spinner for that.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// What `latest.json` looks like. Written by `packaging/make-manifest.py`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// Release notes or a one-line summary, shown beside the button.
    #[serde(default)]
    pub notes: String,
    /// Download URLs per platform.
    #[serde(default)]
    pub downloads: Downloads,
    /// Where a person should go if nothing matches their platform.
    #[serde(default)]
    pub page: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Downloads {
    #[serde(default)]
    pub windows: Option<String>,
    #[serde(default)]
    pub linux_x86_64: Option<String>,
    #[serde(default)]
    pub linux_aarch64: Option<String>,
    #[serde(default)]
    pub macos: Option<String>,
}

/// The outcome of a check, for the UI to draw.
#[derive(Debug, Clone)]
pub enum Outcome {
    Checking,
    UpToDate,
    Available {
        version: String,
        notes: String,
        /// The download for *this* platform, or the release page.
        url: String,
    },
    /// The check could not be made. Kept short and unalarming: this is the
    /// normal case on a network with no internet.
    Unavailable(String),
}

pub type Shared = Arc<Mutex<Outcome>>;

/// Start a check in the background. Returns the slot the result lands in.
pub fn start(url: Option<String>) -> Shared {
    let slot = Arc::new(Mutex::new(Outcome::Checking));
    let url = url
        .or_else(|| std::env::var(MANIFEST_ENV).ok())
        .unwrap_or_else(|| DEFAULT_MANIFEST.to_string());
    let out = slot.clone();
    std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let outcome = match fetch(&url) {
                Ok(manifest) => decide(CURRENT, &manifest),
                Err(e) => Outcome::Unavailable(e),
            };
            *out.lock().expect("update slot poisoned") = outcome;
        })
        .expect("spawn update thread");
    slot
}

fn fetch(url: &str) -> Result<Manifest, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .user_agent(&format!("telekin/{CURRENT}"))
        .build();
    let body = agent
        .get(url)
        .call()
        .map_err(|e| short(&e.to_string()))?
        .into_string()
        .map_err(|e| short(&e.to_string()))?;
    serde_json::from_str(&body).map_err(|e| format!("manifest is not valid: {e}"))
}

/// The first line of an error, which is the part a person can act on.
fn short(e: &str) -> String {
    e.lines().next().unwrap_or(e).trim().to_string()
}

/// Compare what is running with what the manifest offers.
pub fn decide(current: &str, manifest: &Manifest) -> Outcome {
    match (parse(current), parse(&manifest.version)) {
        (Some(here), Some(there)) if there > here => Outcome::Available {
            version: manifest.version.clone(),
            notes: manifest.notes.clone(),
            url: download_for_this_platform(manifest),
        },
        (Some(_), Some(_)) => Outcome::UpToDate,
        _ => Outcome::Unavailable(format!(
            "could not compare versions ({current} vs {})",
            manifest.version
        )),
    }
}

fn download_for_this_platform(m: &Manifest) -> String {
    let specific = if cfg!(target_os = "windows") {
        m.downloads.windows.clone()
    } else if cfg!(target_os = "macos") {
        m.downloads.macos.clone()
    } else if cfg!(target_arch = "aarch64") {
        m.downloads.linux_aarch64.clone()
    } else {
        m.downloads.linux_x86_64.clone()
    };
    specific
        .or_else(|| m.page.clone())
        .unwrap_or_else(|| "https://github.com/phuwanat-vg/telekin/releases/latest".to_string())
}

/// `1.2.3` → `(1, 2, 3)`. A leading `v` is tolerated because tags have one.
///
/// Deliberately not a full semver parser: pre-release suffixes are not
/// something this project ships, and a manifest that carries one is a
/// manifest that should fail the check loudly rather than be half-understood.
pub fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = parts.next()??;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str) -> Manifest {
        Manifest {
            version: version.into(),
            notes: String::new(),
            downloads: Downloads::default(),
            page: Some("https://example.invalid/releases".into()),
        }
    }

    #[test]
    fn versions_parse_the_way_tags_are_written() {
        assert_eq!(parse("1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse(" 10.0.1 "), Some((10, 0, 1)));
        assert_eq!(parse("1.0"), None, "two parts is not a version we ship");
        assert_eq!(parse("1.0.0-beta"), None, "pre-releases are refused, not guessed");
        assert_eq!(parse("1.0.0.0"), None);
    }

    #[test]
    fn a_newer_manifest_is_offered_and_an_older_one_is_not() {
        assert!(matches!(decide("1.0.0", &manifest("1.0.1")), Outcome::Available { .. }));
        assert!(matches!(decide("1.0.0", &manifest("2.0.0")), Outcome::Available { .. }));
        assert!(matches!(decide("1.0.0", &manifest("1.0.0")), Outcome::UpToDate));
        // Running a dev build newer than the release is not "an update".
        assert!(matches!(decide("1.1.0", &manifest("1.0.9")), Outcome::UpToDate));
    }

    #[test]
    fn numeric_not_lexical() {
        // "1.10.0" > "1.9.0", which a string comparison gets backwards.
        assert!(matches!(decide("1.9.0", &manifest("1.10.0")), Outcome::Available { .. }));
    }

    #[test]
    fn a_broken_manifest_does_not_pretend_to_be_an_update() {
        assert!(matches!(decide("1.0.0", &manifest("soon")), Outcome::Unavailable(_)));
    }

    #[test]
    fn the_download_falls_back_to_the_release_page() {
        let m = manifest("9.9.9");
        match decide("1.0.0", &m) {
            Outcome::Available { url, .. } => assert_eq!(url, "https://example.invalid/releases"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_built_version_is_a_real_version() {
        // Guards the release process: a bad workspace version would ship a
        // viewer that can never see an update.
        assert!(parse(CURRENT).is_some(), "CARGO_PKG_VERSION = {CURRENT:?}");
    }
}
