//! Installing a verified package.
//!
//! The UI stages the new files beside the installed app, writes a journal, and starts a
//! copy of this executable as a helper. The helper waits for this process to exit, swaps
//! the files, launches the new app, and keeps the old copy until the new one confirms it
//! started. A failed swap restores the old app.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use super::client::DownloadedUpdate;
use super::settings::ensure_dir;
use super::{CURRENT_VERSION, Error, Result, updates_dir};

const JOURNAL_SCHEMA: u32 = 1;
const JOURNAL_NAME: &str = "pending.json";
const CONFIRMED_NAME: &str = "pending.confirmed";
const MAX_EXTRACTED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 20_000;
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum InstallKind {
    /// A macOS `.app` directory.
    Bundle,
    /// A single executable on Windows or Linux.
    Executable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    schema: u32,
    transaction: String,
    kind: InstallKind,
    install: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    version: String,
    state: String,
    created: u64,
}

impl Journal {
    fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .map_err(|error| Error::new(format!("Couldn’t read the update journal: {error}")))?;
        let journal: Journal = serde_json::from_slice(&bytes)?;
        if journal.schema != JOURNAL_SCHEMA {
            return Err(Error::new("The update journal has an unexpected version."));
        }
        journal.validate()?;
        Ok(journal)
    }

    fn save(&mut self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::new("The update journal has no folder."))?;
        fs::create_dir_all(parent)?;
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        let _ = fs::remove_file(path);
        fs::rename(temp, path)?;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        let install_parent = self
            .install
            .parent()
            .ok_or_else(|| Error::new("The update target has no folder."))?;
        if self.staged.parent() != Some(install_parent)
            || self.backup.parent() != Some(install_parent)
        {
            return Err(Error::new(
                "The update journal points outside the install folder.",
            ));
        }
        if self.install.file_name().is_none() || self.staged.file_name().is_none() {
            return Err(Error::new("The update journal has an invalid target."));
        }
        Ok(())
    }
}

/// The prepared update, held by the UI until the user asks to restart.
pub struct PreparedUpdate {
    pub version: String,
    pub journal: PathBuf,
    pub lock_path: PathBuf,
    pub transaction: String,
    /// Kept open, and locked, until this process exits. The helper waits for it.
    pub lock: File,
}

/// Extract, validate, stage, and journal an update. Nothing installed is touched yet.
pub fn prepare(downloaded: &DownloadedUpdate) -> Result<PreparedUpdate> {
    let install = install_target()?;
    let dir = ensure_dir()?;
    prepare_at(&downloaded.path, &downloaded.version, &install, &dir)
}

fn prepare_at(archive: &Path, version: &str, install: &Path, dir: &Path) -> Result<PreparedUpdate> {
    let kind = kind_for(install);
    let parent = install
        .parent()
        .ok_or_else(|| Error::new("The app has no install folder."))?;
    ensure_writable(parent)?;
    reject_translocation(install)?;

    // Claim the transaction before unpacking or staging anything. A second attempt
    // while this process is alive must fail instead of racing on the same paths.
    let lock_path = dir.join("lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    if lock.try_lock().is_err() {
        return Err(Error::new(
            "Another update is already being installed; restart Calm Toad first.",
        ));
    }

    let transaction = transaction_id();
    let file_name = install
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new("The app has an unusual name."))?;
    let staged = parent.join(format!("{file_name}.staged-{transaction}"));
    let backup = parent.join(format!("{file_name}.backup-{transaction}"));
    let extract = tempfile::Builder::new()
        .prefix("calmtoad-extract-")
        .tempdir()
        .map_err(|error| Error::new(format!("Couldn’t make an update folder: {error}")))?;

    match kind {
        InstallKind::Bundle => extract_zip(archive, extract.path())?,
        InstallKind::Executable if cfg!(target_os = "windows") => {
            extract_zip(archive, extract.path())?
        }
        InstallKind::Executable => extract_tar_gz(archive, extract.path())?,
    }

    let payload = find_payload(extract.path(), kind, version)?;
    if staged.exists() {
        remove_path(&staged)?;
    }
    copy_payload(&payload, &staged, kind)?;
    validate_payload(&staged, kind)?;

    let mut journal = Journal {
        schema: JOURNAL_SCHEMA,
        transaction: transaction.clone(),
        kind,
        install: install.to_path_buf(),
        staged,
        backup,
        version: version.to_string(),
        state: "prepared".into(),
        created: now(),
    };

    let journal_path = dir.join(JOURNAL_NAME);
    // A previous journal without a backup is stale; do not let it describe this one.
    let _ = fs::remove_file(dir.join(CONFIRMED_NAME));
    journal.save(&journal_path)?;

    Ok(PreparedUpdate {
        version: version.to_string(),
        journal: journal_path,
        lock_path,
        transaction,
        lock,
    })
}

/// Copy this executable to a helper location and start it. It waits on the lock file.
pub fn start_helper(prepared: &PreparedUpdate) -> Result<()> {
    let exe = std::env::current_exe()?;
    let helper_dir = updates_dir().join(format!("helper-{}", prepared.transaction));
    fs::create_dir_all(&helper_dir)?;
    let helper = helper_dir.join(
        exe.file_name()
            .ok_or_else(|| Error::new("The app has an unusual executable name."))?,
    );
    fs::copy(&exe, &helper)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&helper)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper, permissions)?;
    }

    Command::new(&helper)
        .env("CALMTOAD_UPDATE_APPLY", &prepared.journal)
        .env("CALMTOAD_UPDATE_LOCK", &prepared.lock_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| Error::new(format!("Couldn’t start the update helper: {error}")))?;
    Ok(())
}

/// If this process was started as the helper, run the install and return its exit code.
/// Called before Rune starts, so the helper never opens a window.
pub fn apply_from_env() -> Option<i32> {
    let journal = std::env::var_os("CALMTOAD_UPDATE_APPLY")?;
    let lock = std::env::var_os("CALMTOAD_UPDATE_LOCK")?;
    let result = apply(PathBuf::from(journal), PathBuf::from(lock));
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Calm Toad update helper: {error}");
            1
        }
    })
}

/// Called once the new app has started. Writes a durable confirmation marker for the
/// helper. The helper owns removing the backup and journal, so this function never
/// deletes either: a failed marker write must leave the backup intact for rollback.
pub fn confirm_pending() {
    confirm_pending_at(&updates_dir(), CURRENT_VERSION, write_confirmation);
}

fn confirm_pending_at(
    dir: &Path,
    version: &str,
    write_marker: impl Fn(&Path, &str) -> Result<()>,
) -> bool {
    let journal_path = dir.join(JOURNAL_NAME);
    let Ok(bytes) = fs::read(&journal_path) else {
        return false;
    };
    let Ok(journal) = serde_json::from_slice::<Journal>(&bytes) else {
        return false;
    };
    if journal.schema != JOURNAL_SCHEMA
        || journal.version != version
        || !matches!(journal.state.as_str(), "installed" | "launched")
    {
        return false;
    }
    write_marker(dir, &journal.transaction).is_ok()
}

fn write_confirmation(dir: &Path, transaction: &str) -> Result<()> {
    let marker = dir.join(CONFIRMED_NAME);
    let temp = marker.with_extension("tmp");
    // A stale temp from an interrupted write must not be mistaken for this one.
    if fs::symlink_metadata(&temp).is_ok() {
        let _ = remove_path(&temp);
    }
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(transaction.as_bytes())?;
        file.sync_all()?;
    }
    let _ = fs::remove_file(&marker);
    fs::rename(&temp, &marker)?;
    // Persist the rename too where opening a directory is supported.
    #[cfg(unix)]
    if let Ok(parent) = File::open(dir) {
        let _ = parent.sync_all();
    }
    Ok(())
}

/// A confirmation counts only when its exact contents match this transaction.
fn has_confirmation(path: &Path, transaction: &str) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != transaction.len() as u64 {
        return false;
    }
    fs::read(path).is_ok_and(|bytes| bytes == transaction.as_bytes())
}

/// Remove helper directories left by old updates. Best effort; never fails startup.
pub fn cleanup_stale() {
    let dir = updates_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with("helper-") {
            let _ = remove_path(&entry.path());
        }
    }
}

fn apply(journal_path: PathBuf, lock_path: PathBuf) -> Result<()> {
    apply_with(journal_path, lock_path, launch, CONFIRM_TIMEOUT)
}

/// Install a prepared journal. `launcher` is injectable so failures are covered by
/// tests without starting a real app.
fn apply_with(
    journal_path: PathBuf,
    lock_path: PathBuf,
    launcher: impl Fn(&Path, InstallKind) -> Result<Option<Child>>,
    confirm_timeout: Duration,
) -> Result<()> {
    // Hold the lock for the whole transaction. Dropping it early would let a second
    // helper claim the same journal while the swap is still in progress.
    let _lock = wait_for_lock(&lock_path)?;
    let mut journal = Journal::load(&journal_path)?;
    if journal.state != "prepared" {
        return Err(Error::new("The update was not ready to install."));
    }
    journal.state = "installing".into();
    journal.save(&journal_path)?;

    if let Err(error) = swap_files(&journal.install, &journal.staged, &journal.backup) {
        journal.state = "rolled-back".into();
        let _ = journal.save(&journal_path);
        return Err(error);
    }

    journal.state = "installed".into();
    journal.save(&journal_path)?;

    let mut child = match launcher(&journal.install, journal.kind) {
        Ok(child) => child,
        Err(error) => {
            restore(&mut journal, &journal_path, &launcher)?;
            return Err(error);
        }
    };
    journal.state = "launched".into();
    journal.save(&journal_path)?;

    let confirmed = journal_path.with_file_name(CONFIRMED_NAME);
    let deadline = Instant::now() + confirm_timeout;
    loop {
        if has_confirmation(&confirmed, &journal.transaction) {
            // The helper owns cleanup after a valid confirmation.
            let _ = remove_path(&journal.backup);
            let _ = fs::remove_file(&journal_path);
            let _ = fs::remove_file(&confirmed);
            return Ok(());
        }
        if let Some(process) = child.as_mut()
            && let Some(status) = process.try_wait()?
        {
            if !status.success() {
                restore(&mut journal, &journal_path, &launcher)?;
                return Err(Error::new(
                    "The new app did not start. The previous version was restored.",
                ));
            }
            // `open` exits after telling Launch Services to open the app. Keep waiting
            // for the new app's confirmation instead of treating that as a crash.
            child = None;
        }
        if Instant::now() >= deadline {
            // Without a confirmation there is no proof the new version can start.
            // Put the old one back rather than leaving a broken install in place.
            restore(&mut journal, &journal_path, &launcher)?;
            return Err(Error::new(
                "The new app did not confirm that it started. The previous version was restored.",
            ));
        }
        sleep(Duration::from_millis(200));
    }
}

/// Move the installed app aside and put the staged app in its place. On failure, put
/// the old app back before returning.
fn swap_files(install: &Path, staged: &Path, backup: &Path) -> Result<()> {
    if backup.exists() {
        remove_path(backup)?;
    }
    fs::rename(install, backup).map_err(|error| {
        Error::new(format!(
            "Couldn’t move the old app aside: {error}. Nothing was changed."
        ))
    })?;
    if let Err(error) = fs::rename(staged, install) {
        let _ = fs::rename(backup, install);
        return Err(Error::new(format!(
            "Couldn’t put the new app in place: {error}. The old app was restored."
        )));
    }
    Ok(())
}

fn restore(
    journal: &mut Journal,
    journal_path: &Path,
    launcher: impl Fn(&Path, InstallKind) -> Result<Option<Child>>,
) -> Result<()> {
    // Never remove the new install unless the old one is actually available.
    if fs::symlink_metadata(&journal.backup).is_err() {
        journal.state = "rollback-failed".into();
        let _ = journal.save(journal_path);
        return Err(Error::new(
            "The previous version is missing, so the update could not be rolled back.",
        ));
    }
    if journal.install.exists() {
        remove_path(&journal.install)?;
    }
    fs::rename(&journal.backup, &journal.install)?;
    let _ = launcher(&journal.install, journal.kind);
    let _ = fs::remove_file(journal_path.with_file_name(CONFIRMED_NAME));
    journal.state = "rolled-back".into();
    let _ = journal.save(journal_path);
    Ok(())
}

fn wait_for_lock(lock_path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        if file.try_lock().is_ok() {
            // Return the open file: its lock lasts exactly as long as the guard.
            return Ok(file);
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "The app did not close, so the update was not installed.",
            ));
        }
        sleep(Duration::from_millis(100));
    }
}

fn launch(path: &Path, kind: InstallKind) -> Result<Option<Child>> {
    // The helper's own environment must not follow the new app: these variables make
    // a process act as the installer, and the app would exit before opening a window.
    let clear = |command: &mut Command| {
        command
            .env_remove("CALMTOAD_UPDATE_APPLY")
            .env_remove("CALMTOAD_UPDATE_LOCK");
    };
    match kind {
        InstallKind::Bundle => {
            let mut command = Command::new("/usr/bin/open");
            clear(&mut command);
            // `open` returns quickly after asking Launch Services to start the app.
            // Treat a failure here as a failed install instead of waiting for a
            // confirmation that can never arrive.
            let status = command
                .arg("-n")
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|error| Error::new(format!("Couldn’t open the new app: {error}")))?;
            if !status.success() {
                return Err(Error::new(
                    "macOS could not open the new app. The previous version was restored.",
                ));
            }
            Ok(None)
        }
        InstallKind::Executable => {
            let mut command = Command::new(path);
            clear(&mut command);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(Some)
                .map_err(|error| Error::new(format!("Couldn’t start the new app: {error}")))
        }
    }
}

fn install_target() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "macos")]
    {
        for ancestor in exe.ancestors() {
            if ancestor
                .extension()
                .is_some_and(|extension| extension == "app")
            {
                return Ok(ancestor.to_path_buf());
            }
        }
        Err(Error::new(
            "This copy of Calm Toad is not inside a .app bundle, so it cannot update itself.",
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = exe;
        Ok(std::env::current_exe()?)
    }
}

fn kind_for(install: &Path) -> InstallKind {
    if install
        .extension()
        .is_some_and(|extension| extension == "app")
    {
        InstallKind::Bundle
    } else {
        InstallKind::Executable
    }
}

fn reject_translocation(install: &Path) -> Result<()> {
    if install.to_string_lossy().contains("/AppTranslocation/") {
        return Err(Error::new(
            "macOS is running Calm Toad from a temporary location. Move it to Applications, then try again.",
        ));
    }
    Ok(())
}

fn ensure_writable(dir: &Path) -> Result<()> {
    tempfile::NamedTempFile::new_in(dir).map_err(|_| {
        Error::new(
            "Calm Toad cannot write to its install folder. Move it somewhere you can write, then try again.",
        )
    })?;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|error| Error::new(format!("The update package is not a valid zip: {error}")))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(Error::new("The update package has too many files."));
    }
    let mut total = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| Error::new(format!("The update package is damaged: {error}")))?;
        if entry.is_symlink() {
            return Err(Error::new(
                "The update package contains a link; it was rejected.",
            ));
        }
        let Some(relative) = entry.enclosed_name() else {
            return Err(Error::new("The update package contains an unsafe path."));
        };
        let out = dest.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > MAX_EXTRACTED_BYTES {
            return Err(Error::new("The update package expands to too much data."));
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut target = File::create(&out)?;
        io::copy(&mut entry, &mut target)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let file = File::open(archive)?;
    let decoder = GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    let mut total = 0u64;
    let mut count = 0usize;
    for entry in tar
        .entries()
        .map_err(|error| Error::new(format!("The update package is damaged: {error}")))?
    {
        let mut entry =
            entry.map_err(|error| Error::new(format!("The update package is damaged: {error}")))?;
        count += 1;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(Error::new("The update package has too many files."));
        }
        let path = entry
            .path()
            .map_err(|error| Error::new(format!("The update package has an unsafe path: {error}")))?
            .to_path_buf();
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(Error::new("The update package contains an unsafe path."));
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(Error::new(
                "The update package contains a link; it was rejected.",
            ));
        }
        total = total.saturating_add(entry.size());
        if total > MAX_EXTRACTED_BYTES {
            return Err(Error::new("The update package expands to too much data."));
        }
        entry
            .unpack_in(dest)
            .map_err(|error| Error::new(format!("Couldn’t unpack the update: {error}")))?;
    }
    Ok(())
}

fn find_payload(root: &Path, kind: InstallKind, version: &str) -> Result<PathBuf> {
    let mut found = Vec::new();
    walk(root, 0, &mut |path| {
        if kind == InstallKind::Bundle
            && path.is_dir()
            && path.extension().is_some_and(|extension| extension == "app")
        {
            found.push(path.to_path_buf());
        }
        if kind == InstallKind::Executable && path.is_file() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name == "Calm Toad.exe"
                || name == "calmtoad.exe"
                || name == "Calm Toad"
                || name == "calmtoad"
            {
                found.push(path.to_path_buf());
            }
        }
    });
    found
        .into_iter()
        .next()
        .ok_or_else(|| Error::new(format!("The {version} package did not contain Calm Toad.")))
}

fn walk(root: &Path, depth: usize, visit: &mut impl FnMut(&Path)) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        visit(&path);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            walk(&path, depth + 1, visit);
        }
    }
}

fn copy_payload(source: &Path, target: &Path, kind: InstallKind) -> Result<()> {
    match kind {
        InstallKind::Bundle => copy_tree(source, target),
        InstallKind::Executable => {
            fs::copy(source, target)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::metadata(source)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(target, permissions)?;
            }
            Ok(())
        }
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::new(
            "The update package contains a link; it was rejected.",
        ));
    }
    if metadata.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        fs::set_permissions(target, metadata.permissions())?;
    } else {
        fs::copy(source, target)?;
        fs::set_permissions(target, metadata.permissions())?;
    }
    Ok(())
}

fn validate_payload(path: &Path, kind: InstallKind) -> Result<()> {
    match kind {
        InstallKind::Bundle => {
            let executable = path.join("Contents/MacOS/calmtoad");
            if !executable.is_file() || !path.join("Contents/Info.plist").is_file() {
                return Err(Error::new(
                    "The update did not contain a complete Calm Toad app.",
                ));
            }
        }
        InstallKind::Executable => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
                return Err(Error::new(
                    "The update did not contain a usable Calm Toad app.",
                ));
            }
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn transaction_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{nanos}-{}", std::process::id())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_may_not_move_files_between_folders() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Calm Toad.app");
        let journal = Journal {
            schema: JOURNAL_SCHEMA,
            transaction: "t".into(),
            kind: InstallKind::Bundle,
            install: install.clone(),
            staged: dir.path().join("staged.app"),
            backup: dir.path().join("backup.app"),
            version: "0.2.0".into(),
            state: "prepared".into(),
            created: 0,
        };
        assert!(journal.validate().is_ok());

        let mut bad = journal.clone();
        bad.staged = PathBuf::from("/tmp/staged.app");
        assert!(bad.validate().is_err());

        let mut bad = journal;
        bad.backup = dir.path().join("elsewhere/backup.app");
        assert!(bad.validate().is_err());
    }

    #[test]
    fn a_bundle_is_found_inside_the_release_zip_layout() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Calm Toad/Calm Toad.app");
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::write(bundle.join("Contents/MacOS/calmtoad"), b"binary").unwrap();
        fs::write(bundle.join("Contents/Info.plist"), b"plist").unwrap();
        let found = find_payload(dir.path(), InstallKind::Bundle, "0.2.0").unwrap();
        assert_eq!(found, bundle);
        validate_payload(&found, InstallKind::Bundle).unwrap();
    }

    #[test]
    fn extraction_rejects_traversal() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("bad.zip");
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("../escape", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"no").unwrap();
        zip.finish().unwrap();

        let out = tempfile::tempdir().unwrap();
        assert!(extract_zip(&archive_path, out.path()).is_err());
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn swapping_puts_the_new_app_in_place_and_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Calm Toad");
        let staged = dir.path().join("Calm Toad.staged");
        let backup = dir.path().join("Calm Toad.backup");
        fs::write(&install, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        swap_files(&install, &staged, &backup).unwrap();
        assert_eq!(fs::read(&install).unwrap(), b"new");
        assert_eq!(fs::read(&backup).unwrap(), b"old");
        assert!(!staged.exists());
    }

    #[test]
    fn a_failed_swap_puts_the_old_app_back() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Calm Toad");
        let missing = dir.path().join("missing-staged");
        let backup = dir.path().join("Calm Toad.backup");
        fs::write(&install, b"old").unwrap();

        assert!(swap_files(&install, &missing, &backup).is_err());
        assert_eq!(fs::read(&install).unwrap(), b"old");
        assert!(!backup.exists());
    }

    #[test]
    fn extraction_rejects_a_tar_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("bad.tar.gz");
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        tar.append_link(&mut header, "link", "/etc/passwd").unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let out = tempfile::tempdir().unwrap();
        assert!(extract_tar_gz(&archive_path, out.path()).is_err());
        assert!(!out.path().join("link").exists());
    }

    #[test]
    fn extraction_rejects_a_tar_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("bad.tar.gz");
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        // Write the name through the raw header: `set_path` refuses `..` itself.
        header.as_old_mut().name[..9].copy_from_slice(b"../escape");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, &b"no"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let out = tempfile::tempdir().unwrap();
        assert!(extract_tar_gz(&archive_path, out.path()).is_err());
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn prepares_and_swaps_a_bundle_beside_the_install() {
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let apps = root.path().join("Applications");
        let data = root.path().join("data/updates");
        fs::create_dir_all(&apps).unwrap();
        fs::create_dir_all(&data).unwrap();

        let install = apps.join("Calm Toad.app");
        fs::create_dir_all(install.join("Contents/MacOS")).unwrap();
        fs::write(install.join("Contents/Info.plist"), b"old").unwrap();
        fs::write(install.join("Contents/MacOS/calmtoad"), b"old").unwrap();

        let archive = root.path().join("update.zip");
        let file = File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (path, contents) in [
            (
                "Calm Toad/Calm Toad.app/Contents/Info.plist",
                b"new".as_slice(),
            ),
            (
                "Calm Toad/Calm Toad.app/Contents/MacOS/calmtoad",
                b"newbinary".as_slice(),
            ),
        ] {
            zip.start_file(path, options).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap();

        let prepared = prepare_at(&archive, "0.2.0", &install, &data).unwrap();
        assert!(prepared.journal.exists());

        let journal = Journal::load(&prepared.journal).unwrap();
        assert_eq!(journal.state, "prepared");
        assert_eq!(journal.version, "0.2.0");
        assert!(journal.staged.exists());
        assert_eq!(
            fs::read(journal.staged.join("Contents/MacOS/calmtoad")).unwrap(),
            b"newbinary"
        );
        // Nothing installed was touched yet.
        assert_eq!(
            fs::read(install.join("Contents/MacOS/calmtoad")).unwrap(),
            b"old"
        );

        // A second preparation cannot take the lock while the first is alive.
        let second = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&prepared.lock_path)
            .unwrap();
        assert!(second.try_lock().is_err());

        // The same claim protects `prepare_at`: it takes the lock before extracting.
        let claimed = prepare_at(&archive, "0.2.0", &install, &data);
        assert!(claimed.is_err());

        swap_files(&journal.install, &journal.staged, &journal.backup).unwrap();
        assert_eq!(
            fs::read(journal.install.join("Contents/MacOS/calmtoad")).unwrap(),
            b"newbinary"
        );
        assert_eq!(
            fs::read(journal.backup.join("Contents/MacOS/calmtoad")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn a_failed_launch_restores_the_old_app() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Calm Toad");
        let staged = dir.path().join("Calm Toad.staged");
        let backup = dir.path().join("Calm Toad.backup");
        let journal_path = dir.path().join(JOURNAL_NAME);
        let lock_path = dir.path().join("lock");
        fs::write(&install, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        let mut journal = Journal {
            schema: JOURNAL_SCHEMA,
            transaction: "t".into(),
            kind: InstallKind::Executable,
            install: install.clone(),
            staged,
            backup: backup.clone(),
            version: "0.2.0".into(),
            state: "prepared".into(),
            created: 0,
        };
        journal.save(&journal_path).unwrap();

        let outcome = apply_with(
            journal_path.clone(),
            lock_path,
            |_, _| Err(Error::new("the launch failed")),
            Duration::from_secs(1),
        );

        assert!(outcome.is_err());
        assert_eq!(fs::read(&install).unwrap(), b"old");
        assert!(!backup.exists());
        assert_eq!(Journal::load(&journal_path).unwrap().state, "rolled-back");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_app_that_exits_with_failure_restores_the_old_app() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Calm Toad");
        let staged = dir.path().join("Calm Toad.staged");
        let backup = dir.path().join("Calm Toad.backup");
        let journal_path = dir.path().join(JOURNAL_NAME);
        let lock_path = dir.path().join("lock");
        fs::write(&install, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        let mut journal = Journal {
            schema: JOURNAL_SCHEMA,
            transaction: "t".into(),
            kind: InstallKind::Executable,
            install: install.clone(),
            staged,
            backup: backup.clone(),
            version: "0.2.0".into(),
            state: "prepared".into(),
            created: 0,
        };
        journal.save(&journal_path).unwrap();

        let outcome = apply_with(
            journal_path,
            lock_path,
            |_, _| {
                Command::new("/bin/false")
                    .spawn()
                    .map(Some)
                    .map_err(|error| Error::new(error.to_string()))
            },
            Duration::from_secs(2),
        );

        assert!(outcome.is_err());
        assert_eq!(fs::read(&install).unwrap(), b"old");
        assert!(!backup.exists());
    }

    #[test]
    fn a_new_app_that_never_confirms_restores_the_old_app() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Calm Toad");
        let staged = dir.path().join("Calm Toad.staged");
        let backup = dir.path().join("Calm Toad.backup");
        let journal_path = dir.path().join(JOURNAL_NAME);
        let lock_path = dir.path().join("lock");
        fs::write(&install, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        let mut journal = Journal {
            schema: JOURNAL_SCHEMA,
            transaction: "t".into(),
            kind: InstallKind::Executable,
            install: install.clone(),
            staged,
            backup: backup.clone(),
            version: "0.2.0".into(),
            state: "prepared".into(),
            created: 0,
        };
        journal.save(&journal_path).unwrap();

        let outcome = apply_with(
            journal_path.clone(),
            lock_path,
            |_, _| Ok(None),
            Duration::from_millis(300),
        );

        assert!(outcome.is_err());
        assert_eq!(fs::read(&install).unwrap(), b"old");
        assert!(!backup.exists());
        assert_eq!(Journal::load(&journal_path).unwrap().state, "rolled-back");
    }

    fn pending_files(
        dir: &Path,
        state: &str,
        transaction: &str,
    ) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let install = dir.join("Calm Toad");
        let staged = dir.join("Calm Toad.staged");
        let backup = dir.join("Calm Toad.backup");
        let journal_path = dir.join(JOURNAL_NAME);
        let lock_path = dir.join("lock");
        fs::write(&install, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();
        fs::write(&backup, b"old").unwrap();
        let mut journal = Journal {
            schema: JOURNAL_SCHEMA,
            transaction: transaction.into(),
            kind: InstallKind::Executable,
            install: install.clone(),
            staged,
            backup: backup.clone(),
            version: "0.2.0".into(),
            state: state.into(),
            created: 0,
        };
        journal.save(&journal_path).unwrap();
        (
            install,
            backup,
            journal_path,
            lock_path,
            dir.join(CONFIRMED_NAME),
        )
    }

    #[test]
    fn a_confirmed_transaction_writes_a_marker_and_leaves_cleanup_to_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let (_install, backup, journal_path, _lock, marker) =
            pending_files(dir.path(), "launched", "txn-confirmed");

        assert!(confirm_pending_at(dir.path(), "0.2.0", write_confirmation));
        assert_eq!(fs::read(&marker).unwrap(), b"txn-confirmed");
        // The new app must not delete the rollback copy; the helper does that only
        // after it reads a matching marker.
        assert!(backup.exists());
        assert!(journal_path.exists());
        assert!(!dir.path().join("pending.tmp").exists());
    }

    #[test]
    fn a_failed_confirmation_keeps_the_backup_and_journal() {
        let dir = tempfile::tempdir().unwrap();
        let (_install, backup, journal_path, _lock, marker) =
            pending_files(dir.path(), "launched", "txn-failed");

        let confirmed = confirm_pending_at(dir.path(), "0.2.0", |_, _| {
            Err(Error::new("the marker could not be written"))
        });

        assert!(!confirmed);
        assert!(!marker.exists());
        assert!(backup.exists());
        assert!(journal_path.exists());
    }

    #[test]
    fn a_matching_confirmation_finishes_the_update_and_removes_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (install, backup, journal_path, lock_path, marker) =
            pending_files(dir.path(), "prepared", "txn-ok");

        let outcome = apply_with(
            journal_path.clone(),
            lock_path,
            {
                let marker = marker.clone();
                move |_, _| {
                    fs::write(&marker, b"txn-ok").unwrap();
                    Ok(None)
                }
            },
            Duration::from_secs(1),
        );

        assert!(outcome.is_ok());
        assert_eq!(fs::read(&install).unwrap(), b"new");
        assert!(!backup.exists());
        assert!(!journal_path.exists());
        assert!(!marker.exists());
    }

    #[test]
    fn a_confirmation_for_another_transaction_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let (install, backup, journal_path, lock_path, marker) =
            pending_files(dir.path(), "prepared", "txn-current");

        let outcome = apply_with(
            journal_path.clone(),
            lock_path,
            {
                let marker = marker.clone();
                move |_, _| {
                    fs::write(&marker, b"txn-other").unwrap();
                    Ok(None)
                }
            },
            Duration::from_millis(300),
        );

        assert!(outcome.is_err());
        assert_eq!(fs::read(&install).unwrap(), b"old");
        assert!(!backup.exists());
        assert!(!marker.exists());
        assert_eq!(Journal::load(&journal_path).unwrap().state, "rolled-back");
    }

    #[test]
    fn a_failed_confirmation_still_rolls_back_without_losing_the_old_version() {
        let dir = tempfile::tempdir().unwrap();
        let (install, backup, journal_path, lock_path, marker) =
            pending_files(dir.path(), "prepared", "txn-write-fails");

        let outcome = apply_with(
            journal_path.clone(),
            lock_path,
            {
                let updates = dir.path().to_path_buf();
                move |_, _| {
                    // The marker cannot be written; the helper must not treat that
                    // as a confirmation or delete the rollback copy.
                    let confirmed = confirm_pending_at(&updates, "0.2.0", |_, _| {
                        Err(Error::new("the marker could not be written"))
                    });
                    assert!(!confirmed);
                    Ok(None)
                }
            },
            Duration::from_millis(300),
        );

        assert!(outcome.is_err());
        assert_eq!(fs::read(&install).unwrap(), b"old");
        assert!(!backup.exists());
        assert!(!marker.exists());
        assert_eq!(Journal::load(&journal_path).unwrap().state, "rolled-back");
    }

    #[test]
    fn restore_keeps_the_new_install_when_the_backup_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (install, backup, journal_path, _lock, _marker) =
            pending_files(dir.path(), "installed", "txn-missing-backup");
        fs::remove_file(&backup).unwrap();
        // Simulate the state after the swap: the new version is installed.
        fs::write(&install, b"new").unwrap();
        let mut journal = Journal::load(&journal_path).unwrap();

        let result = restore(&mut journal, &journal_path, |_, _| Ok(None));

        assert!(result.is_err());
        assert_eq!(fs::read(&install).unwrap(), b"new");
        assert_eq!(
            Journal::load(&journal_path).unwrap().state,
            "rollback-failed"
        );
    }

    #[test]
    fn transaction_ids_are_unique_enough() {
        let a = transaction_id();
        let b = transaction_id();
        assert_ne!(a, b);
        assert!(a.contains('-'));
    }
}
