//! Checking the signed manifest and downloading a verified package.

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::manifest::{self, Manifest, PlatformAsset};
use super::{CURRENT_VERSION, Error, MANIFEST_URL, PUBLIC_KEY, Result, hex, platform_key};

/// A manifest is a few kilobytes; refuse anything that pretends otherwise.
const MAX_MANIFEST_BYTES: usize = 128 * 1024;
/// Downloads larger than this are not a Calm Toad release.
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const READ_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: String,
    pub release_url: Option<String>,
    pub asset: PlatformAsset,
}

pub struct DownloadedUpdate {
    pub version: String,
    pub path: PathBuf,
    /// Kept alive for as long as the package is needed.
    _dir: tempfile::TempDir,
}

impl std::fmt::Debug for DownloadedUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadedUpdate")
            .field("version", &self.version)
            .field("path", &self.path)
            .finish()
    }
}

impl DownloadedUpdate {
    /// A stand-in package for the headless update scenes.
    #[doc(hidden)]
    pub fn placeholder(version: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary update folder");
        let path = dir.path().join("package");
        let _ = std::fs::write(&path, b"");
        Self {
            version: version.into(),
            path,
            _dir: dir,
        }
    }
}

/// Check the stable release feed for a newer version on this platform.
pub fn check() -> Result<Option<AvailableUpdate>> {
    if std::env::var_os("CALMTOAD_DISABLE_UPDATE_CHECK").is_some() {
        return Ok(None);
    }
    #[cfg(debug_assertions)]
    let url =
        std::env::var("CALMTOAD_UPDATE_MANIFEST_URL").unwrap_or_else(|_| MANIFEST_URL.to_string());
    #[cfg(not(debug_assertions))]
    let url = MANIFEST_URL.to_string();

    check_at(&url, CURRENT_VERSION)
}

/// Testable form of [`check`]: no environment, explicit address and current version.
pub fn check_at(url: &str, current: &str) -> Result<Option<AvailableUpdate>> {
    let platform = platform_key()
        .ok_or_else(|| Error::new("Calm Toad updates are not available for this processor yet."))?;
    let agent = agent();

    // Fetch and verify the exact bytes before parsing anything in them.
    let manifest_bytes = get_bytes(&agent, url, MAX_MANIFEST_BYTES)?;
    let signature_url = format!("{url}.minisig");
    let signature = get_bytes(&agent, &signature_url, MAX_MANIFEST_BYTES)?;
    manifest::verify(&manifest_bytes, &signature, &[PUBLIC_KEY])?;

    let manifest = Manifest::parse(&manifest_bytes)?;
    if !manifest.is_newer_than(current) {
        return Ok(None);
    }
    let asset = manifest.validate(platform, current)?.clone();

    Ok(Some(AvailableUpdate {
        version: manifest.version,
        notes: manifest.notes.unwrap_or_default(),
        release_url: manifest.release_url,
        asset,
    }))
}

/// Download and verify a package. Cancellation is checked between chunks.
pub fn download(
    update: &AvailableUpdate,
    cancel: &AtomicBool,
    progress: impl Fn(u64, Option<u64>),
) -> Result<DownloadedUpdate> {
    let agent = agent();
    let response = agent
        .get(&update.asset.url)
        .call()
        .map_err(|error| Error::new(format!("The update could not be downloaded: {error}")))?;
    validate_response_url(response.get_url())?;

    let expected = update.asset.size;
    if let Some(length) = response
        .header("Content-Length")
        .and_then(|header| header.parse::<u64>().ok())
        && length != expected
    {
        return Err(Error::new(
            "The update download changed size; it was not installed.",
        ));
    }

    let dir = tempfile::Builder::new()
        .prefix("calmtoad-update-")
        .tempdir()
        .map_err(|error| Error::new(format!("Couldn’t make a download folder: {error}")))?;
    let path = dir.path().join("package");
    let mut file = File::create(&path)?;
    let mut reader = response.into_reader();
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buffer = vec![0u8; READ_CHUNK];

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::new("Update cancelled."));
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| Error::new(format!("The download stopped: {error}")))?;
        if read == 0 {
            break;
        }
        received = received.saturating_add(read as u64);
        if received > expected || received > MAX_ARCHIVE_BYTES {
            return Err(Error::new(
                "The download was larger than the release said; it was discarded.",
            ));
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        progress(received, Some(expected));
    }
    file.flush()?;

    if received != expected {
        return Err(Error::new(format!(
            "The download ended at {received} of {expected} bytes; it was discarded."
        )));
    }
    let digest = hex(&hasher.finalize());
    if !digest.eq_ignore_ascii_case(&update.asset.sha256) {
        return Err(Error::new(
            "The update’s checksum did not match; it was discarded.",
        ));
    }

    Ok(DownloadedUpdate {
        version: update.version.clone(),
        path,
        _dir: dir,
    })
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(20))
        .redirects(5)
        .user_agent(&format!("Calm Toad/{CURRENT_VERSION} updater"))
        .build()
}

fn get_bytes(agent: &ureq::Agent, url: &str, limit: usize) -> Result<Vec<u8>> {
    let response = agent
        .get(url)
        .call()
        .map_err(|error| Error::new(format!("Couldn’t check for updates: {error}")))?;
    validate_response_url(response.get_url())?;
    if let Some(length) = response
        .header("Content-Length")
        .and_then(|header| header.parse::<u64>().ok())
        && length > limit as u64
    {
        return Err(Error::new("The update manifest was unexpectedly large."));
    }
    let mut reader = response.into_reader().take(limit as u64 + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| Error::new(format!("Couldn’t read the update manifest: {error}")))?;
    if bytes.len() > limit {
        return Err(Error::new("The update manifest was unexpectedly large."));
    }
    Ok(bytes)
}

fn validate_response_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url)?;
    if parsed.scheme() != "https" {
        return Err(Error::new("The update server did not use HTTPS."));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host == "github.com" || host.ends_with(".githubusercontent.com") {
        Ok(())
    } else {
        Err(Error::new(
            "The update server redirected somewhere unexpected; it was ignored.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_must_stay_on_github_or_its_asset_hosts() {
        assert!(validate_response_url("https://github.com/a/b").is_ok());
        assert!(validate_response_url("https://release-assets.githubusercontent.com/x").is_ok());
        assert!(validate_response_url("https://example.com/x").is_err());
        assert!(validate_response_url("http://github.com/a/b").is_err());
    }
}
