//! Optional local-browser rendering for Browser mode.
//!
//! The renderer owns one headless Chrome/Chromium process per Browser crawl. Each page gets a
//! fresh browser context so cookies, cache and storage cannot leak between URL checks. The module
//! is deliberately small: it launches a browser, captures one main document at a time, and shuts
//! everything down. Crawl policy (scope, robots, scheduling and page budgets) stays in
//! [`crate::crawl`]; this module only enforces what must be enforced *before* a document request
//! leaves the browser.
//!
//! # Dependency decision
//!
//! `chromiumoxide` 0.9.1 is pinned for Chrome DevTools Protocol access. It was chosen over
//! `headless_chrome` because it exposes CDP domains directly — `Fetch` interception at request
//! and response stages, isolated `BrowserContext`s, `Target` events and `Page.stopLoading` — which
//! this module needs for scope/robots refusal, authoritative response metadata, per-page isolation
//! and prompt cancellation. It also builds a Rust-only stack (no Node or bundled Chromium) and
//! keeps the sandbox and TLS verification on; those defaults are not overridden. The cost is a
//! pinned tokio runtime and a larger dependency tree, both confined to this module.
//!
//! # Browser selection
//!
//! An explicit executable passed to [`discover_browser`] is used exactly as chosen; if it cannot
//! run, that is an error and there is no fallback. Without an explicit choice, the
//! `CALMTOAD_BROWSER_PATH` environment variable is tried first, then the usual per-platform
//! installations. Nothing is ever downloaded.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};

use chromiumoxide::Browser;
use chromiumoxide::BrowserConfig;
use chromiumoxide::cdp::browser_protocol::browser::{
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{
    EmulateNetworkConditionsByRuleParams, EnableParams as NetworkEnableParams, ErrorReason,
    EventDataReceived, EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent,
    EventResponseReceived, GetResponseBodyParams as NetworkGetResponseBodyParams, ResourceType,
    SetUserAgentOverrideParams,
};
use chromiumoxide::cdp::browser_protocol::page::{
    EventFrameNavigated, EventLifecycleEvent, EventNavigatedWithinDocument, StopLoadingParams,
};
use chromiumoxide::cdp::browser_protocol::target::{CreateTargetParams, EventTargetCreated};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::handler::HandlerConfig;
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use tokio::sync::Mutex;

use crate::crawl::USER_AGENT;

/// The first main-document request belongs to the requested URL. Later document requests
/// (script navigation, meta refresh) are recorded and handed back to the crawl queue instead of
/// being audited as this URL.
pub const NAVIGATION_HOP_LIMIT: usize = 5;

/// Fixed desktop viewport so rendered DOM output stays comparable between pages.
pub const VIEWPORT: (u32, u32) = (1365, 768);

/// Browser pages are heavy, so a Browser crawl never renders more than this at once.
pub const MAX_CONCURRENCY: usize = 2;

/// Decides whether the browser may request a document. The crawl owns this policy so scope
/// and robots rules are enforced before a navigation leaves the browser.
pub type UrlFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// How a crawl captures pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderMode {
    /// The fast HTTP fetch. No browser is needed.
    #[default]
    Html,
    /// A local headless browser that executes page scripts.
    Browser,
}

impl RenderMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Html => "HTML",
            Self::Browser => "Browser",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RenderConfig {
    /// An explicit Chrome/Chromium executable. `None` discovers a supported installation.
    pub browser_path: Option<PathBuf>,
    pub viewport: (u32, u32),
    /// Short pause after `DOMContentLoaded` for app hydration.
    pub settle: Duration,
    /// Hard per-page deadline covering navigation, settling and extraction.
    pub deadline: Duration,
    /// Guardrails on resource work, not a site page limit.
    pub max_requests: usize,
    pub max_transfer_bytes: u64,
    /// Same DOM evidence cap as HTML mode.
    pub max_body_bytes: usize,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            browser_path: None,
            viewport: VIEWPORT,
            settle: Duration::from_secs(1),
            deadline: Duration::from_secs(15),
            max_requests: 300,
            max_transfer_bytes: 50 * 1024 * 1024,
            max_body_bytes: 1_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderState {
    #[default]
    Captured,
    /// The per-page deadline expired; partial facts are retained.
    TimedOut,
    /// Startup, protocol, crash or navigation failure.
    Failed,
    /// The main response was a 3xx and the crawl queue owns the destination.
    Redirected,
    /// A script changed the document's path or query; the destination is queued separately.
    Navigated,
    /// The user stopped the crawl while this page was rendering.
    Cancelled,
    /// Scope, robots or interception refused the document request.
    Blocked,
    /// A resource guardrail stopped loading before the document was complete.
    Truncated,
}

impl RenderState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Captured => "Captured",
            Self::TimedOut => "Timed out",
            Self::Failed => "Render failed",
            Self::Redirected => "Redirect",
            Self::Navigated => "URL changed",
            Self::Cancelled => "Stopped",
            Self::Blocked => "Blocked",
            Self::Truncated => "Resource limited",
        }
    }
}

/// Whether the pre-script source HTML needed for staged evidence was captured.
///
/// This is deliberately separate from [`RenderState`]: a page can finish rendering while its
/// source body is missing, which means a script-removed noindex could have been missed. That
/// must remain an incomplete capture, not a clean result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceState {
    /// Not an HTML 200 response, so there is no source document to compare.
    #[default]
    NotApplicable,
    /// The full decoded body was captured within the evidence cap.
    Captured,
    /// The body exceeded the evidence cap before it was collected.
    TooLarge,
    /// The body was collected only in part.
    Truncated,
    /// The body was unavailable, failed, or did not finish before the capture ended.
    Unavailable,
}

impl SourceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotApplicable => "Not applicable",
            Self::Captured => "Captured",
            Self::TooLarge => "Exceeded the evidence cap",
            Self::Truncated => "Truncated at the evidence cap",
            Self::Unavailable => "Not captured",
        }
    }

    /// True when the source stage is fully known, or there is no source to know.
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Captured | Self::NotApplicable)
    }
}

/// One essential resource that did not load while the page was rendering.
///
/// A failed application script can leave the rendered DOM unhydrated, so its failure makes the
/// capture incomplete even though the document itself loaded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceFailure {
    pub url: String,
    pub resource_type: String,
    pub error: String,
}

/// Everything one browser job observed.
#[derive(Clone, Debug)]
pub struct Capture {
    pub state: RenderState,
    /// The actual main-document response, from the interception event.
    pub status: u16,
    pub content_type: String,
    pub location: String,
    pub headers: Vec<(String, String)>,
    /// The initial main-response HTML, captured before scripts ran.
    pub source_html: Option<String>,
    /// Whether the source stage is fully known; never inferred from prose.
    pub source_state: SourceState,
    pub source_note: String,
    /// The rendered main-document DOM after the readiness policy.
    pub rendered_html: Option<String>,
    pub rendered_truncated: bool,
    /// Same-origin scripts that failed to load or returned an HTTP error.
    pub resource_failures: Vec<ResourceFailure>,
    pub requested_url: String,
    pub observed_url: String,
    pub reason: String,
    pub elapsed_ms: u64,
    pub requests: usize,
    pub transfer_bytes: u64,
    pub navigations: usize,
    /// A later top-level document request that was refused instead of audited here.
    pub navigation_to: String,
    pub browser: String,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            state: RenderState::Captured,
            status: 0,
            content_type: String::new(),
            location: String::new(),
            headers: Vec::new(),
            source_html: None,
            source_state: SourceState::NotApplicable,
            source_note: String::new(),
            rendered_html: None,
            rendered_truncated: false,
            resource_failures: Vec::new(),
            requested_url: String::new(),
            observed_url: String::new(),
            reason: String::new(),
            elapsed_ms: 0,
            requests: 0,
            transfer_bytes: 0,
            navigations: 0,
            navigation_to: String::new(),
            browser: String::new(),
        }
    }
}

impl Capture {
    pub fn is_success(&self) -> bool {
        self.state == RenderState::Captured
    }
}

/// How long a freshly spawned browser may take to report its DevTools endpoint.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

/// A process handle shared by normal shutdown, failed startup and emergency app cleanup.
/// All signalling and reaping uses this same lock, so a collected child's PID is never used
/// to signal a later, unrelated process that reused the number.
struct OwnedBrowserProcess {
    child: tokio::process::Child,
    profile: PathBuf,
}

type OwnedProcess = Arc<StdMutex<OwnedBrowserProcess>>;

/// Browser roots currently owned by renderers in this process.
///
/// A normal window close and the final return from `main` call [`kill_owned_browsers`], so a
/// detached crawl worker cannot leave an owned browser behind when the app exits.
static LIVE_BROWSERS: LazyLock<StdMutex<HashMap<u32, OwnedProcess>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Kill every still-owned browser root and remove its application-owned profile.
pub fn kill_owned_browsers() {
    let owned = match LIVE_BROWSERS.lock() {
        Ok(mut owned) => std::mem::take(&mut *owned),
        Err(_) => return,
    };
    for process in owned.into_values() {
        stop_browser_process(&process);
        let profile = lock(&process).profile.clone();
        remove_profile(&profile);
    }
}

/// How many browser roots this process currently owns. Exposed for lifecycle tests.
#[doc(hidden)]
pub fn owned_browser_count() -> usize {
    LIVE_BROWSERS.lock().map(|owned| owned.len()).unwrap_or(0)
}

/// Remove an application-owned profile, tolerating the short window in which a killed browser
/// still writes to it.
fn remove_profile(profile: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match std::fs::remove_dir_all(profile) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return,
        }
    }
}

/// Stop and collect an owned child, returning whether it was still running. Holding its handle
/// across the entire operation also makes an emergency sweep racing normal shutdown safe.
fn stop_browser_process(process: &OwnedProcess) -> bool {
    let mut owned = lock(process);
    let child = &mut owned.child;
    match child.try_wait() {
        Ok(Some(_)) => return false,
        Ok(None) => {}
        // Do not fall back to a remembered PID when process ownership cannot be established.
        Err(_) => return false,
    }
    kill_child_tree(child);
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => break,
        }
    }
    true
}

fn kill_child_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    {
        // Windows keeps this process identity pinned while Child owns its process handle.
        // The caller has checked it is live and excludes every concurrent wait/reap.
        if let Some(pid) = child.id()
            && let Ok(mut taskkill) = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match taskkill.try_wait() {
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => {
                        let _ = taskkill.kill();
                        let _ = taskkill.try_wait();
                        break;
                    }
                    _ => break,
                }
            }
        }
    }
    // Unix uses the unreaped Child handle rather than a copied PID. On Windows this is the
    // handle-based fallback if taskkill could not finish the owned process tree.
    let _ = child.start_kill();
}

fn unregister_browser(process: &OwnedProcess, pid: Option<u32>) {
    if let Some(pid) = pid {
        let mut registry = lock(&LIVE_BROWSERS);
        unregister_process(&mut registry, pid, process);
    }
}

fn unregister_process(registry: &mut HashMap<u32, OwnedProcess>, pid: u32, process: &OwnedProcess) {
    // A newly launched browser may already have reused the old process's PID. Removing the
    // old registration must never remove the new owner's entry.
    if registry
        .get(&pid)
        .is_some_and(|entry| Arc::ptr_eq(entry, process))
    {
        registry.remove(&pid);
    }
}

#[derive(Clone, Debug)]
pub struct BrowserInfo {
    /// The browser executable used for the isolated process.
    pub path: PathBuf,
    /// The selected application bundle on macOS, when the choice came from one.
    pub application: Option<PathBuf>,
    /// A version label when known. Discovery deliberately does not wait on a version
    /// subprocess; capability is proven by the controlled CDP launch.
    pub version: String,
}

impl BrowserInfo {
    /// A short application name for the setup strip.
    pub fn label(&self) -> String {
        self.application
            .as_deref()
            .or(Some(self.path.as_path()))
            .and_then(Path::file_stem)
            .and_then(|name| name.to_str())
            .unwrap_or("Chrome or Chromium")
            .to_string()
    }
}

/// Find a usable local Chrome/Chromium. No download is attempted.
///
/// An explicitly chosen executable is used exactly as chosen: if it cannot run, that is
/// reported instead of silently falling back to a different installed browser.
pub fn discover_browser(explicit: Option<&Path>) -> Result<BrowserInfo, String> {
    if let Some(path) = explicit {
        return usable_browser(path).map_err(|reason| format!("{}: {reason}", path.display()));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(value) = std::env::var("CALMTOAD_BROWSER_PATH")
        && !value.trim().is_empty()
    {
        candidates.push(PathBuf::from(value));
    }
    candidates.extend(platform_candidates());

    for candidate in candidates {
        if let Ok(info) = usable_browser(&candidate) {
            return Ok(info);
        }
    }
    Err("No Chrome or Chromium installation was found. Choose a browser application.".into())
}

fn usable_browser(candidate: &Path) -> Result<BrowserInfo, String> {
    let Some(executable) = normalize_executable(candidate) else {
        return Err("that location is not a browser application or executable".into());
    };
    if !executable.is_file() {
        return Err("the browser executable is missing".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&executable)
            .map(|metadata| metadata.permissions().mode() & 0o111 == 0)
            .unwrap_or(true)
        {
            return Err("the browser executable is not runnable".into());
        }
    }
    Ok(BrowserInfo {
        path: executable,
        application: app_bundle_for(candidate),
        version: String::new(),
    })
}

/// The application bundle containing `path`, if any.
fn app_bundle_for(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

fn platform_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut paths = vec![
            PathBuf::from("/Applications/Google Chrome.app"),
            PathBuf::from("/Applications/Chromium.app"),
            PathBuf::from("/Applications/Google Chrome Canary.app"),
            PathBuf::from("/Applications/Microsoft Edge.app"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join("Applications/Google Chrome.app"));
            paths.push(home.join("Applications/Chromium.app"));
        }
        paths
    }
    #[cfg(target_os = "windows")]
    {
        let mut paths = Vec::new();
        let roots = [
            std::env::var_os("ProgramFiles"),
            std::env::var_os("ProgramFiles(x86)"),
            std::env::var_os("LOCALAPPDATA"),
        ];
        for root in roots.into_iter().flatten() {
            let root = PathBuf::from(root);
            paths.push(root.join("Google/Chrome/Application/chrome.exe"));
            paths.push(root.join("Chromium/Application/chrome.exe"));
            paths.push(root.join("Microsoft/Edge/Application/msedge.exe"));
        }
        paths
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut paths = Vec::new();
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
        ] {
            for dir in ["/usr/bin", "/usr/local/bin", "/snap/bin"] {
                paths.push(Path::new(dir).join(name));
            }
        }
        paths
    }
}

/// Accept an app bundle, an app directory or a plain executable.
fn normalize_executable(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if path.extension().is_some_and(|ext| ext == "app") || path.is_dir() {
        let macos = path.join("Contents/MacOS");
        if let Ok(entries) = std::fs::read_dir(&macos) {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// One browser process owned by one crawl.
pub struct BrowserRenderer {
    runtime: Option<tokio::runtime::Runtime>,
    browser: Arc<Mutex<Browser>>,
    semaphore: Arc<tokio::sync::Semaphore>,
    owned: Arc<StdMutex<HashSet<String>>>,
    info: BrowserInfo,
    config: RenderConfig,
    profile: Option<tempfile::TempDir>,
    /// Root browser process, when this renderer launched it.
    pid: Option<u32>,
    /// The spawned child shared with emergency cleanup; only its locked handle may be killed.
    process: OwnedProcess,
    monitor: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    closed: bool,
}

impl BrowserRenderer {
    /// Launch one owned browser for a crawl.
    ///
    /// The process is spawned and registered before the DevTools handshake, so closing the app
    /// or pressing Stop during startup can still find and kill it.
    pub fn launch(config: RenderConfig, cancel: Arc<AtomicBool>) -> Result<Self, String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("Stopped before the browser finished starting".into());
        }
        let info = discover_browser(config.browser_path.as_deref())?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Stopped before the browser finished starting".into());
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("calmtoad-render")
            .build()
            .map_err(|error| format!("could not start the render runtime: {error}"))?;

        // A separate, application-owned profile. The `TempDir` removes it on shutdown.
        let profile = tempfile::TempDir::new()
            .map_err(|error| format!("could not create a temporary browser profile: {error}"))?;
        let viewport = config.viewport;
        let browser_config = BrowserConfig::builder()
            .chrome_executable(&info.path)
            .user_data_dir(profile.path())
            .window_size(viewport.0, viewport.1)
            .viewport(Viewport {
                width: viewport.0,
                height: viewport.1,
                ..Viewport::default()
            })
            .new_headless_mode()
            .respect_https_errors()
            .disable_cache()
            .disable_default_args()
            // chromiumoxide adds the leading `--` itself. Prefixing here produces switches
            // such as `----deny-permission-prompts`, which Chrome silently ignores.
            .args([
                "no-first-run",
                "disable-background-networking",
                "disable-breakpad",
                "disable-client-side-phishing-detection",
                "disable-component-extensions-with-background-pages",
                "disable-dev-shm-usage",
                "disable-extensions",
                "disable-sync",
                "deny-permission-prompts",
                "disable-features=TranslateUI",
                "force-color-profile=srgb",
                "password-store=basic",
                "use-mock-keychain",
            ])
            .launch_timeout(LAUNCH_TIMEOUT)
            .build()
            .map_err(|error| format!("could not configure the browser: {error}"))?;

        // Spawn the process ourselves so its PID exists and is registered before the DevTools
        // handshake. `Browser::launch` would keep the PID private until after that wait, and a
        // close during the gap could miss the live browser. `tokio::process` needs a runtime
        // context even for the synchronous spawn.
        let child = {
            let _guard = runtime.enter();
            browser_config.launch()
        };
        let child = match child {
            Ok(child) => child,
            Err(error) => {
                runtime.shutdown_timeout(Duration::from_secs(2));
                return Err(format!("Could not start {}: {error}", info.path.display()));
            }
        };
        let mut child = child.into_inner();
        let stderr = child.stderr.take();
        let pid = child.id();
        let process = Arc::new(StdMutex::new(OwnedBrowserProcess {
            child,
            profile: profile.path().to_path_buf(),
        }));
        if let Some(pid) = pid
            && let Ok(mut owned) = LIVE_BROWSERS.lock()
        {
            owned.insert(pid, process.clone());
        }

        let launched = runtime.block_on(async {
            let stderr = stderr
                .ok_or_else(|| "the browser did not expose its startup output".to_string())?;
            let ws = wait_for_devtools(stderr, LAUNCH_TIMEOUT, &cancel).await?;
            let (browser, mut handler) = Browser::connect_with_config(
                ws,
                HandlerConfig {
                    // Match the previous launch policy: certificate errors are respected (not
                    // ignored), no reused HTTP cache, and the configured viewport.
                    ignore_https_errors: false,
                    ignore_invalid_messages: true,
                    viewport: Some(Viewport {
                        width: viewport.0,
                        height: viewport.1,
                        ..Viewport::default()
                    }),
                    cache_enabled: false,
                    ..HandlerConfig::default()
                },
            )
            .await
            .map_err(|error| format!("could not attach to the browser: {error}"))?;
            tokio::spawn(async move { while handler.next().await.is_some() {} });
            Ok::<_, String>(browser)
        });
        let browser = match launched {
            Ok(browser) => browser,
            Err(error) => {
                stop_failed_launch(runtime, &process, pid);
                return Err(error);
            }
        };
        let setup = runtime.block_on(async {
            tokio::select! {
                _ = wait_cancelled(&cancel) => {
                    return Err("Stopped before the browser finished starting".to_string());
                }
                _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    browser.execute(SetDownloadBehaviorParams {
                        behavior: SetDownloadBehaviorBehavior::Deny,
                        browser_context_id: None,
                        download_path: None,
                        events_enabled: None,
                    }),
                ) => {}
            }
            let version = tokio::select! {
                _ = wait_cancelled(&cancel) => {
                    return Err("Stopped before the browser finished starting".to_string());
                }
                version = tokio::time::timeout(Duration::from_secs(5), browser.version()) => version
                    .ok()
                    .and_then(Result::ok)
                    .map(|value| value.product)
                    .unwrap_or_else(|| info.version.clone()),
            };
            Ok::<_, String>(version)
        });
        let version = match setup {
            Ok(version) => version,
            Err(error) => {
                stop_failed_launch(runtime, &process, pid);
                return Err(error);
            }
        };
        let browser = Arc::new(Mutex::new(browser));
        let owned: Arc<StdMutex<HashSet<String>>> = Arc::new(StdMutex::new(HashSet::new()));
        let monitor = spawn_popup_monitor(&runtime, &browser, &owned);

        Ok(Self {
            runtime: Some(runtime),
            browser,
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENCY)),
            owned,
            info: BrowserInfo {
                path: info.path,
                application: info.application,
                version,
            },
            config,
            profile: Some(profile),
            pid,
            process,
            monitor: StdMutex::new(Some(monitor)),
            closed: false,
        })
    }

    pub fn info(&self) -> &BrowserInfo {
        &self.info
    }

    /// The application-owned profile directory, while the browser is running.
    pub fn profile_path(&self) -> Option<&Path> {
        self.profile.as_ref().map(tempfile::TempDir::path)
    }

    /// The owned browser root process. Exposed for lifecycle tests.
    #[doc(hidden)]
    pub fn browser_pid(&self) -> Option<u32> {
        self.pid
    }

    /// Render one URL. Runs off the UI thread and may be called from two crawl workers.
    pub fn render(&self, url: &str, allowed: UrlFilter, cancel: Arc<AtomicBool>) -> Capture {
        let started = Instant::now();
        if cancel.load(Ordering::Relaxed) {
            return stopped_capture(url, &self.info.version, "Stopped before rendering");
        }
        if !allowed(url) {
            return blocked_capture(url, &self.info.version);
        }

        let handle = match self.runtime.as_ref() {
            Some(runtime) => runtime.handle().clone(),
            None => {
                return stopped_capture(url, &self.info.version, "The renderer is closed");
            }
        };
        let result = handle.block_on(run_job(
            self.browser.clone(),
            self.owned.clone(),
            self.semaphore.clone(),
            self.config.clone(),
            self.info.version.clone(),
            url.to_string(),
            allowed,
            cancel,
        ));
        let mut capture = match result {
            Ok(capture) => capture,
            Err(error) => Capture {
                state: RenderState::Failed,
                requested_url: url.to_string(),
                observed_url: url.to_string(),
                reason: error,
                browser: self.info.version.clone(),
                ..Capture::default()
            },
        };
        capture.elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        capture
    }

    /// Close the browser and remove its profile. Called on normal completion and cancellation.
    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Ok(mut slot) = self.monitor.lock()
            && let Some(monitor) = slot.take()
        {
            monitor.abort();
        }
        let browser = self.browser.clone();
        let runtime = self.runtime.take();
        let Some(runtime) = runtime else {
            return;
        };
        runtime.block_on(async move {
            // Every shutdown step is bounded. A browser that ignores `close` is killed instead
            // of holding Stop or app close for the full launch timeout.
            if let Ok(mut guard) =
                tokio::time::timeout(Duration::from_secs(2), browser.lock()).await
            {
                let _ = tokio::time::timeout(Duration::from_secs(1), guard.close()).await;
            }
        });
        // This shared child is the only authority for killing/reaping the browser. In
        // particular, never signal the saved numeric PID after wait has collected the child.
        stop_browser_process(&self.process);
        unregister_browser(&self.process, self.pid.take());
        runtime.shutdown_timeout(Duration::from_secs(2));
        // Dropping the temp directory removes the application-owned profile.
        self.profile.take();
    }
}

impl Drop for BrowserRenderer {
    fn drop(&mut self) {
        // A panic or early return must not strand the owned process.
        self.shutdown();
    }
}

/// Kill a browser whose startup handshake failed or was cancelled, and collect it.
fn stop_failed_launch(runtime: tokio::runtime::Runtime, process: &OwnedProcess, pid: Option<u32>) {
    stop_browser_process(process);
    unregister_browser(process, pid);
    runtime.shutdown_timeout(Duration::from_secs(2));
}

/// Read Chrome's stderr until it prints its DevTools WebSocket, bounded by the launch timeout
/// and the crawl's cancellation flag.
async fn wait_for_devtools(
    stderr: tokio::process::ChildStderr,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<String, String> {
    use tokio::io::AsyncBufReadExt;

    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let timer = tokio::time::sleep(timeout);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            _ = &mut timer => {
                return Err(format!(
                    "did not report a debugging endpoint within {} seconds",
                    timeout.as_secs()
                ));
            }
            _ = wait_cancelled(cancel) => {
                return Err("Stopped before the browser finished starting".into());
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some((_, ws)) = line.rsplit_once("listening on ") {
                        let ws = ws.trim();
                        if ws.starts_with("ws") && ws.contains("devtools/browser") {
                            return Ok(ws.to_string());
                        }
                    }
                }
                Ok(None) => {
                    return Err("exited before reporting a debugging endpoint".into());
                }
                Err(error) => {
                    return Err(format!(
                        "could not read the browser's startup output: {error}"
                    ));
                }
            },
        }
    }
}

fn spawn_popup_monitor(
    runtime: &tokio::runtime::Runtime,
    browser: &Arc<Mutex<Browser>>,
    owned: &Arc<StdMutex<HashSet<String>>>,
) -> tokio::task::JoinHandle<()> {
    let browser = browser.clone();
    let owned = owned.clone();
    runtime.spawn(async move {
        let mut events = {
            let guard = browser.lock().await;
            match guard.event_listener::<EventTargetCreated>().await {
                Ok(events) => events,
                Err(_) => return,
            }
        };
        while let Some(event) = events.next().await {
            // Only windows a page opened are popups. Other pages (the browser's own
            // startup tab) belong to the browser and must stay untouched.
            if event.target_info.r#type != "page" || event.target_info.opener_id.is_none() {
                continue;
            }
            let id = event.target_info.target_id.inner().clone();
            // Page creation registers its target immediately after the command returns. Give a
            // new window a moment before deciding it is an unowned popup.
            tokio::time::sleep(Duration::from_millis(750)).await;
            if owned.lock().map(|set| set.contains(&id)).unwrap_or(true) {
                continue;
            }
            let guard = browser.lock().await;
            if let Ok(page) = guard.get_page(event.target_info.target_id.clone()).await {
                let _ = page.close().await;
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    browser: Arc<Mutex<Browser>>,
    owned: Arc<StdMutex<HashSet<String>>>,
    semaphore: Arc<tokio::sync::Semaphore>,
    config: RenderConfig,
    version: String,
    url: String,
    allowed: UrlFilter,
    cancel: Arc<AtomicBool>,
) -> Result<Capture, String> {
    // One absolute deadline covers queueing, setup, navigation, settling, source capture,
    // extraction and the bounded cleanup that follows. No phase gets its own fresh budget.
    let deadline = Instant::now() + config.deadline;

    let permit = {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let queued = tokio::select! {
            _ = wait_cancelled(&cancel) => {
                return Ok(stopped_capture(&url, &version, "Stopped before rendering"));
            }
            queued = tokio::time::timeout(remaining, semaphore.acquire()) => queued,
        };
        match queued {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err("render queue closed".into()),
            Err(_) => {
                return Ok(timed_out_capture(
                    &url,
                    &version,
                    "The per-page deadline expired while waiting for a browser slot",
                ));
            }
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return Ok(stopped_capture(&url, &version, "Stopped before rendering"));
    }

    let guard = match until_stopped(deadline, &cancel, browser.lock()).await {
        Ok(guard) => guard,
        Err(Interrupted::Cancelled) => {
            return Ok(stopped_capture(
                &url,
                &version,
                "Stopped before the browser was ready",
            ));
        }
        Err(Interrupted::Deadline) => {
            return Ok(timed_out_capture(
                &url,
                &version,
                "The per-page deadline expired before the browser was ready",
            ));
        }
    };

    let context_id = {
        match until_stopped(
            deadline,
            &cancel,
            guard.create_browser_context(Default::default()),
        )
        .await
        {
            Ok(Ok(context)) => context,
            Ok(Err(error)) => {
                return Err(format!(
                    "could not create an isolated browser context: {error}"
                ));
            }
            Err(Interrupted::Cancelled) => {
                return Ok(stopped_capture(
                    &url,
                    &version,
                    "Stopped before an isolated browser context was ready",
                ));
            }
            Err(Interrupted::Deadline) => {
                return Ok(timed_out_capture(
                    &url,
                    &version,
                    "The per-page deadline expired before an isolated browser context was ready",
                ));
            }
        }
    };
    {
        let setup = until_stopped(
            deadline,
            &cancel,
            guard.execute(SetDownloadBehaviorParams {
                behavior: SetDownloadBehaviorBehavior::Deny,
                browser_context_id: Some(context_id.clone()),
                download_path: None,
                events_enabled: None,
            }),
        )
        .await;
        match setup {
            Ok(_) => {}
            Err(Interrupted::Cancelled) => {
                return Ok(stopped_capture(
                    &url,
                    &version,
                    "Stopped before an isolated browser context was ready",
                ));
            }
            Err(Interrupted::Deadline) => {
                return Ok(timed_out_capture(
                    &url,
                    &version,
                    "The per-page deadline expired before the browser page was ready",
                ));
            }
        }
    }
    let page = {
        match until_stopped(
            deadline,
            &cancel,
            guard.new_page(CreateTargetParams {
                url: "about:blank".into(),
                browser_context_id: Some(context_id.clone()),
                ..Default::default()
            }),
        )
        .await
        {
            Ok(Ok(page)) => page,
            Ok(Err(error)) => {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    guard.dispose_browser_context(context_id.clone()),
                )
                .await;
                return Err(format!("could not open a browser page: {error}"));
            }
            Err(Interrupted::Cancelled) => {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    guard.dispose_browser_context(context_id.clone()),
                )
                .await;
                return Ok(stopped_capture(
                    &url,
                    &version,
                    "Stopped before a browser page was ready",
                ));
            }
            Err(Interrupted::Deadline) => {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    guard.dispose_browser_context(context_id.clone()),
                )
                .await;
                return Ok(timed_out_capture(
                    &url,
                    &version,
                    "The per-page deadline expired before a browser page was ready",
                ));
            }
        }
    };
    drop(guard);

    if let Ok(mut set) = owned.lock() {
        set.insert(page.target_id().inner().clone());
    }

    let result = capture_page(&page, &config, &url, allowed, cancel.clone(), deadline).await;

    if let Ok(mut set) = owned.lock() {
        set.remove(page.target_id().inner());
    }
    // Cleanup is time-bounded so a stuck page cannot hold a crawl worker, even when the page
    // deadline has already expired.
    let _ = tokio::time::timeout(Duration::from_secs(1), page.close()).await;
    if let Ok(guard) = tokio::time::timeout(Duration::from_secs(2), browser.lock()).await {
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            guard.dispose_browser_context(context_id),
        )
        .await;
    }
    drop(permit);
    let mut result = result;
    result.browser = version;
    Ok(result)
}

fn timed_out_capture(url: &str, version: &str, reason: &str) -> Capture {
    Capture {
        state: RenderState::TimedOut,
        requested_url: url.to_string(),
        observed_url: url.to_string(),
        reason: reason.to_string(),
        browser: version.to_string(),
        ..Capture::default()
    }
}

fn stopped_capture(url: &str, version: &str, reason: &str) -> Capture {
    Capture {
        state: RenderState::Cancelled,
        requested_url: url.to_string(),
        observed_url: url.to_string(),
        reason: reason.to_string(),
        browser: version.to_string(),
        ..Capture::default()
    }
}

fn blocked_capture(url: &str, version: &str) -> Capture {
    Capture {
        state: RenderState::Blocked,
        requested_url: url.to_string(),
        observed_url: url.to_string(),
        reason: "Outside the crawl scope or blocked by robots.txt".to_string(),
        browser: version.to_string(),
        ..Capture::default()
    }
}

#[derive(Default)]
struct Progress {
    status: u16,
    content_type: String,
    location: String,
    headers: Vec<(String, String)>,
    source_html: Option<String>,
    source_state: SourceState,
    source_note: String,
    http_seen: bool,
    observed_url: String,
    navigation_started: bool,
    dom_ready: bool,
    navigations: usize,
    navigation_to: String,
    blocked: bool,
    /// A script moved the document to a new path or query; this DOM is not this URL's.
    same_document_to: String,
    /// The Network request id of the first main-document response, used for source capture.
    source_request: String,
    source_decoded: u64,
    source_finished: bool,
    main_document_failed: String,
    /// Same-origin script URL/type/error, kept for the assessment and exports.
    resource_failures: Vec<ResourceFailure>,
    /// Script request id → URL, so a transport failure can name its resource.
    request_meta: HashMap<String, String>,
    requests: usize,
    transfer_bytes: u64,
    resource_limited: bool,
    /// Bytes already added to `transfer_bytes` per request from `dataReceived`, so
    /// `loadingFinished` can replace that request's streaming estimate with its total.
    seen_transfer: HashMap<String, u64>,
}

/// A page with more failing scripts than this is already incomplete; keep the evidence bounded.
const MAX_RESOURCE_FAILURES: usize = 5;

impl Progress {
    /// Record an essential (script) failure once, without unbounded growth.
    ///
    /// Scripts are treated the same whether they are local or on a CDN: a failed bundle can
    /// leave the page unhydrated either way. Other resource types are not content evidence.
    fn note_resource_failure(&mut self, url: &str, resource_type: &str, error: &str) {
        if resource_type != "Script" {
            return;
        }
        if self
            .resource_failures
            .iter()
            .any(|failure| failure.url == url && failure.error == error)
        {
            return;
        }
        if self.resource_failures.len() >= MAX_RESOURCE_FAILURES {
            return;
        }
        self.resource_failures.push(ResourceFailure {
            url: url.to_string(),
            resource_type: resource_type.to_string(),
            error: error.to_string(),
        });
    }
}

async fn capture_page(
    page: &Page,
    config: &RenderConfig,
    url: &str,
    allowed: UrlFilter,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
) -> Capture {
    let progress = Arc::new(StdMutex::new(Progress {
        observed_url: url.to_string(),
        ..Progress::default()
    }));

    let main_frame = match until_stopped(deadline, &cancel, page.mainframe()).await {
        Ok(Ok(Some(frame))) => frame,
        Ok(Ok(None)) => return failure_capture(url, "The browser did not expose a main frame"),
        Ok(Err(error)) => {
            return failure_capture(url, &format!("Could not inspect the page: {error}"));
        }
        Err(Interrupted::Cancelled) => {
            return stopped_capture(url, "", "Stopped before the page setup finished");
        }
        Err(Interrupted::Deadline) => {
            return timed_out_capture(url, "", "The per-page deadline expired during page setup");
        }
    };

    match until_stopped(
        deadline,
        &cancel,
        page.execute(SetUserAgentOverrideParams::new(USER_AGENT)),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            return failure_capture(url, &format!("Could not set the crawler identity: {error}"));
        }
        Err(Interrupted::Cancelled) => {
            return stopped_capture(url, "", "Stopped before the page setup finished");
        }
        Err(Interrupted::Deadline) => {
            return timed_out_capture(url, "", "The per-page deadline expired during page setup");
        }
    }
    // Network events give us streaming byte counts and a bounded response-body buffer. The
    // body is only requested after it finishes and only when its decoded size fits the
    // evidence cap, so an oversized compressed body is never decoded just to be cut.
    let buffer = i64::try_from(config.max_body_bytes.saturating_add(1)).unwrap_or(i64::MAX);
    match until_stopped(
        deadline,
        &cancel,
        page.execute(NetworkEnableParams {
            max_total_buffer_size: Some(buffer.saturating_mul(8)),
            max_resource_buffer_size: Some(buffer),
            ..Default::default()
        }),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            return failure_capture(
                url,
                &format!("Could not observe resource transfer: {error}"),
            );
        }
        Err(Interrupted::Cancelled) => {
            return stopped_capture(url, "", "Stopped before the page setup finished");
        }
        Err(Interrupted::Deadline) => {
            return timed_out_capture(url, "", "The per-page deadline expired during page setup");
        }
    }
    match until_stopped(
        deadline,
        &cancel,
        page.execute(FetchEnableParams {
            patterns: Some(vec![
                RequestPattern {
                    url_pattern: None,
                    resource_type: Some(ResourceType::Document),
                    request_stage: Some(RequestStage::Request),
                },
                RequestPattern {
                    url_pattern: None,
                    resource_type: Some(ResourceType::Document),
                    request_stage: Some(RequestStage::Response),
                },
            ]),
            handle_auth_requests: Some(false),
        }),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            return failure_capture(
                url,
                &format!("Could not enable navigation interception: {error}"),
            );
        }
        Err(Interrupted::Cancelled) => {
            return stopped_capture(url, "", "Stopped before the page setup finished");
        }
        Err(Interrupted::Deadline) => {
            return timed_out_capture(url, "", "The per-page deadline expired during page setup");
        }
    }

    let mut fetch_events = match until_stopped(
        deadline,
        &cancel,
        page.event_listener::<EventRequestPaused>(),
    )
    .await
    {
        Ok(Ok(events)) => events,
        Ok(Err(error)) => {
            return failure_capture(url, &format!("Could not observe navigation: {error}"));
        }
        Err(Interrupted::Cancelled) => {
            return stopped_capture(url, "", "Stopped before the page setup finished");
        }
        Err(Interrupted::Deadline) => {
            return timed_out_capture(url, "", "The per-page deadline expired during page setup");
        }
    };
    let responder = {
        let page = page.clone();
        let progress = progress.clone();
        let allowed = allowed.clone();
        let cancel = cancel.clone();
        let main_frame = main_frame.clone();
        tokio::spawn(async move {
            while let Some(event) = fetch_events.next().await {
                if cancel.load(Ordering::Relaxed) {
                    let _ = page
                        .execute(FailRequestParams::new(
                            event.request_id.clone(),
                            ErrorReason::Aborted,
                        ))
                        .await;
                    continue;
                }
                if event.frame_id != main_frame {
                    // Subframes render as part of the page, but are never audited as crawl pages.
                    if is_http_url(&event.request.url) {
                        let _ = page
                            .execute(ContinueRequestParams::new(event.request_id.clone()))
                            .await;
                    } else {
                        let _ = page
                            .execute(FailRequestParams::new(
                                event.request_id.clone(),
                                ErrorReason::BlockedByClient,
                            ))
                            .await;
                    }
                    continue;
                }

                if event.response_status_code.is_none() {
                    // Request stage: enforce scope and robots before anything leaves the browser.
                    let hop = {
                        let mut state = lock(&progress);
                        state.navigations += 1;
                        if state.navigations == 1 {
                            state.navigation_started = true;
                            state.observed_url = event.request.url.clone();
                        }
                        state.navigations
                    };
                    if !is_http_url(&event.request.url) || !allowed(&event.request.url) {
                        {
                            let mut state = lock(&progress);
                            state.navigation_to = event.request.url.clone();
                            state.blocked = true;
                        }
                        let _ = page
                            .execute(FailRequestParams::new(
                                event.request_id.clone(),
                                ErrorReason::BlockedByClient,
                            ))
                            .await;
                        continue;
                    }
                    if hop > 1 {
                        // A script, meta refresh or history navigation wants a new document.
                        // Record it for the crawl queue instead of auditing it as this URL.
                        // A repeat request for the same document URL is not a destination.
                        {
                            let mut state = lock(&progress);
                            if same_document_destination(&event.request.url, &state.observed_url)
                                .is_some()
                            {
                                state.navigation_to = event.request.url.clone();
                            }
                        }
                        let _ = page
                            .execute(FailRequestParams::new(
                                event.request_id.clone(),
                                ErrorReason::Aborted,
                            ))
                            .await;
                        continue;
                    }
                    let _ = page
                        .execute(ContinueRequestParams::new(event.request_id.clone()))
                        .await;
                } else {
                    // Response stage: only this event decides the page's status and headers.
                    let status =
                        event.response_status_code.unwrap_or_default().clamp(0, 999) as u16;
                    let headers: Vec<(String, String)> = event
                        .response_headers
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .map(|header| (header.name.clone(), header.value.clone()))
                        .collect();
                    let content_type = header_value(&headers, "content-type");
                    let location = header_value(&headers, "location");
                    let html = status == 200 && content_type.to_ascii_lowercase().contains("html");
                    {
                        let mut state = lock(&progress);
                        if !state.http_seen {
                            state.http_seen = true;
                            state.status = status;
                            state.content_type = content_type.clone();
                            state.location = location.clone();
                            state.headers = headers;
                            if let Some(network_id) = event.network_id.as_ref() {
                                state.source_request = network_id.inner().clone();
                            }
                            if html {
                                // The source is required evidence, not optional context. Until
                                // it is captured the page is incomplete, even if the rendered
                                // DOM looks clean.
                                state.source_state = SourceState::Unavailable;
                                state.source_note =
                                    "The pre-script source body was not captured".into();
                            } else {
                                state.source_state = SourceState::NotApplicable;
                            }
                        }
                    }
                    if (300..400).contains(&status) {
                        // Stop before Chrome follows the redirect. The crawl queues the
                        // location through its normal scope/robots/budget path.
                        let _ = page
                            .execute(FailRequestParams::new(
                                event.request_id.clone(),
                                ErrorReason::Aborted,
                            ))
                            .await;
                        continue;
                    }
                    // The body is captured after it finishes, from the Network domain, so a
                    // response without Content-Length is not skipped and a compressed body is
                    // only decoded when its decoded length fits the evidence cap.
                    let _ = page
                        .execute(ContinueRequestParams::new(event.request_id.clone()))
                        .await;
                }
            }
        })
    };

    macro_rules! listener {
        ($ty:ty, $label:expr) => {
            match until_stopped(deadline, &cancel, page.event_listener::<$ty>()).await {
                Ok(Ok(events)) => events,
                Ok(Err(_)) => {
                    responder.abort();
                    return failure_capture(url, $label);
                }
                Err(Interrupted::Cancelled) => {
                    responder.abort();
                    return stopped_capture(url, "", "Stopped before the page setup finished");
                }
                Err(Interrupted::Deadline) => {
                    responder.abort();
                    return timed_out_capture(
                        url,
                        "",
                        "The per-page deadline expired during page setup",
                    );
                }
            }
        };
    }

    let mut lifecycle = listener!(EventLifecycleEvent, "Could not observe document readiness");
    let mut navigated = listener!(EventFrameNavigated, "Could not observe page navigation");
    let mut within = listener!(
        EventNavigatedWithinDocument,
        "Could not observe URL changes"
    );
    let mut requests = listener!(
        EventRequestWillBeSent,
        "Could not observe resource requests"
    );
    let mut finished = listener!(EventLoadingFinished, "Could not observe resource transfer");
    let mut data = listener!(
        EventDataReceived,
        "Could not observe streamed resource data"
    );
    let mut failed = listener!(EventLoadingFailed, "Could not observe resource failures");
    let mut responses = listener!(
        EventResponseReceived,
        "Could not observe resource responses"
    );

    let watcher = {
        let progress = progress.clone();
        let main_frame = main_frame.clone();
        let page = page.clone();
        let cancel = cancel.clone();
        let requested = url.to_string();
        let max_requests = config.max_requests;
        let max_transfer = config.max_transfer_bytes;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    event = lifecycle.next() => {
                        let Some(event) = event else { break };
                        if event.frame_id == main_frame && event.name == "DOMContentLoaded" {
                            let mut state = lock(&progress);
                            if state.navigation_started {
                                state.dom_ready = true;
                            }
                        }
                    }
                    event = navigated.next() => {
                        let Some(event) = event else { break };
                        if event.frame.id == main_frame && is_http_url(&event.frame.url) {
                            lock(&progress).observed_url = event.frame.url.clone();
                        }
                    }
                    event = within.next() => {
                        let Some(event) = event else { break };
                        if event.frame_id == main_frame {
                            let destination = same_document_destination(&event.url, &requested);
                            let mut state = lock(&progress);
                            state.observed_url = event.url.clone();
                            if let Some(destination) = destination
                                && state.same_document_to.is_empty()
                            {
                                // The DOM now belongs to the destination. Stop treating it as
                                // the requested URL; the crawl queues the destination instead.
                                state.same_document_to = destination;
                            }
                        }
                    }
                    event = requests.next() => {
                        let Some(event) = event else { break };
                        if !is_http_url(&event.request.url) {
                            continue;
                        }
                        let over = {
                            let mut state = lock(&progress);
                            state.requests += 1;
                            if state.source_request.is_empty()
                                && event.r#type == Some(ResourceType::Document)
                                && event.frame_id.as_ref() == Some(&main_frame)
                            {
                                state.source_request = event.request_id.inner().clone();
                            }
                            // Keep the URL of script requests so a later loading failure can
                            // name the resource instead of only reporting an error code.
                            if event.r#type == Some(ResourceType::Script) {
                                state.request_meta.insert(
                                    event.request_id.inner().clone(),
                                    event.request.url.clone(),
                                );
                            }
                            state.requests > max_requests
                        };
                        if over {
                            limit_page(&progress, &page).await;
                        }
                    }
                    event = data.next() => {
                        let Some(event) = event else { break };
                        let id = event.request_id.inner().clone();
                        let encoded = event.encoded_data_length.max(0) as u64;
                        let decoded = event.data_length.max(0) as u64;
                        let over = {
                            let mut state = lock(&progress);
                            // This Chrome reports zero encoded bytes per chunk for an in-flight
                            // response, so fall back to the decoded size. That is conservative
                            // for compressed bodies but still stops a body that never finishes;
                            // `loadingFinished` corrects the total down to reported bytes.
                            let chunk = encoded.max(decoded);
                            let counted = state.seen_transfer.entry(id.clone()).or_default();
                            *counted = counted.saturating_add(chunk);
                            state.transfer_bytes = state.transfer_bytes.saturating_add(chunk);
                            if id == state.source_request {
                                state.source_decoded = state.source_decoded.saturating_add(decoded);
                            }
                            state.transfer_bytes > max_transfer
                        };
                        if over {
                            limit_page(&progress, &page).await;
                        }
                    }
                    event = finished.next() => {
                        let Some(event) = event else { break };
                        let id = event.request_id.inner().clone();
                        let over = {
                            let mut state = lock(&progress);
                            if id == state.source_request {
                                state.source_finished = true;
                            }
                            // `loadingFinished` reports the request total. Streaming may have
                            // counted decoded bytes as a fallback, so replace this request's
                            // estimate with the reported encoded total when there is one.
                            let seen = state.seen_transfer.remove(&id).unwrap_or(0);
                            let total = event.encoded_data_length.max(0.0) as u64;
                            let corrected = if total > 0 { total } else { seen };
                            state.transfer_bytes = state
                                .transfer_bytes
                                .saturating_sub(seen)
                                .saturating_add(corrected);
                            state.transfer_bytes > max_transfer
                        };
                        if over {
                            limit_page(&progress, &page).await;
                        }
                    }
                    event = failed.next() => {
                        let Some(event) = event else { break };
                        let id = event.request_id.inner().clone();
                        let mut state = lock(&progress);
                        if id == state.source_request && !state.source_finished {
                            state.main_document_failed = event.error_text.clone();
                        }
                        // Aborts we asked for (redirects, blocked destinations, Stop) are not
                        // evidence that the application failed to load.
                        let canceled = event.canceled.unwrap_or(false)
                            || event.blocked_reason.is_some();
                        if !canceled
                            && let Some(url) = state.request_meta.remove(&id)
                        {
                            state.note_resource_failure(&url, "Script", &event.error_text);
                        }
                    }
                    event = responses.next() => {
                        let Some(event) = event else { break };
                        if event.r#type == ResourceType::Script && event.response.status >= 400 {
                            let mut state = lock(&progress);
                            let status = event.response.status.max(0) as u16;
                            state.note_resource_failure(
                                &event.response.url,
                                "Script",
                                &format!("HTTP {status}"),
                            );
                        }
                    }
                    _ = wait_cancelled(&cancel) => break,
                }
            }
        })
    };

    // `Page.navigate` is tracked by the CDP driver until load completes, which would defeat the
    // per-page deadline on slow or hanging pages. Send it from a task and watch readiness from
    // the events instead; the command itself is submitted immediately and is not cancelled by
    // dropping the waiter.
    let navigation = {
        let page = page.clone();
        let url = url.to_string();
        tokio::spawn(async move {
            let _ = page.goto(url).await;
        })
    };
    let mut failure: Option<String> = None;
    let mut cancelled = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            failure = Some("Stopped by the user before the page finished rendering".into());
            break;
        }
        let (ready, blocked, redirected, limited, same_document) = {
            let state = lock(&progress);
            (
                state.dom_ready,
                state.blocked,
                state.http_seen && (300..400).contains(&state.status),
                state.resource_limited,
                !state.same_document_to.is_empty(),
            )
        };
        if blocked || redirected || ready || limited || same_document {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    navigation.abort();

    let (ready, blocked, redirected, limited, same_document, navigations, status, html) = {
        let state = lock(&progress);
        (
            state.dom_ready,
            state.blocked,
            state.http_seen && (300..400).contains(&state.status),
            state.resource_limited,
            !state.same_document_to.is_empty(),
            state.navigations,
            state.status,
            state.content_type.to_ascii_lowercase().contains("html"),
        )
    };

    // A main document that failed after its headers arrived but before it finished is a
    // failed render, not a clean partial page.
    if failure.is_none() && !blocked && !redirected && !limited && !cancelled {
        let failed = lock(&progress);
        if !failed.main_document_failed.is_empty() && !failed.source_finished {
            failure = Some(format!(
                "The main document failed to load: {}",
                failed.main_document_failed
            ));
        }
    }

    if !blocked && !redirected && !limited && !same_document && failure.is_none() {
        // A short settle for hydration, bounded by the same deadline and cancellation.
        let settle = config
            .settle
            .min(deadline.saturating_duration_since(Instant::now()));
        let settle_until = Instant::now() + settle;
        while Instant::now() < settle_until {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                failure = Some("Stopped by the user before the page finished rendering".into());
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // Settling can itself change the document identity, block a later navigation or trip a
    // guardrail. Source capture gives scripts another window, so identity is re-read after it
    // and again after extraction before any DOM is attributed to the requested URL.

    // Source evidence is needed even when the rendered DOM is not audited (a history change,
    // a timeout, or a truncated DOM). Capture it after the main document finishes, bounded by
    // the same deadline and decoded-byte cap.
    capture_source(page, &progress, deadline, &cancel, config.max_body_bytes).await;

    let (blocked, limited, same_document) = {
        let state = lock(&progress);
        (
            state.blocked,
            state.resource_limited,
            !state.same_document_to.is_empty(),
        )
    };
    let mut rendered = None;
    let mut rendered_truncated = false;
    let mut should_extract = failure.is_none()
        && !blocked
        && !redirected
        && !limited
        && !same_document
        && status == 200
        && html;
    if should_extract {
        match extract_dom(page, config.max_body_bytes, deadline, &cancel).await {
            Ok((dom, truncated)) => {
                rendered = Some(dom);
                rendered_truncated = truncated;
            }
            Err(reason) => {
                if cancel.load(Ordering::Relaxed) {
                    cancelled = true;
                }
                failure = Some(reason);
            }
        }
    }

    // A transition that landed while the DOM was being serialized belongs to the destination,
    // not this URL. Drop that DOM and its links/canonicals rather than attributing them here.
    let (blocked, limited, same_document) = {
        let state = lock(&progress);
        (
            state.blocked,
            state.resource_limited,
            !state.same_document_to.is_empty(),
        )
    };
    if same_document || blocked || limited {
        rendered = None;
        rendered_truncated = false;
        should_extract = false;
    }

    watcher.abort();
    responder.abort();
    if cancelled {
        // Drop whatever the page is still loading so close and context disposal cannot wait
        // on the network after the user stopped the crawl.
        let _ =
            tokio::time::timeout(Duration::from_secs(1), page.execute(StopLoadingParams {})).await;
    }

    let mut capture = {
        let state = lock(&progress);
        let timed_out = !state.resource_limited
            && Instant::now() >= deadline
            && (!state.dom_ready || !should_extract);
        let render_state = if state.blocked {
            RenderState::Blocked
        } else if cancelled {
            RenderState::Cancelled
        } else if redirected {
            RenderState::Redirected
        } else if !state.same_document_to.is_empty() {
            RenderState::Navigated
        } else if timed_out {
            RenderState::TimedOut
        } else if failure.is_some() {
            RenderState::Failed
        } else if state.resource_limited {
            RenderState::Truncated
        } else if state.http_seen {
            RenderState::Captured
        } else {
            RenderState::Failed
        };
        let navigation_to = if !state.same_document_to.is_empty() {
            state.same_document_to.clone()
        } else {
            state.navigation_to.clone()
        };
        let reason = if !state.same_document_to.is_empty() {
            format!(
                "A script changed the document URL to {}; the destination was queued instead of auditing that DOM as this URL",
                state.same_document_to
            )
        } else if state.resource_limited {
            "A resource limit stopped loading before the page was complete".into()
        } else if !state.navigation_to.is_empty() && !state.blocked {
            format!(
                "The page requested a later document navigation to {}; the destination was queued instead",
                state.navigation_to
            )
        } else if state.blocked {
            format!(
                "Navigation to {} was outside the crawl scope or blocked by robots.txt",
                state.navigation_to
            )
        } else if let Some(reason) = failure.clone() {
            reason
        } else if !state.http_seen {
            "The browser did not expose a main-document response".into()
        } else {
            String::new()
        };
        Capture {
            state: render_state,
            status: state.status,
            content_type: state.content_type.clone(),
            location: state.location.clone(),
            headers: state.headers.clone(),
            source_html: state.source_html.clone(),
            source_state: state.source_state,
            source_note: state.source_note.clone(),
            rendered_html: rendered.clone(),
            rendered_truncated,
            resource_failures: state.resource_failures.clone(),
            requested_url: url.to_string(),
            observed_url: state.observed_url.clone(),
            reason,
            requests: state.requests,
            transfer_bytes: state.transfer_bytes,
            navigations,
            navigation_to,
            browser: String::new(),
            elapsed_ms: 0,
        }
    };

    if capture.state == RenderState::Captured && !ready && should_extract {
        capture.state = RenderState::TimedOut;
        if capture.reason.is_empty() {
            capture.reason = "The page did not reach DOMContentLoaded before the deadline".into();
        }
    }
    if navigations > NAVIGATION_HOP_LIMIT {
        capture.state = RenderState::Failed;
        capture.reason = format!(
            "More than {NAVIGATION_HOP_LIMIT} document navigations were refused in one job"
        );
    }
    capture
}

async fn limit_page(progress: &Arc<StdMutex<Progress>>, page: &Page) {
    let already = {
        let mut state = lock(progress);
        if state.resource_limited {
            true
        } else {
            state.resource_limited = true;
            false
        }
    };
    if already {
        return;
    }
    let _ = page
        .execute(EmulateNetworkConditionsByRuleParams::new(true, Vec::new()))
        .await;
    let _ = page.execute(StopLoadingParams {}).await;
}

/// Capture the pre-script source body after the main document has finished, bounded by the
/// decoded-byte cap. `Network.getResponseBody` is only called when the streamed decoded size is
/// known to fit, so a compressed body cannot be expanded without a limit.
async fn capture_source(
    page: &Page,
    progress: &Arc<StdMutex<Progress>>,
    deadline: Instant,
    cancel: &AtomicBool,
    max_bytes: usize,
) {
    let (needed, request, mut decoded, mut finished) = {
        let state = lock(progress);
        (
            state.source_state == SourceState::Unavailable,
            state.source_request.clone(),
            state.source_decoded,
            state.source_finished,
        )
    };
    if !needed {
        return;
    }
    if cancel.load(Ordering::Relaxed) {
        let mut state = lock(progress);
        state.source_state = SourceState::Unavailable;
        state.source_note = "The source body was not captured because the crawl was stopped".into();
        return;
    }
    if request.is_empty() {
        let mut state = lock(progress);
        state.source_state = SourceState::Unavailable;
        state.source_note = "The browser did not expose a source request identifier".into();
        return;
    }
    if decoded > max_bytes as u64 {
        let mut state = lock(progress);
        state.source_state = SourceState::TooLarge;
        state.source_note = "Source body exceeded the evidence cap and was not captured".into();
        return;
    }
    let limited = lock(progress).resource_limited;
    if limited && !finished {
        let mut state = lock(progress);
        state.source_state = SourceState::TooLarge;
        state.source_note = "Source capture stopped at the transfer cap".into();
        return;
    }

    while !finished {
        if cancel.load(Ordering::Relaxed) {
            let mut state = lock(progress);
            state.source_state = SourceState::Unavailable;
            state.source_note =
                "The source body was not captured because the crawl was stopped".into();
            return;
        }
        let (limited, now_finished) = {
            let state = lock(progress);
            (state.resource_limited, state.source_finished)
        };
        if now_finished {
            break;
        }
        if limited {
            // The transfer cap already stopped loading; waiting for the body would only burn
            // the rest of the deadline. A source that finished before the cap still wins above.
            let mut state = lock(progress);
            state.source_state = SourceState::TooLarge;
            state.source_note = "Source capture stopped at the transfer cap".into();
            return;
        }
        if Instant::now() >= deadline {
            let mut state = lock(progress);
            state.source_state = SourceState::Unavailable;
            state.source_note =
                "The source body did not finish before the per-page deadline".into();
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        let state = lock(progress);
        finished = state.source_finished;
        decoded = state.source_decoded;
    }
    if decoded > max_bytes as u64 {
        let mut state = lock(progress);
        state.source_state = SourceState::TooLarge;
        state.source_note = "Source body exceeded the evidence cap and was not captured".into();
        return;
    }

    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        let mut state = lock(progress);
        state.source_state = SourceState::Unavailable;
        state.source_note = "The source body did not finish before the per-page deadline".into();
        return;
    }
    let result = tokio::select! {
        _ = wait_cancelled(cancel) => {
            let mut state = lock(progress);
            state.source_state = SourceState::Unavailable;
            state.source_note =
                "The source body was not captured because the crawl was stopped".into();
            return;
        }
        result = tokio::time::timeout(
            remaining,
            page.execute(NetworkGetResponseBodyParams::new(request)),
        ) => result,
    };
    match result {
        Ok(Ok(response)) => {
            let body = decode_body(&response.result.body, response.result.base64_encoded);
            if body.len() > max_bytes {
                let (body, _) = truncate_text(&body, max_bytes);
                let mut state = lock(progress);
                state.source_html = Some(body);
                state.source_state = SourceState::Truncated;
                state.source_note = "Source body truncated at the evidence cap".into();
            } else {
                let mut state = lock(progress);
                state.source_html = Some(body);
                state.source_state = SourceState::Captured;
                state.source_note = String::new();
            }
        }
        Ok(Err(error)) => {
            let mut state = lock(progress);
            state.source_state = SourceState::Unavailable;
            state.source_note = format!("Source body was not captured: {error}");
        }
        Err(_) => {
            let mut state = lock(progress);
            state.source_state = SourceState::Unavailable;
            state.source_note = "Source body was not captured before the per-page deadline".into();
        }
    }
}

/// A same-document path or query change means the current DOM is not the requested URL's.
/// Fragment-only changes keep the same document and are not queued.
fn same_document_destination(current: &str, requested: &str) -> Option<String> {
    if !is_http_url(current) {
        return None;
    }
    let current = url::Url::parse(current).ok()?;
    let requested = url::Url::parse(requested).ok()?;
    if current.path() == requested.path() && current.query() == requested.query() {
        return None;
    }
    Some(current.to_string())
}

async fn extract_dom(
    page: &Page,
    max_bytes: usize,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(String, bool), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Stopped by the user before the DOM could be captured".into());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("The per-page deadline expired before the DOM could be captured".into());
    }
    // Serialize the DOM node by node, stopping at the UTF-8 byte cap. `outerHTML` would build
    // the entire document string before slicing it, which the cap claim cannot cover.
    let script = r#"(() => {
  const limit = __LIMIT__;
  const enc = new TextEncoder();
  let used = 0;
  let truncated = false;
  const out = [];
  const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  const escAttr = (s) => esc(s).replace(/"/g, '&quot;');
  const take = (s) => {
    if (truncated) return;
    const room = limit - used;
    if (room <= 0) { if (s.length) truncated = true; return; }
    const bytes = enc.encode(s);
    if (bytes.length <= room) { out.push(s); used += bytes.length; return; }
    let lo = 0, hi = s.length;
    while (lo < hi) {
      const mid = Math.ceil((lo + hi) / 2);
      if (enc.encode(s.slice(0, mid)).length <= room) lo = mid; else hi = mid - 1;
    }
    if (lo > 0 && lo < s.length) {
      const last = s.charCodeAt(lo - 1);
      if (last >= 0xD800 && last <= 0xDBFF) lo -= 1;
    }
    if (lo > 0) { const part = s.slice(0, lo); out.push(part); used += enc.encode(part).length; }
    truncated = true;
  };
  const rawNames = { SCRIPT: 1, STYLE: 1, XMP: 1, IFRAME: 1, NOEMBED: 1, NOFRAMES: 1, PLAINTEXT: 1 };
  const stack = document.documentElement ? [{ n: document.documentElement }] : [];
  while (stack.length && !truncated) {
    const item = stack.pop();
    if (item.close) { take('</' + item.close + '>'); continue; }
    if (item.raw !== undefined) { take(item.raw); continue; }
    const node = item.n;
    const type = node.nodeType;
    if (type === 3) { take(esc(node.data)); continue; }
    if (type === 8) { take('<!--' + node.data + '-->'); continue; }
    if (type !== 1) continue;
    const name = node.localName || node.tagName.toLowerCase();
    let open = '<' + name;
    for (let i = 0; i < node.attributes.length; i++) {
      const a = node.attributes[i];
      open += ' ' + a.name + '="' + escAttr(a.value) + '"';
    }
    open += '>';
    take(open);
    if (truncated) break;
    stack.push({ close: name });
    if (rawNames[node.tagName] === 1) {
      const text = node.textContent || '';
      if (text.length) stack.push({ raw: text });
    } else {
      const children = node.childNodes;
      for (let i = children.length - 1; i >= 0; i--) stack.push({ n: children[i] });
    }
  }
  return [out.join(''), truncated];
})()"#
        .replace("__LIMIT__", &max_bytes.to_string());
    let mut params = EvaluateParams::new(script);
    params.return_by_value = Some(true);
    let evaluated = tokio::select! {
        _ = wait_cancelled(cancel) => {
            return Err("Stopped by the user before the DOM could be captured".into());
        }
        result = tokio::time::timeout(remaining, page.evaluate_expression(params)) => result,
    };
    let evaluated = evaluated
        .map_err(|_| "The DOM capture did not finish before the deadline".to_string())?
        .map_err(|error| format!("The DOM could not be captured: {error}"))?;
    let value: serde_json::Value = evaluated
        .into_value()
        .map_err(|error| format!("The DOM capture was not readable: {error}"))?;
    let Some(parts) = value.as_array() else {
        return Err("The DOM capture returned an unexpected value".into());
    };
    let html = parts
        .first()
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let truncated = parts
        .get(1)
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let (html, cut) = truncate_text(&html, max_bytes);
    Ok((html, truncated || cut))
}

fn failure_capture(url: &str, reason: &str) -> Capture {
    Capture {
        state: RenderState::Failed,
        requested_url: url.to_string(),
        observed_url: url.to_string(),
        reason: reason.to_string(),
        ..Capture::default()
    }
}

async fn wait_cancelled(cancel: &AtomicBool) {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Why a bounded setup phase stopped early.
enum Interrupted {
    Cancelled,
    Deadline,
}

/// Run one setup command under the absolute page deadline and the crawl's cancel flag.
async fn until_stopped<T>(
    deadline: Instant,
    cancel: &AtomicBool,
    future: impl std::future::Future<Output = T>,
) -> Result<T, Interrupted> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    tokio::select! {
        _ = wait_cancelled(cancel) => Err(Interrupted::Cancelled),
        result = tokio::time::timeout(remaining, future) => match result {
            Ok(value) => Ok(value),
            Err(_) => Err(Interrupted::Deadline),
        }
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn header_value(headers: &[(String, String)], name: &str) -> String {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

fn is_http_url(value: &str) -> bool {
    let value = value.trim_start().to_ascii_lowercase();
    value.starts_with("http://") || value.starts_with("https://")
}

fn truncate_text(value: &str, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value.to_string(), false);
    }
    let mut cut = limit;
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    (value[..cut].to_string(), true)
}

fn decode_body(body: &str, base64_encoded: bool) -> String {
    if !base64_encoded {
        return body.to_string();
    }
    use base64::Engine as _;
    match base64::engine::general_purpose::STANDARD.decode(body) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    // Run a separate copy of this test executable, without spawning an unowned shell or
    // descendants. The ordinary test invocation has no marker and returns immediately.
    #[test]
    fn owned_child_fixture() {
        if std::env::var("CALMTOAD_RENDER_LIFECYCLE_CHILD").as_deref() == Ok("sleep") {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    fn spawn_fixture(runtime: &tokio::runtime::Runtime, mode: &str) -> OwnedProcess {
        let _guard = runtime.enter();
        let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "render::lifecycle_tests::owned_child_fixture"])
            .env("CALMTOAD_RENDER_LIFECYCLE_CHILD", mode)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn owned fixture");
        Arc::new(StdMutex::new(OwnedBrowserProcess {
            child,
            profile: PathBuf::new(),
        }))
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("lifecycle runtime")
    }

    #[test]
    fn collected_process_is_not_signalled_by_a_stale_cleanup_owner() {
        let runtime = test_runtime();
        let process = spawn_fixture(&runtime, "exit");
        let stale_emergency_owner = process.clone();
        {
            let mut owned = lock(&process);
            runtime
                .block_on(owned.child.wait())
                .expect("collect exited child");
            assert_eq!(owned.child.id(), None);
        }
        assert!(
            !stop_browser_process(&stale_emergency_owner),
            "a registry snapshot must not signal a numeric PID after normal cleanup reaped it"
        );
    }

    #[test]
    fn concurrent_shutdown_and_emergency_cleanup_signal_and_collect_once() {
        let runtime = test_runtime();
        let process = spawn_fixture(&runtime, "sleep");
        let signals = std::thread::scope(|scope| {
            let normal = scope.spawn(|| stop_browser_process(&process));
            let emergency = scope.spawn(|| stop_browser_process(&process));
            usize::from(normal.join().expect("normal cleanup"))
                + usize::from(emergency.join().expect("emergency cleanup"))
        });
        assert_eq!(signals, 1, "both cleanup owners share one child state");
        assert_eq!(lock(&process).child.id(), None, "the child was collected");
        assert!(!stop_browser_process(&process), "cleanup is idempotent");
    }

    #[test]
    fn stale_registration_does_not_remove_a_new_owner_of_the_same_pid() {
        let runtime = test_runtime();
        let old = spawn_fixture(&runtime, "sleep");
        let newer = spawn_fixture(&runtime, "sleep");
        let pid = lock(&old).child.id().expect("old fixture PID");
        // Model PID reuse without forcing the OS to recycle a real process number.
        let mut registry = HashMap::from([(pid, newer.clone())]);
        unregister_process(&mut registry, pid, &old);
        assert!(Arc::ptr_eq(
            registry.get(&pid).expect("new owner retained"),
            &newer
        ));
        unregister_process(&mut registry, pid, &newer);
        assert!(registry.is_empty());
        stop_browser_process(&old);
        stop_browser_process(&newer);
    }
}
