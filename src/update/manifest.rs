//! The release manifest and its signature.
//!
//! The manifest is small JSON, signed as a whole with minisign. Reading it is deliberately
//! stricter than parsing JSON: the schema, version, platform, and download address all have
//! to make sense before the updater will show an update.

use std::collections::BTreeMap;

use semver::Version;
use serde::Deserialize;

use super::{Error, REPOSITORY, Result};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub schema_version: u32,
    pub version: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub release_url: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub platforms: BTreeMap<String, PlatformAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformAsset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Everything the updater relies on, checked before it is allowed to act.
    pub fn validate(&self, platform: &str, current: &str) -> Result<&PlatformAsset> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::new(format!(
                "This update uses manifest schema {}, but this app understands {}.",
                self.schema_version, SCHEMA_VERSION
            )));
        }
        let offered = Version::parse(&self.version)?;
        if !offered.pre.is_empty() {
            return Err(Error::new(
                "This release is a prerelease; stable builds do not install prereleases.",
            ));
        }
        let installed = Version::parse(current)?;
        if offered <= installed {
            return Err(Error::new(format!(
                "The release server offered {offered}, which is not newer than {installed}."
            )));
        }
        if let Some(url) = &self.release_url {
            validate_release_url(url)?;
        }
        let asset = self.platforms.get(platform).ok_or_else(|| {
            Error::new(format!(
                "Version {} has no download for this computer ({platform}).",
                self.version
            ))
        })?;
        asset.validate()?;
        Ok(asset)
    }

    pub fn is_newer_than(&self, current: &str) -> bool {
        match (Version::parse(&self.version), Version::parse(current)) {
            (Ok(offered), Ok(installed)) => offered.pre.is_empty() && offered > installed,
            _ => false,
        }
    }
}

impl PlatformAsset {
    pub fn validate(&self) -> Result<()> {
        if self.size == 0 || self.size > super::client::MAX_ARCHIVE_BYTES {
            return Err(Error::new(
                "The update package has an impossible size; it was not downloaded.",
            ));
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::new("The update package has an invalid checksum."));
        }
        validate_asset_url(&self.url)
    }
}

/// The manifest may only point at HTTPS GitHub release assets.
pub fn validate_asset_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url)?;
    if parsed.scheme() != "https" {
        return Err(Error::new("The update download must use HTTPS."));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host != "github.com" && !host.ends_with(".githubusercontent.com") {
        return Err(Error::new(
            "The update download does not come from GitHub; it was ignored.",
        ));
    }
    if host == "github.com" {
        let prefix = format!("/{REPOSITORY}/releases/download/");
        if !parsed.path().starts_with(&prefix) {
            return Err(Error::new(
                "The update download does not point at a Calm Toad release.",
            ));
        }
    }
    Ok(())
}

/// Release notes links are only ever opened by the user, but keep them on GitHub too.
pub fn validate_release_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url)?;
    if parsed.scheme() != "https" {
        return Err(Error::new("The release page must use HTTPS."));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host != "github.com"
        || !parsed
            .path()
            .starts_with(&format!("/{REPOSITORY}/releases/"))
    {
        return Err(Error::new(
            "The manifest's release page does not point at this repository.",
        ));
    }
    Ok(())
}

/// Verify the exact manifest bytes against one of the trusted public keys.
///
/// The signature file is minisign's text format. `trusted_comment` is ignored for trust;
/// the signature itself covers the manifest and that trusted comment.
pub fn verify(bytes: &[u8], signature: &[u8], public_keys: &[&str]) -> Result<()> {
    let signature_text = std::str::from_utf8(signature)
        .map_err(|_| Error::new("The update signature is not text."))?;
    let signature = minisign_verify::Signature::decode(signature_text)
        .map_err(|_| Error::new("The update signature is malformed."))?;

    let mut error = Error::new("The update is not signed by a trusted key.");
    for key in public_keys {
        let encoded = key
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
            .unwrap_or_default();
        let public_key = match minisign_verify::PublicKey::from_base64(encoded) {
            Ok(key) => key,
            Err(_) => continue,
        };
        match public_key.verify(bytes, &signature, false) {
            Ok(()) => return Ok(()),
            Err(err) => error = Error::new(format!("The update signature did not verify: {err}")),
        }
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset() -> PlatformAsset {
        PlatformAsset {
            url: format!(
                "https://github.com/{REPOSITORY}/releases/download/v9.0.0/Calm-Toad-Mac-Apple-Silicon.zip"
            ),
            sha256: "a".repeat(64),
            size: 10,
        }
    }

    fn manifest(version: &str) -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
            version: version.into(),
            published_at: None,
            release_url: None,
            notes: None,
            platforms: BTreeMap::from([("macos-aarch64".into(), asset())]),
        }
    }

    #[test]
    fn only_newer_stable_versions_are_offered() {
        assert!(manifest("0.2.0").is_newer_than("0.1.1"));
        assert!(!manifest("0.1.1").is_newer_than("0.1.1"));
        assert!(!manifest("0.1.0").is_newer_than("0.1.1"));
        assert!(!manifest("0.2.0-rc.1").is_newer_than("0.1.1"));
    }

    #[test]
    fn validates_the_platform_entry_and_checksum() {
        assert!(manifest("0.2.0").validate("macos-aarch64", "0.1.1").is_ok());
        assert!(
            manifest("0.2.0")
                .validate("windows-x86_64", "0.1.1")
                .is_err()
        );

        let mut bad = manifest("0.2.0");
        bad.platforms.get_mut("macos-aarch64").unwrap().sha256 = "nope".into();
        assert!(bad.validate("macos-aarch64", "0.1.1").is_err());
    }

    #[test]
    fn rejects_manifests_that_point_away_from_github() {
        let mut bad = manifest("0.2.0");
        bad.platforms.get_mut("macos-aarch64").unwrap().url =
            "https://example.com/Calm-Toad.zip".into();
        assert!(bad.validate("macos-aarch64", "0.1.1").is_err());

        let mut bad = manifest("0.2.0");
        bad.platforms.get_mut("macos-aarch64").unwrap().url =
            format!("http://github.com/{REPOSITORY}/releases/download/v9/x.zip");
        assert!(bad.validate("macos-aarch64", "0.1.1").is_err());
    }

    #[test]
    fn rejects_an_unknown_schema() {
        let mut bad = manifest("0.2.0");
        bad.schema_version = 99;
        assert!(bad.validate("macos-aarch64", "0.1.1").is_err());
    }

    #[test]
    fn verifies_a_signature_from_a_trusted_key() {
        // Fixtures are a manifest and a detached minisign signature made by a test key.
        let manifest = include_bytes!("../../tests/fixtures/update-manifest.json");
        let signature = include_bytes!("../../tests/fixtures/update-manifest.json.minisig");
        let key = include_str!("../../tests/fixtures/update-test.pub");
        assert!(verify(manifest, signature, &[key]).is_ok());
        assert!(verify(manifest, signature, &["untrusted comment: nope\nAAAA"]).is_err());
        assert!(verify(b"{", signature, &[key]).is_err());
    }
}
