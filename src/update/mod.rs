//! In-app updates: a signed release manifest, verified downloads, and a small helper
//! that replaces the installed app after it exits.
//!
//! The app never installs an unverified package. The manifest is signed with a key whose
//! public half is compiled into the binary; the package's SHA-256 is inside that signed
//! manifest. A compromised release token alone is not enough to ship an update.

pub mod client;
pub mod install;
pub mod manifest;
pub mod settings;

use std::fmt;

pub use client::{AvailableUpdate, DownloadedUpdate, check, download};
pub use settings::Settings;

/// The version compiled into this binary.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The public repository that publishes releases.
pub const REPOSITORY: &str = "abrudtkuhl/calmtoad";

/// Stable URL that always points at the newest published release's manifest.
pub const MANIFEST_URL: &str =
    "https://github.com/abrudtkuhl/calmtoad/releases/latest/download/latest.json";

/// The trusted signing key, committed at `assets/updater.pub`. Rotate by adding a second
/// key here in one release, then removing the old one in a later release.
pub const PUBLIC_KEY: &str = include_str!("../../assets/updater.pub");

/// Everything the updater fails with, in words that can be shown in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self(format!("Couldn’t read the update manifest: {value}"))
    }
}

impl From<url::ParseError> for Error {
    fn from(value: url::ParseError) -> Self {
        Self(format!("The update address is not valid: {value}"))
    }
}

impl From<semver::Error> for Error {
    fn from(value: semver::Error) -> Self {
        Self(format!("The update version is not valid: {value}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The platform key used in the release manifest.
pub fn platform_key() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-aarch64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}

/// Where settings, journals, and staged helpers live. Kept beside the platform's normal
/// application data so a read-only install location does not break the updater.
pub fn app_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("CALMTOAD_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join("Library/Application Support/Calm Toad");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return std::path::PathBuf::from(appdata).join("Calm Toad");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
            return std::path::PathBuf::from(config).join("calmtoad");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(".config/calmtoad");
        }
    }
    std::env::temp_dir().join("calmtoad")
}

/// The shared directory for the updater's small files.
pub fn updates_dir() -> std::path::PathBuf {
    app_dir().join("updates")
}

pub(crate) fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}
