//! A desktop SEO spider: crawl one host, then open the views people use first.
//!
//! Internal URLs, titles, meta descriptions, H1s, response codes, canonicals, image alt text,
//! internal links, and the URLs a site's sitemaps list. `cargo run` opens the window. `cargo run -- --snap shots/sample.png --scene sample --size 1180x740`
//! renders it headless.

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use calmtoad::crawl;
use calmtoad::crawl::{
    Audit, ContentCheck, Counts, CrawlOutcome, DEFAULT_MAX_PAGES, Limits, MAX_DEPTH, Page,
    auditable, canonical_issue, content_issue, counts_with, h1_issue, image_issue, links_issue,
    meta_issue, redirect_issue, response_issue, title_issue,
};
use calmtoad::indexability::{
    Assessment, CaptureInfo, CaptureMode, EvidenceStage, FindingCode, Label, ReadState,
    RenderSummary, Severity,
};
use calmtoad::render::{self, RenderMode, RenderState, SourceState};
use calmtoad::sitemap::{FileState, SitemapFile, SitemapKind, SitemapReport, SitemapUrl, UrlState};
use calmtoad::update;
use rune::tooling::AppSpec;
use rune::{
    Align, Color, Cx, El, FileDialogOptions, Icon, Key, KeyEvent, MenuItem, ScrollSpec, Shortcut,
    TextCase, TextEdit, TextStyle, Tx, Waker, col, el, icon, row, text,
};
use rune_kit::{
    Button, ButtonVariant, Cell, Checkbox, ControlSize, IconButton, IconButtonVariant, Kit,
    SegmentedControl, SortDir, Table, TableColumn, TableState, TableStyle,
};

mod theme;

static TOAD_MARK: Icon = Icon {
    name: "toad-mark",
    view: 16.0,
    body: r##"<path fill="#173821" d="M4 0h2v1h4V0h2v2h2v2h1v6h-1v2h-2v2h-2v1H6v-1H4v-2H2v-2H1V4h1V2h2z"/><path fill="#32c768" d="M4 2h2v2h4V2h2v2h2v6h-2v2h-2v1H6v-1H4v-2H2V4h2z"/><path fill="#a4f18c" d="M4 2h2v2H4zm6 0h2v2h-2zM3 5h2v1H3zM11 5h2v1h-2z"/><path fill="#f0f2ef" d="M4 2h2v3H4zm6 0h2v3h-2z"/><path fill="#102317" d="M5 3h1v2H5zm6 0h1v2h-1zM7 6h1v1H7zm2 0h1v1H9zM5 8h6v1h1v1h-2v1H6v-1H4V9h1z"/><path fill="#228c47" d="M3 9h2v1H3zm8 0h2v1h-2zM4 11h2v1H4zm6 0h2v1h-2z"/>"##,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Internal,
    Titles,
    Meta,
    H1,
    Response,
    Canonical,
    Images,
    Links,
    Sitemaps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SitemapView {
    Urls,
    Files,
}

/// What the results toolbar is asking to see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckFilter {
    All,
    Findings,
    Incomplete,
}

impl CheckFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Findings => "Findings",
            Self::Incomplete => "Incomplete",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::All => "Every URL in this view",
            Self::Findings => "URLs with a known finding or confirmed state in this view",
            Self::Incomplete => "URLs whose capture or check did not finish",
        }
    }
}

/// Search, filter, sort and selection for one tab or sitemap view.
struct ViewState {
    query: TextEdit,
    filter: CheckFilter,
    sort: (String, SortDir),
    /// The URL or sitemap-file URL the inspector is following, when still visible.
    selected: Option<String>,
}

impl ViewState {
    fn new(sort_column: &str) -> Self {
        Self {
            query: TextEdit::new(""),
            filter: CheckFilter::All,
            sort: (sort_column.to_string(), SortDir::Ascending),
            selected: None,
        }
    }
}

/// Concise field-adjacent setup errors, kept until the next valid start.
#[derive(Default)]
struct FieldErrors {
    url: String,
    pages: String,
    depth: String,
    setup: String,
}

impl FieldErrors {
    fn any(&self) -> bool {
        !self.url.is_empty()
            || !self.pages.is_empty()
            || !self.depth.is_empty()
            || !self.setup.is_empty()
    }

    fn summary(&self) -> String {
        [&self.url, &self.pages, &self.depth, &self.setup]
            .into_iter()
            .filter(|message| !message.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ")
    }

    fn clear(&mut self) {
        self.url.clear();
        self.pages.clear();
        self.depth.clear();
        self.setup.clear();
    }
}

/// The settings and seed one crawl actually used, kept apart from the next-run controls.
#[derive(Clone, Debug)]
struct RunContext {
    seed: String,
    host: String,
    limits: Limits,
    rendering: RenderMode,
    sitemaps: bool,
}

/// The workflow state of the current crawl.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RunPhase {
    #[default]
    Ready,
    Preparing,
    Crawling,
    Stopping,
    Completed,
    Stopped,
    Limited,
    Failed,
    Blocked,
}

impl RunPhase {
    fn active(self) -> bool {
        matches!(self, Self::Preparing | Self::Crawling | Self::Stopping)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Preparing => "Preparing",
            Self::Crawling => "Crawling",
            Self::Stopping => "Stopping",
            Self::Completed => "Completed",
            Self::Stopped => "Stopped",
            Self::Limited => "Page limit",
            Self::Failed => "Failed",
            Self::Blocked => "Blocked",
        }
    }
}

/// The zero-based window each inspector relation list is showing. Only one bounded window is
/// mounted at a time; these values never accumulate rows.
#[derive(Clone, Copy, Debug, Default)]
struct RelationPages {
    peers: usize,
    h1: usize,
    inbound: usize,
    outbound: usize,
}

const RELATION_PAGE: usize = 25;

fn relation_window(page: usize, total: usize) -> (usize, usize) {
    let last_page = total.saturating_sub(1) / RELATION_PAGE;
    let start = page.min(last_page).saturating_mul(RELATION_PAGE);
    let end = start.saturating_add(RELATION_PAGE).min(total);
    (start, end)
}

/// Retained rows are append-only during a crawl. Every replacement or evidence/audit update
/// bumps the revision, so cached indices can never refer to a previous dataset.
#[derive(Default)]
struct PagePositions {
    revision: u64,
    len: usize,
    by_url: HashMap<String, usize>,
}

#[derive(Default)]
struct InspectorRelations {
    key: Option<(u64, usize, Tab, String)>,
    peers: Vec<usize>,
    inbound: Vec<usize>,
    /// Indices into the selected page's links, deduplicated and stably broken-first.
    outbound: Vec<usize>,
    has_broken: bool,
    #[cfg(test)]
    builds: usize,
}

#[derive(Clone, Copy)]
enum RelationCopy {
    Peers(Tab),
    Incoming,
    Outgoing,
    H1,
}

fn invalidate_inspector(state: &mut State) {
    state.pages_revision = state.pages_revision.wrapping_add(1);
    state.page_positions.get_mut().by_url.clear();
    state.inspector_relations.get_mut().key = None;
}

fn reset_inspector_dataset(state: &mut State) {
    invalidate_inspector(state);
    state.relation_pages = RelationPages::default();
    state.relation_identity = None;
    state.selection_epoch = state.selection_epoch.wrapping_add(1);
}

fn page_positions(state: &State) -> Ref<'_, HashMap<String, usize>> {
    {
        let mut cache = state.page_positions.borrow_mut();
        if cache.revision != state.pages_revision || cache.len != state.pages.len() {
            cache.by_url.clear();
            for (index, page) in state.pages.iter().enumerate() {
                // Preserve the old first-match URL lookup even for repeated input rows.
                cache.by_url.entry(page.url.clone()).or_insert(index);
            }
            cache.revision = state.pages_revision;
            cache.len = state.pages.len();
        }
    }
    Ref::map(state.page_positions.borrow(), |cache| &cache.by_url)
}

fn inspector_relations<'a>(state: &'a State, page: &Page, tab: Tab) -> Ref<'a, InspectorRelations> {
    let matches = state.inspector_relations.borrow().key.as_ref().is_some_and(
        |(revision, len, cached_tab, url)| {
            *revision == state.pages_revision
                && *len == state.pages.len()
                && *cached_tab == tab
                && *url == page.url
        },
    );
    if !matches {
        let positions = page_positions(state);
        let peers = match tab {
            Tab::Titles => state.audit.title_duplicate_peers(&state.pages, page),
            Tab::Meta => state.audit.meta_duplicate_peers(&state.pages, page),
            Tab::H1 => state.audit.h1_duplicate_peers(&state.pages, page),
            _ => Vec::new(),
        }
        .into_iter()
        .filter_map(|peer| positions.get(&peer.url).copied())
        .collect();
        let mut inbound = Vec::new();
        let mut outbound = Vec::new();
        let mut has_broken = false;
        if tab == Tab::Links {
            inbound = state
                .audit
                .inbound_sources(&state.pages, page)
                .into_iter()
                .filter_map(|source| positions.get(&source.url).copied())
                .collect();
            let mut seen = HashSet::new();
            outbound = page
                .links
                .iter()
                .enumerate()
                .filter_map(|(index, link)| seen.insert(link.as_str()).then_some(index))
                .collect();
            let broken = |index: &usize| {
                positions
                    .get(&page.links[*index])
                    .and_then(|index| state.pages.get(*index))
                    .is_some_and(|page| page.status == 0 || page.status >= 400)
            };
            has_broken = outbound.iter().any(broken);
            outbound.sort_by_key(|index| !broken(index));
        }
        let mut cache = state.inspector_relations.borrow_mut();
        cache.key = Some((
            state.pages_revision,
            state.pages.len(),
            tab,
            page.url.clone(),
        ));
        cache.peers = peers;
        cache.inbound = inbound;
        cache.outbound = outbound;
        cache.has_broken = has_broken;
        #[cfg(test)]
        {
            cache.builds += 1;
        }
    }
    state.inspector_relations.borrow()
}

/// Full relation text is only materialized when Copy is pressed, never while painting a row.
fn relation_copy(state: &State, source_url: &str, kind: RelationCopy) -> String {
    let Some(page) = page_by_url(state, source_url) else {
        return String::new();
    };
    if matches!(kind, RelationCopy::H1) {
        return page.h1s.join("\n");
    }
    let tab = match kind {
        RelationCopy::Peers(tab) => tab,
        _ => Tab::Links,
    };
    let relations = inspector_relations(state, page, tab);
    match kind {
        RelationCopy::Peers(_) => relations
            .peers
            .iter()
            .map(|index| state.pages[*index].url.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        RelationCopy::Incoming => relations
            .inbound
            .iter()
            .map(|index| state.pages[*index].url.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        RelationCopy::Outgoing => relations
            .outbound
            .iter()
            .map(|index| page.links[*index].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        RelationCopy::H1 => unreachable!(),
    }
}

fn sync_relation_selection(state: &mut State, selected: Option<&str>) {
    let slot = view_slot(state.tab, state.sitemap_view);
    let changed = match (&state.relation_identity, selected) {
        (Some((previous_slot, previous)), Some(selected)) => {
            *previous_slot != slot || previous != selected
        }
        (None, None) => false,
        _ => true,
    };
    if changed {
        state.relation_pages = RelationPages::default();
        state.selection_epoch = state.selection_epoch.wrapping_add(1);
        state.relation_identity = selected.map(|url| (slot, url.to_string()));
    }
}

/// Counts for the active view, independent of the search query.
#[derive(Clone, Copy, Debug, Default)]
struct ViewCounts {
    total: usize,
    findings: usize,
    incomplete: usize,
}

static OPEN_LINK: Icon = Icon {
    name: "open-link",
    view: 16.0,
    body: r#"<path d="M9 3h4v4M13 3L7 9M12 9v4H3V4h4" fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
static DETAILS_PANEL: Icon = Icon {
    name: "details-panel",
    view: 16.0,
    body: r#"<rect x="1.5" y="2" width="13" height="12" rx="1" fill="none" stroke="currentColor"/><path d="M10 2v12" stroke="currentColor"/>"#,
};

enum CrawlEvent {
    Page(Box<Page>),
    SitemapFile(SitemapFile),
    SitemapUrl(SitemapUrl),
    SitemapPartial(String),
    SitemapDone,
    Done(String, Box<SitemapReport>, CrawlOutcome),
}

struct CrawlJob {
    rx: mpsc::Receiver<CrawlEvent>,
    cancel: Arc<AtomicBool>,
    /// A Stop has been requested; the worker may still be finishing.
    stopping: bool,
}

/// What the update strip is showing.
enum UpdateUi {
    Idle,
    Checking,
    Available(update::AvailableUpdate),
    Downloading {
        version: String,
        received: u64,
        total: Option<u64>,
    },
    Ready(update::DownloadedUpdate),
    Installing,
    Failed(String),
    UpToDate,
}

struct UpdateJob {
    rx: mpsc::Receiver<UpdateEvent>,
    cancel: Arc<AtomicBool>,
    /// A person asked for this check, so its errors deserve a banner.
    manual: bool,
}

enum UpdateEvent {
    Checked(Result<Option<update::AvailableUpdate>, String>),
    Progress { received: u64, total: Option<u64> },
    Downloaded(Result<update::DownloadedUpdate, String>),
}

/// What this machine can offer Browser mode right now.
enum BrowserStatus {
    /// Launching `--version` takes long enough that startup must not wait on it.
    Checking,
    Ready {
        path: PathBuf,
        version: String,
    },
    Unavailable {
        reason: String,
    },
}

impl BrowserStatus {
    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

struct BrowserChoice {
    /// A person's explicit executable choice, kept even when it cannot run so the reason stays
    /// attached to the file they picked.
    path: Option<PathBuf>,
    status: BrowserStatus,
}

fn detect_browser(path: Option<PathBuf>) -> BrowserChoice {
    let status = match render::discover_browser(path.as_deref()) {
        Ok(info) => BrowserStatus::Ready {
            path: info.path,
            version: info.version,
        },
        Err(reason) => BrowserStatus::Unavailable { reason },
    };
    BrowserChoice { path, status }
}

/// Detect a browser off the UI thread. Startup stays responsive while a local Chrome or
/// Chromium answers `--version`.
fn probe_browser(path: Option<PathBuf>) -> mpsc::Receiver<BrowserChoice> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(detect_browser(path));
    });
    rx
}

struct State {
    url: TextEdit,
    rendering: RenderMode,
    browser: BrowserChoice,
    browser_probe: Option<mpsc::Receiver<BrowserChoice>>,
    pages_limit: TextEdit,
    depth_limit: TextEdit,
    use_sitemaps: bool,
    tab: Tab,
    /// One search/filter/sort/selection state per tab and per sitemap view.
    views: Vec<ViewState>,
    details_open: bool,
    evidence_open: bool,
    capture_open: bool,
    run_details_open: bool,
    relation_pages: RelationPages,
    /// Bumped whenever the inspected row or tab changes, so a new inspector starts at the top
    /// instead of inheriting another result's scroll position.
    selection_epoch: u64,
    relation_identity: Option<(usize, String)>,
    pages_revision: u64,
    page_positions: RefCell<PagePositions>,
    inspector_relations: RefCell<InspectorRelations>,
    table: TableState,
    pages: Vec<Page>,
    /// Lowercased URL/title/meta/H1/canonical/content-type text, parallel to `pages`.
    search_index: Vec<String>,
    /// Indices into `pages` after the current tab's query and check filter, in display order.
    visible: Vec<usize>,
    audit: Audit,
    counts: Counts,
    counts_dirty: bool,
    counts_deferred: bool,
    counts_at: Option<Instant>,
    view_counts: ViewCounts,
    sitemap: SitemapReport,
    sitemap_view: SitemapView,
    sitemap_visible: Vec<usize>,
    sitemap_file_visible: Vec<usize>,
    sitemap_discovering: bool,
    message: String,
    phase: RunPhase,
    /// The seed, limits and options the current results came from.
    run: Option<RunContext>,
    /// The terminal reason of the current results, kept apart from next-run validation notices.
    run_outcome: Option<String>,
    field_errors: FieldErrors,
    crawl_more: bool,
    crawl_started: Option<Instant>,
    crawl_elapsed: Option<Duration>,
    export_notice: Option<Result<String, String>>,
    /// A short notice from search, selection or clipboard actions.
    action_notice: Option<String>,
    max_pages: usize,
    job: Option<CrawlJob>,
    waker: Option<Waker>,
    update: UpdateUi,
    update_job: Option<UpdateJob>,
    settings: update::Settings,
    next_update_check: Option<Instant>,
    /// The first frame has been laid out, so a pending update can keep its new copy.
    update_confirmed: bool,
}

fn init() -> State {
    let mut table = TableState::new().key("grid");
    table.set_sort("indexability", SortDir::Ascending);
    State {
        url: TextEdit::new("https://example.com/"),
        rendering: RenderMode::Html,
        browser: BrowserChoice {
            path: None,
            status: BrowserStatus::Checking,
        },
        browser_probe: Some(probe_browser(None)),
        pages_limit: TextEdit::new(DEFAULT_MAX_PAGES.to_string()),
        depth_limit: TextEdit::new(MAX_DEPTH.to_string()),
        use_sitemaps: true,
        tab: Tab::Internal,
        views: vec![
            ViewState::new("indexability"),
            ViewState::new("issue"),
            ViewState::new("issue"),
            ViewState::new("issue"),
            ViewState::new("issue"),
            ViewState::new("issue"),
            ViewState::new("issue"),
            ViewState::new("issue"),
            ViewState::new("issue"),
            ViewState::new("issue"),
        ],
        details_open: false,
        evidence_open: false,
        capture_open: false,
        run_details_open: false,
        relation_pages: RelationPages::default(),
        selection_epoch: 0,
        relation_identity: None,
        pages_revision: 1,
        page_positions: RefCell::new(PagePositions::default()),
        inspector_relations: RefCell::new(InspectorRelations::default()),
        table,
        pages: Vec::new(),
        search_index: Vec::new(),
        visible: Vec::new(),
        audit: Audit::default(),
        counts: Counts::default(),
        counts_dirty: false,
        counts_deferred: false,
        counts_at: None,
        view_counts: ViewCounts::default(),
        sitemap: SitemapReport::default(),
        sitemap_view: SitemapView::Urls,
        sitemap_visible: Vec::new(),
        sitemap_file_visible: Vec::new(),
        sitemap_discovering: false,
        message: "Ready · enter a site URL to begin. Up to 40 pages and 3 levels deep.".into(),
        phase: RunPhase::Ready,
        run: None,
        run_outcome: None,
        field_errors: FieldErrors::default(),
        crawl_more: false,
        crawl_started: None,
        crawl_elapsed: None,
        export_notice: None,
        action_notice: None,
        max_pages: DEFAULT_MAX_PAGES,
        job: None,
        waker: None,
        update: UpdateUi::Idle,
        update_job: None,
        settings: update::Settings::load(),
        next_update_check: None,
        update_confirmed: false,
    }
}

fn setup(state: &mut State, waker: Waker) {
    state.waker = Some(waker);
    update::install::cleanup_stale();
}

fn view_slot(tab: Tab, sitemap_view: SitemapView) -> usize {
    match tab {
        Tab::Internal => 0,
        Tab::Titles => 1,
        Tab::Meta => 2,
        Tab::H1 => 3,
        Tab::Response => 4,
        Tab::Canonical => 5,
        Tab::Images => 6,
        Tab::Links => 7,
        Tab::Sitemaps => match sitemap_view {
            SitemapView::Urls => 8,
            SitemapView::Files => 9,
        },
    }
}

impl State {
    fn view(&self) -> &ViewState {
        &self.views[view_slot(self.tab, self.sitemap_view)]
    }

    fn view_mut(&mut self) -> &mut ViewState {
        let slot = view_slot(self.tab, self.sitemap_view);
        &mut self.views[slot]
    }
}

fn poll(state: &mut State, _t: f64, tx: &mut Tx) -> bool {
    if !state.update_confirmed {
        // The window has laid out a frame. Now the new copy is known to start, so the
        // previous version can be removed.
        state.update_confirmed = true;
        update::install::confirm_pending();
    }
    let browser_changed = poll_browser(state);
    let update_changed = poll_updates(state, tx);
    let crawl_changed = poll_crawl(state);
    if state.crawl_more {
        state.crawl_more = false;
        tx.wake_after(Duration::from_millis(1));
    } else if let Some(wait) = deferred_counts_wait(state) {
        tx.wake_after(wait);
    }
    browser_changed || update_changed || crawl_changed
}

fn poll_browser(state: &mut State) -> bool {
    let Some(probe) = &state.browser_probe else {
        return false;
    };
    match probe.try_recv() {
        Ok(choice) => {
            state.browser = choice;
            state.browser_probe = None;
            true
        }
        Err(mpsc::TryRecvError::Empty) => false,
        Err(mpsc::TryRecvError::Disconnected) => {
            state.browser_probe = None;
            false
        }
    }
}

fn poll_updates(state: &mut State, tx: &mut Tx) -> bool {
    let mut changed = false;
    let mut finished = false;
    if let Some(job) = &state.update_job {
        loop {
            match job.rx.try_recv() {
                Ok(UpdateEvent::Checked(result)) => {
                    finished = true;
                    state.settings.mark_checked();
                    let _ = state.settings.save();
                    match result {
                        Ok(Some(update)) => {
                            state.update = if state.settings.skip_matches(&update.version) {
                                UpdateUi::Idle
                            } else {
                                UpdateUi::Available(update)
                            };
                        }
                        Ok(None) => state.update = UpdateUi::UpToDate,
                        // An automatic check that could not reach GitHub stays quiet;
                        // the next one is a day away and the footer still shows the
                        // version. A check the person asked for reports the reason.
                        Err(error) => {
                            state.update = if job.manual {
                                UpdateUi::Failed(error)
                            } else {
                                UpdateUi::Idle
                            };
                        }
                    }
                    changed = true;
                }
                Ok(UpdateEvent::Progress { received, total }) => {
                    if let UpdateUi::Downloading {
                        received: current,
                        total: current_total,
                        ..
                    } = &mut state.update
                    {
                        *current = received;
                        *current_total = total;
                        changed = true;
                    }
                }
                Ok(UpdateEvent::Downloaded(result)) => {
                    finished = true;
                    // A cancel the person asked for is not a failure to report.
                    let cancelled = job.cancel.load(Ordering::Relaxed);
                    state.update = match result {
                        Ok(downloaded) => UpdateUi::Ready(downloaded),
                        Err(_) if cancelled => UpdateUi::Idle,
                        Err(error) => UpdateUi::Failed(error),
                    };
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
    }
    if finished {
        state.update_job = None;
    }

    let busy = matches!(
        state.update,
        UpdateUi::Checking
            | UpdateUi::Downloading { .. }
            | UpdateUi::Ready(_)
            | UpdateUi::Installing
    );
    if state.update_job.is_none() && !busy {
        let now = Instant::now();
        match state.next_update_check {
            None => {
                let due = now + Duration::from_secs(3);
                state.next_update_check = Some(due);
                tx.wake_after(Duration::from_secs(3));
            }
            Some(due) if now >= due => {
                if state.settings.is_due() {
                    start_check(state, false);
                }
                let next = now + Duration::from_secs(24 * 60 * 60);
                state.next_update_check = Some(next);
                tx.wake_after(Duration::from_secs(24 * 60 * 60));
            }
            Some(_) => {}
        }
    }
    changed
}

const CRAWL_EVENTS_PER_POLL: usize = 512;
const CRAWL_POLL_BUDGET: Duration = Duration::from_millis(8);

fn poll_crawl(state: &mut State) -> bool {
    let Some(job) = state.job.take() else {
        return false;
    };
    let start_len = state.pages.len();
    let mut changed = false;
    let mut sitemap_changed = false;
    let mut finished = None;
    let started = Instant::now();
    let mut processed = 0_usize;
    let mut more = false;
    loop {
        if processed >= CRAWL_EVENTS_PER_POLL || started.elapsed() >= CRAWL_POLL_BUDGET {
            more = true;
            break;
        }
        match job.rx.try_recv() {
            Ok(CrawlEvent::Page(page)) => {
                state.sitemap.observe_page(&page);
                state.search_index.push(page_search_text(&page));
                state.pages.push(*page);
                changed = true;
                sitemap_changed = true;
                if state.phase == RunPhase::Preparing {
                    state.phase = RunPhase::Crawling;
                }
            }
            Ok(CrawlEvent::SitemapFile(file)) => {
                state.sitemap.insert_file(file);
                sitemap_changed = true;
            }
            Ok(CrawlEvent::SitemapUrl(url)) => {
                state.sitemap.insert_url(url);
                sitemap_changed = true;
            }
            Ok(CrawlEvent::SitemapPartial(reason)) => {
                state.sitemap.note_partial(reason);
                sitemap_changed = true;
            }
            Ok(CrawlEvent::SitemapDone) => {
                state.sitemap.discovery_done = true;
                state.sitemap_discovering = false;
                sitemap_changed = true;
            }
            Ok(CrawlEvent::Done(message, report, outcome)) => {
                finished = Some((message, report, outcome));
                break;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = Some((
                    format!("{} pages on the site", state.pages.len()),
                    Box::new(std::mem::take(&mut state.sitemap)),
                    CrawlOutcome::Completed,
                ));
                break;
            }
        }
        processed += 1;
    }
    if changed {
        invalidate_inspector(state);
        let mut broken_added = false;
        for page in &state.pages[start_len..] {
            state.audit.push(page);
            broken_added |= page.status == 0 || page.status >= 400;
        }
        if broken_added {
            state.audit.rebuild_broken_links(&state.pages);
        }
        state.counts_dirty = true;
        state.message = format!("{} pages so far…", state.pages.len());
        refresh_visible(state);
    } else if sitemap_changed && state.tab == Tab::Sitemaps {
        refresh_sitemap_visible(state);
    }
    let counts_changed = refresh_counts(state, false);
    let did_finish = finished.is_some();
    match finished {
        Some((message, report, outcome)) => {
            state.phase = match outcome {
                CrawlOutcome::Completed => RunPhase::Completed,
                CrawlOutcome::Stopped => RunPhase::Stopped,
                CrawlOutcome::Limited => RunPhase::Limited,
                CrawlOutcome::Failed => RunPhase::Failed,
                CrawlOutcome::Blocked => RunPhase::Blocked,
            };
            state.crawl_elapsed = state.crawl_started.take().map(|started| started.elapsed());
            state.run_outcome = Some(message.clone());
            state.message = message;
            state.audit = Audit::from_pages(&state.pages);
            invalidate_inspector(state);
            state.sitemap = *report;
            state.sitemap_discovering = false;
            state.counts_dirty = true;
            let _ = refresh_counts(state, true);
            sort_pages(state);
            if state.tab == Tab::Sitemaps {
                sort_sitemap(state);
            }
            // Keep a still-valid selection; otherwise the first visible row is chosen by
            // `sync_selection`. Never jump to a Page URL while Sitemap Files is showing.
            sync_selection(state);
        }
        None => {
            state.job = Some(job);
            state.crawl_more = more;
        }
    }
    if did_finish {
        state.crawl_more = false;
    }
    changed || sitemap_changed || did_finish || counts_changed || state.counts_deferred
}

fn start_check(state: &mut State, manual: bool) {
    if state.update_job.is_some() {
        return;
    }
    if manual {
        state.update = UpdateUi::Checking;
    }
    let (tx, rx) = mpsc::channel();
    let waker = state.waker.clone();
    std::thread::spawn(move || {
        let result = update::check().map_err(|error| error.to_string());
        let _ = tx.send(UpdateEvent::Checked(result));
        if let Some(wake) = &waker {
            wake();
        }
    });
    state.update_job = Some(UpdateJob {
        rx,
        cancel: Arc::new(AtomicBool::new(false)),
        manual,
    });
}

fn start_download(state: &mut State) {
    if state.update_job.is_some() {
        return;
    }
    let UpdateUi::Available(offered) = std::mem::replace(&mut state.update, UpdateUi::Idle) else {
        state.update = UpdateUi::Idle;
        return;
    };
    let version = offered.version.clone();
    let total = Some(offered.asset.size);
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = cancel.clone();
    let waker = state.waker.clone();
    std::thread::spawn(move || {
        let progress_tx = tx.clone();
        let progress_wake = waker.clone();
        let result = update::download(&offered, &thread_cancel, move |received, total| {
            let _ = progress_tx.send(UpdateEvent::Progress { received, total });
            if let Some(wake) = &progress_wake {
                wake();
            }
        })
        .map_err(|error| error.to_string());
        let _ = tx.send(UpdateEvent::Downloaded(result));
        if let Some(wake) = &waker {
            wake();
        }
    });
    state.update = UpdateUi::Downloading {
        version,
        received: 0,
        total,
    };
    state.update_job = Some(UpdateJob {
        rx,
        cancel,
        manual: true,
    });
}

fn cancel_download(state: &mut State) {
    if let Some(job) = &state.update_job {
        job.cancel.store(true, Ordering::Relaxed);
    }
}

fn dismiss_update(state: &mut State) {
    cancel_download(state);
    state.update = UpdateUi::Idle;
}

fn skip_update(state: &mut State) {
    let version = match &state.update {
        UpdateUi::Available(offered) => Some(offered.version.clone()),
        _ => None,
    };
    if let Some(version) = version {
        let _ = state.settings.skip(&version);
    }
    state.update = UpdateUi::Idle;
}

fn can_install(state: &State) -> bool {
    !state.phase.active() && matches!(state.update, UpdateUi::Ready(_))
}

fn start_install(state: &mut State, tx: &mut Tx) {
    // Starting the helper swaps the running binary. Never begin that while a crawl is
    // feeding results into this process; the user can stop the crawl first.
    if !can_install(state) {
        return;
    }
    let UpdateUi::Ready(downloaded) = std::mem::replace(&mut state.update, UpdateUi::Installing)
    else {
        return;
    };
    match update::install::prepare(&downloaded) {
        Ok(prepared) => match update::install::start_helper(&prepared) {
            Ok(()) => {
                // Keep the lock until this process really exits. The helper polls it
                // and would otherwise be free to swap files during shutdown.
                std::mem::forget(prepared);
                tx.quit();
            }
            Err(error) => state.update = UpdateUi::Failed(error.to_string()),
        },
        Err(error) => state.update = UpdateUi::Failed(error.to_string()),
    }
}

fn check_for_updates_menu(state: &mut State, _tx: &mut Tx) {
    state.settings.clear_skip().ok();
    start_check(state, true);
}

fn retry_update(state: &mut State) {
    state.settings.clear_skip().ok();
    start_check(state, true);
}

fn validate_start(state: &State) -> Result<(String, Limits), FieldErrors> {
    let mut errors = FieldErrors::default();
    let seed = match crawl::normalize_seed(&state.url.text) {
        Ok(seed) => Some(seed),
        Err(message) => {
            errors.url = message;
            None
        }
    };
    let max_pages = match state.pages_limit.text.trim().parse::<usize>() {
        Ok(pages) if pages > 0 => Some(pages),
        _ => {
            errors.pages = "Enter a whole number of pages greater than 0.".into();
            None
        }
    };
    let max_depth = match state.depth_limit.text.trim().parse::<u8>() {
        Ok(depth) if depth <= 10 => Some(depth),
        _ => {
            errors.depth = "Enter a whole depth from 0 to 10.".into();
            None
        }
    };
    if state.rendering == RenderMode::Browser && !state.browser.status.is_ready() {
        errors.setup = "Choose a usable Chrome or Chromium before starting.".into();
    }
    match (seed, max_pages, max_depth) {
        (Some(seed), Some(max_pages), Some(max_depth)) if !errors.any() => Ok((
            seed,
            Limits {
                max_pages,
                max_depth,
            },
        )),
        _ => Err(errors),
    }
}

fn focus_first_error(state: &mut State) {
    state.url.focused = false;
    state.pages_limit.focused = false;
    state.depth_limit.focused = false;
    state.view_mut().query.focused = false;
    if !state.field_errors.url.is_empty() {
        state.url.focused = true;
    } else if !state.field_errors.pages.is_empty() {
        state.pages_limit.focused = true;
    } else if !state.field_errors.depth.is_empty() {
        state.depth_limit.focused = true;
    }
}

fn start_crawl(state: &mut State) {
    if state.phase.active() {
        return;
    }
    let (seed, limits) = match validate_start(state) {
        Ok(plan) => plan,
        Err(errors) => {
            state.field_errors = errors;
            state.message = state.field_errors.summary();
            focus_first_error(state);
            return;
        }
    };
    let host = url::Url::parse(&seed)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| seed.clone());
    state.field_errors.clear();
    state.run_outcome = None;
    state.run = Some(RunContext {
        seed: seed.clone(),
        host,
        limits,
        rendering: state.rendering,
        sitemaps: state.use_sitemaps,
    });
    state.max_pages = limits.max_pages;
    state.phase = RunPhase::Preparing;
    state.crawl_started = Some(Instant::now());
    state.crawl_elapsed = None;
    state.export_notice = None;
    state.action_notice = None;
    state.pages.clear();
    reset_inspector_dataset(state);
    state.search_index.clear();
    state.visible.clear();
    state.audit = Audit::default();
    state.counts = Counts::default();
    state.counts_dirty = false;
    state.counts_deferred = false;
    state.counts_at = None;
    state.view_counts = ViewCounts::default();
    state.sitemap = SitemapReport::default();
    state.sitemap.discovery_done = !state.use_sitemaps;
    state.sitemap_discovering = state.use_sitemaps;
    state.sitemap_visible.clear();
    state.sitemap_file_visible.clear();
    state.table.clear_selection();
    reset_view_searches(state);
    state.message = format!("Preparing {seed}…");

    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let waker = state.waker.clone();
    let options = crawl::CrawlOptions {
        sitemaps: state.use_sitemaps,
        rendering: state.rendering,
        browser_path: state.browser.path.clone(),
    };
    state.job = Some(CrawlJob {
        rx,
        cancel: cancel.clone(),
        stopping: false,
    });
    std::thread::spawn(move || {
        let event_tx = tx.clone();
        let wake = waker.clone();
        let outcome = crawl::crawl_site_with(&seed, limits, options, cancel.clone(), |event| {
            let event = match event {
                crawl::CrawlEvent::Page(page) => CrawlEvent::Page(page),
                crawl::CrawlEvent::SitemapFile(file) => CrawlEvent::SitemapFile(file),
                crawl::CrawlEvent::SitemapUrl(url) => CrawlEvent::SitemapUrl(url),
                crawl::CrawlEvent::SitemapPartial(reason) => CrawlEvent::SitemapPartial(reason),
                crawl::CrawlEvent::SitemapDone => CrawlEvent::SitemapDone,
            };
            let _ = event_tx.send(event);
            if let Some(wake) = &wake {
                wake();
            }
        });
        let _ = tx.send(CrawlEvent::Done(
            outcome.message,
            Box::new(outcome.sitemap),
            outcome.outcome,
        ));
        if let Some(wake) = &waker {
            wake();
        }
    });
}

fn reset_view_searches(state: &mut State) {
    for view in &mut state.views {
        view.query.replace_text("");
        view.filter = CheckFilter::All;
        view.selected = None;
    }
}

fn stop_crawl(state: &mut State) {
    if let Some(job) = &mut state.job {
        if !job.stopping {
            job.cancel.store(true, Ordering::Relaxed);
            job.stopping = true;
        }
        state.phase = RunPhase::Stopping;
        state.message = "Stopping…".into();
    }
}

/// Stop the crawl and kill every browser this process still owns.
///
/// `on_close` runs on the normal window-close path, which otherwise leaves the detached crawl
/// thread and its browser running. `main` calls the same cleanup after `run()` returns so the
/// quit path cannot leak either.
fn app_close(state: &mut State, _tx: &mut Tx) {
    stop_owned_work(state);
    render::kill_owned_browsers();
}

/// The state part of app close, kept separate so it can be tested without a window.
fn stop_owned_work(state: &mut State) {
    if let Some(job) = &state.job {
        job.cancel.store(true, Ordering::Relaxed);
    }
}

fn read_is_incomplete(status: u16, content_type: &str, read: ReadState) -> bool {
    match read {
        // A transport failure is a known failed response, not a body that was cut short.
        ReadState::Failed => false,
        ReadState::Truncated => true,
        // No body is expected for an error, redirect or non-HTML response; those are known
        // states, not incomplete checks. A 200 HTML body that was never read is incomplete.
        ReadState::NotRead => status == 200 && content_type.to_ascii_lowercase().contains("html"),
        ReadState::Complete => false,
    }
}

fn page_incomplete(page: &Page) -> bool {
    !page.indexability.capture.is_complete()
        || read_is_incomplete(
            page.status,
            &page.content_type,
            page.indexability.evidence.read,
        )
}

/// Findings in an assessment that are observations, not just "this capture did not finish".
fn assessment_has_known_finding(assessment: &Assessment) -> bool {
    assessment.findings.iter().any(|finding| {
        finding.severity != Severity::Info
            && !matches!(
                finding.code,
                FindingCode::CaptureIncomplete
                    | FindingCode::BodyTruncated
                    | FindingCode::EvidenceTruncated
                    | FindingCode::BodyNotRead
            )
    })
}

/// A known finding at the current view's semantics. Incompleteness and a confirmed observation
/// can coexist: a retained noindex or observed error is still a finding when the capture is
/// incomplete. Completed content claims remain gated by `content_issue`.
fn page_has_finding(tab: Tab, page: &Page, audit: &Audit) -> bool {
    match tab {
        Tab::Internal => assessment_has_known_finding(&page.indexability),
        Tab::Titles => content_issue(page, audit, ContentCheck::Title),
        Tab::Meta => content_issue(page, audit, ContentCheck::Meta),
        Tab::H1 => content_issue(page, audit, ContentCheck::H1),
        Tab::Canonical => content_issue(page, audit, ContentCheck::Canonical),
        Tab::Images => content_issue(page, audit, ContentCheck::Images),
        Tab::Response => !(200..=299).contains(&page.status),
        Tab::Links => audit.has_broken_links(page),
        Tab::Sitemaps => false,
    }
}

/// Lowercased facts the result search reads, built once per page.
fn page_search_text(page: &Page) -> String {
    let mut text = String::with_capacity(
        page.url.len()
            + page.title.len()
            + page.meta_description.len()
            + page.canonical.len()
            + page.content_type.len()
            + 56,
    );
    text.push_str(&page.url);
    text.push(' ');
    text.push_str(&page.title);
    text.push(' ');
    text.push_str(&page.meta_description);
    for h1 in &page.h1s {
        text.push(' ');
        text.push_str(h1);
    }
    text.push(' ');
    text.push_str(&page.canonical);
    text.push(' ');
    text.push_str(&page.content_type);
    text.to_lowercase()
}

fn normalized_query(query: &str) -> String {
    query.trim().to_lowercase()
}

fn page_matches_query(state: &State, index: usize, tab: Tab, needle: &str) -> bool {
    if state
        .search_index
        .get(index)
        .is_some_and(|text| text.contains(needle))
    {
        return true;
    }
    let Some(page) = state.pages.get(index) else {
        return false;
    };
    issue_for(tab, page, &state.audit)
        .to_lowercase()
        .contains(needle)
}

fn sitemap_url_incomplete(entry: &SitemapUrl) -> bool {
    entry.state != UrlState::Checked
        || !entry.assessment.capture.is_complete()
        || read_is_incomplete(
            entry.status,
            &entry.content_type,
            entry.assessment.evidence.read,
        )
}

fn sitemap_url_has_finding(entry: &SitemapUrl) -> bool {
    match entry.state {
        UrlState::Checked => assessment_has_known_finding(&entry.assessment),
        // Blocked, skipped and not-checked are known coverage states that need attention, not
        // completed clean checks. They may also appear under Incomplete.
        UrlState::Blocked | UrlState::Skipped | UrlState::NotChecked => true,
        UrlState::Pending => false,
    }
}

fn sitemap_file_incomplete(file: &SitemapFile) -> bool {
    matches!(file.state, FileState::Pending | FileState::Skipped)
}

fn sitemap_file_has_finding(file: &SitemapFile) -> bool {
    match file.state {
        FileState::Error | FileState::Skipped => true,
        // A successful read can still carry a document-level issue; guessed absence and
        // pending discovery are informational rather than findings.
        FileState::Ok => file.needs_attention(),
        FileState::Absent | FileState::Pending => false,
    }
}

fn visible_id_ref(state: &State, row: usize) -> Option<&str> {
    if state.tab == Tab::Sitemaps {
        return match state.sitemap_view {
            SitemapView::Urls => state
                .sitemap_visible
                .get(row)
                .and_then(|index| state.sitemap.urls.get(*index))
                .map(|entry| entry.url.as_str()),
            SitemapView::Files => state
                .sitemap_file_visible
                .get(row)
                .and_then(|index| state.sitemap.files.get(*index))
                .map(|file| file.url.as_str()),
        };
    }
    state
        .visible
        .get(row)
        .and_then(|index| state.pages.get(*index))
        .map(|page| page.url.as_str())
}

fn visible_id(state: &State, row: usize) -> Option<String> {
    visible_id_ref(state, row).map(str::to_string)
}

/// Keep the selected row inside the visible set, choosing the first visible row when the old
/// selection was filtered out and clearing it when nothing is visible.
fn sync_selection(state: &mut State) {
    let count = visible_len(state);
    let desired = state.view().selected.as_deref();
    let chosen = desired
        .filter(|id| (0..count).any(|row| visible_id_ref(state, row) == Some(*id)))
        .map(str::to_string)
        .or_else(|| visible_id(state, 0));
    sync_relation_selection(state, chosen.as_deref());
    match chosen {
        Some(id) => {
            state.table.select(id.clone());
            state.view_mut().selected = Some(id);
        }
        None => {
            state.table.clear_selection();
            state.view_mut().selected = None;
        }
    }
}

fn apply_page_sort(state: &mut State, force: bool) {
    // While a crawl streams, new rows append instead of re-sorting the list under the
    // pointer. The run finishes with a deliberate sort, and user actions force one.
    if !force && state.phase.active() {
        return;
    }
    let (column, dir) = state.view().sort.clone();
    let tab = state.tab;
    let State { pages, audit, .. } = state;
    state.visible.sort_by(|left, right| {
        dir.apply(compare(&pages[*left], &pages[*right], tab, &column, audit))
    });
}

fn refresh_visible(state: &mut State) {
    refresh_visible_inner(state, false);
}

fn refresh_visible_forced(state: &mut State) {
    refresh_visible_inner(state, true);
}

fn refresh_visible_inner(state: &mut State, force: bool) {
    if state.tab == Tab::Sitemaps {
        refresh_sitemap_visible_inner(state, force);
        return;
    }
    let tab = state.tab;
    let filter = state.view().filter;
    let needle = normalized_query(&state.view().query.text);
    let mut findings = 0;
    let mut incomplete = 0;
    let mut matching = Vec::new();
    for (index, page) in state.pages.iter().enumerate() {
        let is_incomplete = page_incomplete(page);
        let is_finding = page_has_finding(tab, page, &state.audit);
        incomplete += usize::from(is_incomplete);
        findings += usize::from(is_finding);
        let keep = match filter {
            CheckFilter::All => true,
            CheckFilter::Findings => is_finding,
            CheckFilter::Incomplete => is_incomplete,
        };
        if !keep {
            continue;
        }
        if !needle.is_empty() && !page_matches_query(state, index, tab, &needle) {
            continue;
        }
        matching.push(index);
    }
    state.view_counts = ViewCounts {
        total: state.pages.len(),
        findings,
        incomplete,
    };
    if !force && state.phase.active() {
        // Incoming rows append; existing rows keep the order the person already chose and is
        // reading, even while new crawl events arrive.
        state.visible = merge_streaming_order(&state.visible, &matching);
    } else {
        state.visible = matching;
        apply_page_sort(state, force);
    }
    sync_selection(state);
}

fn merge_streaming_order(previous: &[usize], matching: &[usize]) -> Vec<usize> {
    let matching_set: std::collections::HashSet<usize> = matching.iter().copied().collect();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut ordered = Vec::with_capacity(matching.len());
    for index in previous {
        if matching_set.contains(index) && seen.insert(*index) {
            ordered.push(*index);
        }
    }
    for index in matching {
        if seen.insert(*index) {
            ordered.push(*index);
        }
    }
    ordered
}

fn refresh_sitemap_visible(state: &mut State) {
    refresh_sitemap_visible_inner(state, false);
}

fn refresh_sitemap_visible_inner(state: &mut State, force: bool) {
    let view = state.sitemap_view;
    let filter = state.view().filter;
    let needle = normalized_query(&state.view().query.text);
    let (mut findings, mut incomplete) = (0, 0);
    match view {
        SitemapView::Urls => {
            let mut visible = Vec::new();
            for (index, entry) in state.sitemap.urls.iter().enumerate() {
                let is_incomplete = sitemap_url_incomplete(entry);
                let is_finding = sitemap_url_has_finding(entry);
                incomplete += usize::from(is_incomplete);
                findings += usize::from(is_finding);
                let keep = match filter {
                    CheckFilter::All => true,
                    CheckFilter::Findings => is_finding,
                    CheckFilter::Incomplete => is_incomplete,
                };
                if !keep {
                    continue;
                }
                if !needle.is_empty() {
                    let mut text = format!(
                        "{} {} {} {}",
                        entry.url,
                        entry.state.label(),
                        entry.issue,
                        entry.sources.join(" ")
                    );
                    if entry.state == UrlState::Checked {
                        text.push(' ');
                        text.push_str(entry.assessment.reason_text().as_str());
                    }
                    if !text.to_lowercase().contains(&needle) {
                        continue;
                    }
                }
                visible.push(index);
            }
            if state.tab == Tab::Sitemaps {
                state.view_counts = ViewCounts {
                    total: state.sitemap.urls.len(),
                    findings,
                    incomplete,
                };
            }
            if !force && state.phase.active() {
                state.sitemap_visible = merge_streaming_order(&state.sitemap_visible, &visible);
            } else {
                state.sitemap_visible = visible.clone();
            }
        }
        SitemapView::Files => {
            let mut visible = Vec::new();
            for (index, file) in state.sitemap.files.iter().enumerate() {
                let is_incomplete = sitemap_file_incomplete(file);
                let is_finding = sitemap_file_has_finding(file);
                incomplete += usize::from(is_incomplete);
                findings += usize::from(is_finding);
                let keep = match filter {
                    CheckFilter::All => true,
                    CheckFilter::Findings => is_finding,
                    CheckFilter::Incomplete => is_incomplete,
                };
                if !keep {
                    continue;
                }
                if !needle.is_empty() {
                    let text = format!(
                        "{} {} {} {} {}",
                        file.url,
                        file.state.label(),
                        file.origin.label(),
                        file.issue,
                        sitemap_kind_label(file.kind)
                    )
                    .to_lowercase();
                    if !text.contains(&needle) {
                        continue;
                    }
                }
                visible.push(index);
            }
            if state.tab == Tab::Sitemaps {
                state.view_counts = ViewCounts {
                    total: state.sitemap.files.len(),
                    findings,
                    incomplete,
                };
            }
            if !force && state.phase.active() {
                state.sitemap_file_visible =
                    merge_streaming_order(&state.sitemap_file_visible, &visible);
            } else {
                state.sitemap_file_visible = visible.clone();
            }
        }
    }
    apply_sitemap_sort(state, force);
    sync_selection(state);
}

fn apply_sitemap_sort(state: &mut State, force: bool) {
    if !force && state.phase.active() {
        return;
    }
    let (column, dir) = state.view().sort.clone();
    match state.sitemap_view {
        SitemapView::Urls => {
            let State {
                sitemap,
                sitemap_visible,
                ..
            } = state;
            sitemap_visible.sort_by(|left, right| {
                dir.apply(compare_sitemap_url(
                    &sitemap.urls[*left],
                    &sitemap.urls[*right],
                    &column,
                ))
            });
        }
        SitemapView::Files => {
            let State {
                sitemap,
                sitemap_file_visible,
                ..
            } = state;
            sitemap_file_visible.sort_by(|left, right| {
                dir.apply(compare_sitemap_file(
                    &sitemap.files[*left],
                    &sitemap.files[*right],
                    &column,
                ))
            });
        }
    }
}

fn sort_sitemap(state: &mut State) {
    refresh_sitemap_visible_inner(state, true);
}

fn compare_sitemap_url(a: &SitemapUrl, b: &SitemapUrl, column: &str) -> std::cmp::Ordering {
    match column {
        "state" => a.state.label().cmp(b.state.label()),
        "status" => a.status.cmp(&b.status),
        "issue" => b
            .needs_attention()
            .cmp(&a.needs_attention())
            .then_with(|| a.issue.cmp(&b.issue))
            .then_with(|| a.url.cmp(&b.url)),
        "source" => a.sources.first().cmp(&b.sources.first()),
        _ => a.url.cmp(&b.url),
    }
}

fn compare_sitemap_file(a: &SitemapFile, b: &SitemapFile, column: &str) -> std::cmp::Ordering {
    match column {
        "state" => a.state.label().cmp(b.state.label()),
        "status" => a.status.cmp(&b.status),
        "urls" => a.urls.cmp(&b.urls),
        "issue" => b
            .needs_attention()
            .cmp(&a.needs_attention())
            .then_with(|| a.issue.cmp(&b.issue))
            .then_with(|| a.url.cmp(&b.url)),
        _ => a.url.cmp(&b.url),
    }
}

fn refresh_counts(state: &mut State, force: bool) -> bool {
    if !state.counts_dirty {
        return false;
    }
    if !force
        && state.phase.active()
        && state
            .counts_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(100))
    {
        state.counts_deferred = true;
        return false;
    }
    state.counts = counts_with(&state.pages, &state.audit);
    state.counts_at = Some(Instant::now());
    state.counts_dirty = false;
    state.counts_deferred = false;
    true
}

fn deferred_counts_wait(state: &State) -> Option<Duration> {
    if !state.counts_deferred {
        return None;
    }
    let elapsed = state.counts_at.map(|at| at.elapsed()).unwrap_or_default();
    Some(
        Duration::from_millis(100)
            .saturating_sub(elapsed)
            .max(Duration::from_millis(1)),
    )
}

fn set_pages(state: &mut State, pages: Vec<Page>, message: String) {
    let seed = pages
        .first()
        .map(|page| page.url.clone())
        .unwrap_or_else(|| state.url.text.clone());
    let host = url::Url::parse(&seed)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| seed.clone());
    state.run = Some(RunContext {
        seed,
        host,
        limits: Limits {
            max_pages: pages.len().max(1),
            max_depth: state.depth_limit.text.trim().parse().unwrap_or(MAX_DEPTH),
        },
        rendering: state.rendering,
        sitemaps: state.use_sitemaps,
    });
    state.run_outcome = Some(message.clone());
    state.pages = pages;
    reset_inspector_dataset(state);
    state.search_index = state.pages.iter().map(page_search_text).collect();
    state.audit = Audit::from_pages(&state.pages);
    state.message = message;
    state.counts_dirty = true;
    let _ = refresh_counts(state, true);
    for tab in [
        Tab::Internal,
        Tab::Titles,
        Tab::Meta,
        Tab::H1,
        Tab::Response,
        Tab::Canonical,
        Tab::Images,
        Tab::Links,
    ] {
        let slot = view_slot(tab, state.sitemap_view);
        state.views[slot].selected = None;
    }
    state.phase = RunPhase::Completed;
    sort_pages(state);
}

fn load_sample(state: &mut State) {
    state.url.replace_text("https://example.com/");
    state.phase = RunPhase::Completed;
    state.view_mut().filter = CheckFilter::All;
    state.tab = Tab::Internal;
    state.sitemap_view = SitemapView::Urls;
    state.sitemap_discovering = false;
    state.sitemap = calmtoad::sitemap::sample_report();
    set_pages(
        state,
        crawl::sample_pages(),
        "9 pages on example.com".into(),
    );
    refresh_sitemap_visible(state);
}

fn load_indexability_sample(state: &mut State) {
    state.url.replace_text("https://example.com/");
    state.phase = RunPhase::Completed;
    state.view_mut().filter = CheckFilter::All;
    state.tab = Tab::Internal;
    state.sitemap_view = SitemapView::Urls;
    state.sitemap_discovering = false;
    state.sitemap = calmtoad::sitemap::sample_report();
    let pages = crawl::indexability_sample_pages();
    let message = format!("{} pages on example.com", pages.len());
    set_pages(state, pages, message);
    state.table.set_sort("indexability", SortDir::Ascending);
    sort_pages(state);
    refresh_sitemap_visible(state);
}

/// One seam for a header sort: the table indicator and the effective view order always move
/// together, whichever widget requested the change.
fn set_page_sort(state: &mut State, column: &str, dir: SortDir) {
    state.view_mut().sort = (column.to_string(), dir);
    state.table.set_sort(column, dir);
    refresh_visible_forced(state);
}

fn set_sitemap_sort(state: &mut State, column: &str, dir: SortDir) {
    state.view_mut().sort = (column.to_string(), dir);
    state.table.set_sort(column, dir);
    sort_sitemap(state);
}

fn sort_pages(state: &mut State) {
    if state.tab == Tab::Sitemaps {
        sort_sitemap(state);
        return;
    }
    refresh_visible_forced(state);
}

/// Hard blockers first, then advisories, then unknowns, then clean pages.
fn label_rank(label: Label) -> u8 {
    match label {
        Label::FetchFailed => 0,
        Label::HttpError => 1,
        Label::Noindex => 2,
        Label::Redirect => 3,
        Label::ReviewCanonical => 4,
        Label::CanonicalElsewhere => 5,
        Label::NotAssessed => 6,
        Label::NoBlockFound => 7,
    }
}

fn compare(a: &Page, b: &Page, tab: Tab, column: &str, audit: &Audit) -> std::cmp::Ordering {
    match (tab, column) {
        (_, "url") => a.url.cmp(&b.url),
        (Tab::Internal | Tab::Response, "status") => a.status.cmp(&b.status),
        (Tab::Internal, "indexability") => label_rank(a.indexability.label)
            .cmp(&label_rank(b.indexability.label))
            .then_with(|| a.indexability.label.text().cmp(b.indexability.label.text())),
        (Tab::Internal, "words") => a.words.cmp(&b.words),
        (Tab::Titles, "title") => a.title.cmp(&b.title),
        (Tab::Titles, "len") => a.title.chars().count().cmp(&b.title.chars().count()),
        (Tab::Titles, "issue") => compare_issue_rows(a, b, tab, audit),
        (Tab::Meta, "meta") => a.meta_description.cmp(&b.meta_description),
        (Tab::Meta, "len") => a
            .meta_description
            .chars()
            .count()
            .cmp(&b.meta_description.chars().count()),
        (Tab::Meta, "issue") => compare_issue_rows(a, b, tab, audit),
        (Tab::H1, "h1") => a.h1().cmp(&b.h1()),
        (Tab::H1, "count") => a.h1s.len().cmp(&b.h1s.len()),
        (Tab::H1, "issue") => compare_issue_rows(a, b, tab, audit),
        (Tab::Response, "type") => a.content_type.cmp(&b.content_type),
        (Tab::Response, "issue") => compare_issue_rows(a, b, tab, audit),
        (Tab::Canonical, "canonical") => a.canonical.cmp(&b.canonical),
        (Tab::Canonical, "issue") => compare_issue_rows(a, b, tab, audit),
        (Tab::Images, "images") => a.images.cmp(&b.images),
        (Tab::Images, "missing_alt") => a.images_missing_alt.cmp(&b.images_missing_alt),
        (Tab::Images, "issue") => compare_issue_rows(a, b, tab, audit),
        (Tab::Links, "inbound") => audit.inbound(a).cmp(&audit.inbound(b)),
        (Tab::Links, "outbound") => a.links.len().cmp(&b.links.len()),
        (Tab::Links, "issue") => compare_issue_rows(a, b, tab, audit),
        _ => a.url.cmp(&b.url),
    }
}

/// Put rows with findings first, incomplete checks after them, and clean rows last.
fn compare_issue_rows(a: &Page, b: &Page, tab: Tab, audit: &Audit) -> std::cmp::Ordering {
    let (a_issue, b_issue) = (issue_for(tab, a, audit), issue_for(tab, b, audit));
    let row_rank = |page: &Page, issue: &str| {
        if !issue.is_empty() {
            0
        } else if matches!(
            tab,
            Tab::Titles | Tab::Meta | Tab::H1 | Tab::Canonical | Tab::Images
        ) && !auditable(page)
        {
            1
        } else {
            2
        }
    };
    row_rank(a, &a_issue)
        .cmp(&row_rank(b, &b_issue))
        .then_with(|| a_issue.cmp(&b_issue))
        .then_with(|| a.url.cmp(&b.url))
}

fn visible_index(state: &State, row: usize) -> usize {
    state.visible.get(row).copied().unwrap_or(0)
}

fn visible_len(state: &State) -> usize {
    if state.tab == Tab::Sitemaps {
        return match state.sitemap_view {
            SitemapView::Urls => state.sitemap_visible.len(),
            SitemapView::Files => state.sitemap_file_visible.len(),
        };
    }
    state.visible.len()
}

fn issue_for(tab: Tab, page: &Page, audit: &Audit) -> String {
    match tab {
        Tab::Internal => page.indexability.reason_text(),
        Tab::Titles => title_issue(page, audit),
        Tab::Meta => meta_issue(page, audit),
        Tab::H1 => h1_issue(page, audit),
        Tab::Response => {
            let mut issue = response_issue(page);
            let chain = redirect_issue(page, audit);
            if !chain.is_empty() {
                if !issue.is_empty() {
                    issue.push_str(" · ");
                }
                issue.push_str(&chain);
            }
            issue
        }
        Tab::Canonical => canonical_issue(page),
        Tab::Images => image_issue(page),
        Tab::Links => links_issue(page, audit),
        Tab::Sitemaps => String::new(),
    }
}

fn tab_key(tab: Tab) -> &'static str {
    match tab {
        Tab::Internal => "tab-internal",
        Tab::Titles => "tab-titles",
        Tab::Meta => "tab-meta",
        Tab::H1 => "tab-h1",
        Tab::Response => "tab-response",
        Tab::Canonical => "tab-canonical",
        Tab::Images => "tab-images",
        Tab::Links => "tab-links",
        Tab::Sitemaps => "tab-sitemaps",
    }
}

fn tab_label(tab: Tab, counts: &Counts, sitemap: &SitemapReport) -> String {
    match tab {
        Tab::Internal => format!("Internal {}", counts.pages),
        Tab::Titles => format!("Titles {}", counts.title),
        Tab::Meta => format!("Meta {}", counts.meta),
        Tab::H1 => format!("H1 {}", counts.h1),
        Tab::Response => format!("Response {}", counts.response),
        Tab::Canonical => format!("Canonical {}", counts.canonical),
        Tab::Images => format!("Images {}", counts.images),
        Tab::Links => format!("Links {}", counts.links),
        Tab::Sitemaps => format!("Sitemaps {}", sitemap.urls.len()),
    }
}

fn tab_help(tab: Tab, counts: &Counts, sitemap: &SitemapReport) -> String {
    match tab {
        Tab::Internal => format!("{} crawled URLs", counts.pages),
        Tab::Titles => format!(
            "{} URLs with a title finding · {} crawled URLs",
            counts.title, counts.pages
        ),
        Tab::Meta => format!(
            "{} URLs with a meta description finding · {} crawled URLs",
            counts.meta, counts.pages
        ),
        Tab::H1 => format!(
            "{} URLs with an H1 finding · {} crawled URLs",
            counts.h1, counts.pages
        ),
        Tab::Response => format!(
            "{} URLs with a response finding · {} crawled URLs",
            counts.response, counts.pages
        ),
        Tab::Canonical => format!(
            "{} URLs with a canonical finding · {} crawled URLs",
            counts.canonical, counts.pages
        ),
        Tab::Images => format!(
            "{} URLs with an image-alt finding · {} crawled URLs",
            counts.images, counts.pages
        ),
        Tab::Links => format!(
            "{} URLs with a broken-link finding · {} crawled URLs",
            counts.links, counts.pages
        ),
        Tab::Sitemaps => format!(
            "{} URLs listed by sitemaps · {} sitemap files",
            sitemap.urls.len(),
            sitemap.files.len()
        ),
    }
}

fn tab_short(tab: Tab) -> &'static str {
    match tab {
        Tab::Internal => "Internal",
        Tab::Titles => "Titles",
        Tab::Meta => "Meta",
        Tab::H1 => "H1",
        Tab::Response => "Response",
        Tab::Canonical => "Canonical",
        Tab::Images => "Images",
        Tab::Links => "Links",
        Tab::Sitemaps => "Sitemaps",
    }
}

fn tab_count(tab: Tab, counts: &Counts, sitemap: &SitemapReport) -> usize {
    match tab {
        Tab::Internal => counts.pages,
        Tab::Titles => counts.title,
        Tab::Meta => counts.meta,
        Tab::H1 => counts.h1,
        Tab::Response => counts.response,
        Tab::Canonical => counts.canonical,
        Tab::Images => counts.images,
        Tab::Links => counts.links,
        Tab::Sitemaps => sitemap.urls.len(),
    }
}

fn tab_title(tab: Tab) -> &'static str {
    match tab {
        Tab::Internal => "Internal URLs",
        Tab::Titles => "Page titles",
        Tab::Meta => "Meta descriptions",
        Tab::H1 => "H1",
        Tab::Response => "Response codes",
        Tab::Canonical => "Canonicals",
        Tab::Images => "Image alt text",
        Tab::Links => "Internal links",
        Tab::Sitemaps => "Sitemaps",
    }
}

fn active_title(state: &State) -> &'static str {
    match (state.tab, state.sitemap_view) {
        (Tab::Sitemaps, SitemapView::Urls) => "Sitemap URLs",
        (Tab::Sitemaps, SitemapView::Files) => "Sitemap files",
        _ => tab_title(state.tab),
    }
}

const GUTTER: f32 = 20.0;

fn view(state: &State, cx: &mut Cx<State>) -> El {
    let k = theme::app(cx.system_dark());
    let inset = cx.titlebar_inset();
    let counts = &state.counts;
    let mut app = col()
        .key("app")
        .w_full()
        .h_full()
        .fill(k.c.page)
        .child(topbar(&k, state, counts, inset));
    if state.phase.active() {
        app = app.child(progress_line(&k, state));
    }
    app = app.child(command_bar(&k, state));
    if state.rendering == RenderMode::Browser {
        app = app.child(browser_strip(&k, state));
    }
    if let Some(banner) = update_banner(&k, state) {
        app = app.child(banner);
    }
    if !state.pages.is_empty() || state.tab == Tab::Sitemaps {
        app = app.child(section_header(&k, state));
    }
    if state.run_details_open {
        app = app.child(run_details_panel(&k, state));
    }
    app.child(results(&k, state))
        .child(status_line(&k, state))
        .on_key(handle_app_key)
}

fn handle_app_key(state: &mut State, event: &KeyEvent, _tx: &mut Tx) -> bool {
    matches!(event.key, Key::Escape) && close_context_state(state)
}

fn close_context(state: &mut State, _tx: &mut Tx) {
    close_context_state(state);
}

fn close_context_state(state: &mut State) -> bool {
    if state.details_open {
        state.details_open = false;
        return true;
    }
    if state.run_details_open {
        state.run_details_open = false;
        return true;
    }
    if state.evidence_open {
        state.evidence_open = false;
        return true;
    }
    if state.capture_open {
        state.capture_open = false;
        return true;
    }
    false
}

fn focus_search_results(state: &mut State, tx: &mut Tx) {
    // Drop the current field first; a model-only focus change would otherwise stay with the
    // previously focused field for a frame.
    tx.blur();
    focus_search_state(state);
}

fn focus_site_url(state: &mut State, tx: &mut Tx) {
    tx.blur();
    focus_url_state(state);
}

fn focus_search_state(state: &mut State) -> bool {
    let has_data = if state.tab == Tab::Sitemaps {
        match state.sitemap_view {
            SitemapView::Urls => !state.sitemap.urls.is_empty(),
            SitemapView::Files => !state.sitemap.files.is_empty(),
        }
    } else {
        !state.pages.is_empty()
    };
    if !has_data {
        return false;
    }
    state.url.focused = false;
    state.pages_limit.focused = false;
    state.depth_limit.focused = false;
    state.view_mut().query.focused = true;
    true
}

fn focus_url_state(state: &mut State) -> bool {
    state.url.focused = true;
    state.pages_limit.focused = false;
    state.depth_limit.focused = false;
    state.view_mut().query.focused = false;
    true
}

fn run_details_panel(k: &Kit, state: &State) -> El {
    let mut lines: Vec<String> = Vec::new();
    if let Some(run) = &state.run {
        lines.push(format!("Seed {}", run.seed));
        lines.push(format!(
            "{} pages · depth {} · {} · Sitemaps {}",
            run.limits.max_pages,
            run.limits.max_depth,
            run.rendering.label(),
            if run.sitemaps { "on" } else { "off" }
        ));
    } else {
        lines.push("No run details yet.".into());
    }
    lines.push(format!(
        "{} · {}",
        state.phase.label(),
        run_report_reason(state)
    ));
    let noun = run_view_noun(state);
    lines.push(format!(
        "View {} of {} {} · {} filter{}",
        visible_len(state),
        state.view_counts.total,
        noun,
        state.view().filter.label(),
        if state.view().query.text.trim().is_empty() {
            String::new()
        } else {
            " · search active".to_string()
        }
    ));
    lines.push(sitemap_coverage_line(state));
    col()
        .key("run-details-panel")
        .w_full()
        .gap(3.0)
        .px(GUTTER)
        .pb(8.0)
        .child(
            text(lines.join("\n"))
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .wrap()
                .w_full(),
        )
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
}

fn micro(k: &Kit, value: impl Into<String>) -> El {
    text(value)
        .font(k.ty.small)
        .color(k.c.text_muted)
        .text_case(TextCase::Uppercase)
}

fn dot(color: Color) -> El {
    el().w(5.0).h(5.0).radius(2.5).fill(color).a11y_hidden()
}

fn vrule(k: &Kit) -> El {
    el().w(1.0).h_full().fill(k.c.border_weak).a11y_hidden()
}

fn topbar(k: &Kit, state: &State, counts: &Counts, inset: f32) -> El {
    let tabs = row().gap(2.0).center_y().children(
        [
            Tab::Internal,
            Tab::Titles,
            Tab::Meta,
            Tab::H1,
            Tab::Response,
            Tab::Canonical,
            Tab::Images,
            Tab::Links,
            Tab::Sitemaps,
        ]
        .into_iter()
        .map(|tab| tab_button(k, state, tab, counts)),
    );
    let bar = row()
        .w_full()
        .h(44.0)
        .pl(GUTTER + inset)
        .pr(GUTTER)
        .center_y()
        .gap(4.0)
        .child(icon(&TOAD_MARK, 20.0).key("toad-mark").label("Calm Toad"))
        .child(tabs)
        .child(el().grow(1.0));
    bar.border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
}

fn update_banner(k: &Kit, state: &State) -> Option<El> {
    match &state.update {
        UpdateUi::Available(offered) => {
            let mut actions = vec![
                Button::new("Download")
                    .variant(ButtonVariant::Neutral)
                    .control_size(ControlSize::Regular)
                    .on_press(start_download)
                    .build(k),
                Button::new("Later")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Regular)
                    .on_press(dismiss_update)
                    .build(k),
                Button::new("Skip version")
                    .variant(ButtonVariant::GhostMuted)
                    .control_size(ControlSize::Regular)
                    .help(format!("Do not remind me about {}", offered.version))
                    .on_press(skip_update)
                    .build(k),
            ];
            if let Some(url) = offered.release_url.clone() {
                actions.insert(
                    1,
                    Button::new("Release notes")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Regular)
                        .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                            let _ = tx.open_url(&url);
                        })
                        .build(k),
                );
            }
            Some(banner(
                k,
                format!("Calm Toad {} is available", offered.version),
                if offered.notes.trim().is_empty() {
                    format!("You are running {}.", update::CURRENT_VERSION)
                } else {
                    short_notes(&offered.notes)
                },
                actions,
                None,
            ))
        }
        UpdateUi::Downloading {
            version,
            received,
            total,
        } => {
            let fraction = total
                .filter(|total| *total > 0)
                .map(|total| *received as f32 / total as f32)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            Some(banner(
                k,
                format!("Downloading Calm Toad {version}"),
                format!(
                    "{} of {}",
                    format_bytes(*received),
                    total.map(format_bytes).unwrap_or_else(|| "…".into())
                ),
                vec![
                    Button::new("Cancel")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Regular)
                        .on_press(cancel_download)
                        .build(k),
                ],
                Some(fraction),
            ))
        }
        UpdateUi::Ready(downloaded) => Some(banner(
            k,
            format!("Calm Toad {} is ready", downloaded.version),
            if state.phase.active() {
                "Stop the crawl before restarting to install.".into()
            } else {
                "Restarting will clear the current crawl results.".into()
            },
            vec![
                Button::new("Restart and install")
                    .variant(ButtonVariant::Neutral)
                    .control_size(ControlSize::Regular)
                    .disabled(state.phase.active())
                    .on_press_tx(start_install)
                    .build(k),
            ],
            None,
        )),
        UpdateUi::Failed(error) => Some(banner(
            k,
            "Update did not finish".into(),
            error.clone(),
            vec![
                Button::new("Check again")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Regular)
                    .on_press(retry_update)
                    .build(k),
                Button::new("Dismiss")
                    .variant(ButtonVariant::GhostMuted)
                    .control_size(ControlSize::Regular)
                    .on_press(dismiss_update)
                    .build(k),
            ],
            None,
        )),
        _ => None,
    }
}

fn banner(k: &Kit, title: String, detail: String, actions: Vec<El>, progress: Option<f32>) -> El {
    let mut body = col().w_full().gap(7.0).child(
        row()
            .w_full()
            .gap(12.0)
            .center_y()
            .child(
                col()
                    .gap(3.0)
                    .min_w(0.0)
                    .child(text(title).font(k.ty.text_small).color(k.c.text_base))
                    .child(
                        text(detail)
                            .font(k.ty.small)
                            .color(k.c.text_muted)
                            .wrap()
                            .w_full(),
                    ),
            )
            .child(el().grow(1.0))
            .children(actions),
    );
    if let Some(fraction) = progress {
        body = body.child(
            row()
                .w_full()
                .h(3.0)
                .fill(k.c.border_weak)
                .child(el().h_full().w_frac(fraction).fill(k.c.text_base))
                .child(el().grow(1.0)),
        );
    }
    col().w_full().px(GUTTER).pb(8.0).child(
        body.py(8.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak),
    )
}

fn short_notes(notes: &str) -> String {
    let line = notes
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let summary: String = line.chars().take(140).collect();
    if line.chars().count() > 140 {
        format!("{summary}…")
    } else {
        summary
    }
}

fn format_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn failed(state: &State) -> bool {
    state.pages.is_empty()
        && matches!(
            state.phase,
            RunPhase::Failed | RunPhase::Blocked | RunPhase::Stopped
        )
}

fn command_bar(k: &Kit, state: &State) -> El {
    let bar = row()
        .w_full()
        .h(40.0)
        .radius(k.radius.lg)
        .fill(k.c.bg_base)
        .border(1.0, k.c.border_base)
        .clip_rounded(k.radius.lg)
        .focus_within_ring(1.0, 0.0, k.c.border_strong)
        .child(url_field(k, state))
        .child(vrule(k))
        .child(pages_field(k, state))
        .child(vrule(k))
        .child(depth_field(k, state))
        .child(vrule(k))
        .child(rendering_picker(k, state))
        .child(vrule(k))
        .child(sitemaps_toggle(k, state))
        .child(crawl_button(k, state));
    let mut block = col().w_full().px(GUTTER).pt(12.0).pb(8.0).child(bar);
    if state.field_errors.any() {
        block = block.child(
            text(state.field_errors.summary())
                .key("setup-error")
                .font(k.ty.text_xsmall)
                .color(k.c.danger.fg)
                .wrap()
                .w_full()
                .pt(4.0)
                .live_region(rune::LiveRegion::Assertive),
        );
    }
    block
}

fn url_field(k: &Kit, state: &State) -> El {
    rune_kit::TextInput::new(&state.url)
        .plain()
        .height(38.0)
        .text_style(TextStyle::new(12.5, 400.0).lh(18.0))
        .placeholder("https://example.com")
        .label("Site URL")
        .width(None)
        .on_edit(|state: &mut State| &mut state.url)
        .on_change(|state: &mut State, _| state.field_errors.clear())
        .on_submit(|state: &mut State, _| start_crawl(state))
        .invalid(!state.field_errors.url.is_empty())
        .disabled(state.phase.active())
        .build(k)
        .grow(1.0)
        .min_w(0.0)
        .key("url")
}

fn pages_field(k: &Kit, state: &State) -> El {
    row()
        .h_full()
        .px(8.0)
        .gap(4.0)
        .center_y()
        .child(micro(k, "Pages"))
        .child(
            rune_kit::TextInput::new(&state.pages_limit)
                .plain()
                .height(38.0)
                .width(Some(58.0))
                .text_style(k.ty.mono)
                .label("Page limit")
                .on_edit(|state: &mut State| &mut state.pages_limit)
                .on_change(|state: &mut State, _| state.field_errors.clear())
                .on_submit(|state: &mut State, _| start_crawl(state))
                .invalid(!state.field_errors.pages.is_empty())
                .disabled(state.phase.active())
                .build(k)
                .key("pages-limit"),
        )
}

fn depth_field(k: &Kit, state: &State) -> El {
    row()
        .h_full()
        .px(8.0)
        .gap(4.0)
        .center_y()
        .child(micro(k, "Depth"))
        .child(
            rune_kit::TextInput::new(&state.depth_limit)
                .plain()
                .height(38.0)
                .width(Some(36.0))
                .text_style(k.ty.mono)
                .label("Depth limit")
                .on_edit(|state: &mut State| &mut state.depth_limit)
                .on_change(|state: &mut State, _| state.field_errors.clear())
                .on_submit(|state: &mut State, _| start_crawl(state))
                .invalid(!state.field_errors.depth.is_empty())
                .disabled(state.phase.active())
                .build(k)
                .key("depth-limit"),
        )
}

fn rendering_picker(k: &Kit, state: &State) -> El {
    let mut picker = SegmentedControl::new(
        &["HTML", "Browser"],
        Some(match state.rendering {
            RenderMode::Html => 0,
            RenderMode::Browser => 1,
        }),
    )
    .label("Page rendering")
    .fit();
    if state.phase.active() {
        // The running crawl's capture mode was fixed when it started.
        picker = picker
            .segment_state(0, rune_kit::State::Disabled)
            .segment_state(1, rune_kit::State::Disabled);
    }
    row().h_full().px(4.0).center_y().child(
        picker
            .on_pick(|state: &mut State, index, _tx| {
                state.rendering = match index {
                    1 => RenderMode::Browser,
                    _ => RenderMode::Html,
                };
            })
            .build(k)
            .key("rendering"),
    )
    .help("HTML reads the server response. Browser executes scripts in a local Chrome or Chromium before the content checks.")
}

fn browser_strip(k: &Kit, state: &State) -> El {
    let (color, label, help) = match &state.browser.status {
        BrowserStatus::Checking => (
            k.c.text_muted,
            "Finding a browser…".to_string(),
            "Looking for installed Chrome or Chromium.".to_string(),
        ),
        BrowserStatus::Ready { path, version } => {
            let label = version
                .split_once('/')
                .map(|(name, number)| {
                    format!("{name} {}", number.split('.').next().unwrap_or(number))
                })
                .unwrap_or_else(|| version.clone());
            (
                k.c.text_muted,
                label,
                format!("{version}\n{}", path.display()),
            )
        }
        BrowserStatus::Unavailable { reason } => (
            k.c.warning.fg,
            "Chrome or Chromium required".to_string(),
            reason.clone(),
        ),
    };
    let unavailable = matches!(state.browser.status, BrowserStatus::Unavailable { .. });
    let line = row()
        .key("browser-strip")
        .w_full()
        .px(GUTTER)
        .pb(4.0)
        .gap(8.0)
        .center_y()
        .child(dot(color))
        .child(
            text(label)
                .font(k.ty.text_xsmall)
                .color(color)
                .help(help.clone()),
        )
        .child(el().grow(1.0))
        .child(
            Button::new("Choose browser…")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .disabled(state.phase.active())
                .help("Choose a local Chrome or Chromium executable")
                .on_press_tx(choose_browser)
                .build(k)
                .key("choose-browser"),
        );
    if unavailable {
        col().w_full().gap(2.0).child(line).child(
            text(help)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .px(GUTTER)
                .pb(4.0)
                .w_full()
                .wrap()
                .key("browser-error"),
        )
    } else {
        line
    }
}

fn choose_browser(state: &mut State, tx: &mut Tx) {
    // The running crawl owns its browser configuration; changing it mid-run would not affect
    // that crawl and would leave the strip describing a different setup.
    if state.phase.active() {
        return;
    }
    let options = FileDialogOptions {
        title: "Choose a Chrome or Chromium executable".into(),
        ..Default::default()
    };
    tx.open_file(options, |state: &mut State, result, _tx| match result {
        Ok(Some(path)) => {
            if state.phase.active() {
                return;
            }
            state.browser = BrowserChoice {
                path: Some(path.clone()),
                status: BrowserStatus::Checking,
            };
            state.browser_probe = Some(probe_browser(Some(path)));
            state.rendering = RenderMode::Browser;
            state.message = "Checking the chosen browser…".into();
        }
        Ok(None) => {}
        Err(error) => {
            state.message = format!("Could not open the file chooser: {error}");
        }
    });
}

fn sitemaps_toggle(k: &Kit, state: &State) -> El {
    row().h_full().px(12.0).center_y().child(
        Checkbox::new(state.use_sitemaps)
            .label("Sitemaps")
            .disabled(state.phase.active())
            .on_toggle(|state: &mut State| state.use_sitemaps ^= true)
            .build(k)
            .help("Add sitemap-listed URLs to the crawl; robots.txt rules are always respected")
            .key("use-sitemaps"),
    )
}

fn crawl_button(k: &Kit, state: &State) -> El {
    let font = TextStyle::new(10.5, 500.0).mono().lh(14.0).tracking(1.2);
    if state.phase == RunPhase::Stopping {
        return Button::new("Stopping")
            .variant(ButtonVariant::Danger)
            .disabled(true)
            .control_size(ControlSize::ExtraLarge)
            .label_with(move |_, ink| {
                text("STOPPING")
                    .font(font)
                    .color(ink)
                    .text_case(TextCase::Uppercase)
            })
            .build(k)
            .h_full()
            .key("crawl");
    }
    if state.phase.active() {
        Button::new("Stop")
            .variant(ButtonVariant::Danger)
            .control_size(ControlSize::ExtraLarge)
            .label_with(move |_, ink| {
                text("STOP")
                    .font(font)
                    .color(ink)
                    .text_case(TextCase::Uppercase)
            })
            .on_press(stop_crawl)
            .build(k)
            .h_full()
            .key("crawl")
    } else {
        let blocked = state.rendering == RenderMode::Browser && !state.browser.status.is_ready();
        Button::new("Crawl")
            .variant(ButtonVariant::Submit)
            .disabled(blocked)
            .control_size(ControlSize::ExtraLarge)
            .label_with(move |_, ink| {
                text("CRAWL")
                    .font(font)
                    .color(ink)
                    .text_case(TextCase::Uppercase)
            })
            .on_press(start_crawl)
            .build(k)
            .h_full()
            .key("crawl")
    }
}

fn tab_button(k: &Kit, state: &State, tab: Tab, counts: &Counts) -> El {
    let selected = state.tab == tab;
    let count = tab_count(tab, counts, &state.sitemap);
    let count_ink = k.c.text_muted;
    let label_ink = if selected {
        k.c.text_base
    } else {
        k.c.text_muted
    };
    let small = k.ty.small;
    Button::new(tab_label(tab, counts, &state.sitemap))
        .control_size(ControlSize::Small)
        .variant(if selected {
            ButtonVariant::Neutral
        } else {
            ButtonVariant::Ghost
        })
        .label_with(move |_, _| {
            row()
                .gap(6.0)
                .center_y()
                .child(
                    text(tab_short(tab))
                        .font(small)
                        .color(label_ink)
                        .text_case(TextCase::Uppercase),
                )
                .child(
                    text(count.to_string())
                        .font(small.tracking(0.0))
                        .color(count_ink),
                )
        })
        .on_press(move |state: &mut State| {
            state.tab = tab;
            state.relation_pages = RelationPages::default();
            state.selection_epoch = state.selection_epoch.wrapping_add(1);
            let (column, dir) = state.view().sort.clone();
            state.table.set_sort(column, dir);
            sort_pages(state);
        })
        .help(tab_help(tab, counts, &state.sitemap))
        .build(k)
        .key(tab_key(tab))
}

fn progress_line(k: &Kit, state: &State) -> El {
    let fraction = (state.pages.len() as f32 / state.max_pages.max(1) as f32).clamp(0.0, 1.0);
    row()
        .w_full()
        .h(2.0)
        .fill(k.c.border_weak)
        .child(el().h_full().w_frac(fraction).fill(k.c.text_base))
        .child(el().grow(1.0))
        .key("progress")
}

fn section_header(k: &Kit, state: &State) -> El {
    if state.tab == Tab::Sitemaps {
        return sitemap_section_header(k, state);
    }
    let total = state.view_counts.total;
    let shown = visible_len(state);
    let query_active = !state.view().query.text.trim().is_empty();
    let mut summary = format!("{shown} of {total} URLs");
    if query_active {
        summary.push_str(" shown");
    }
    if state.view_counts.findings > 0 || state.view_counts.incomplete > 0 {
        summary.push_str(&format!(
            " · {} findings · {} incomplete in view",
            state.view_counts.findings, state.view_counts.incomplete
        ));
    }
    if let Some(run) = &state.run {
        summary.push_str(&format!(" · {}", run.host));
    }
    row()
        .w_full()
        .h(40.0)
        .px(GUTTER)
        .center_y()
        .gap(10.0)
        .child(
            text(summary)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .grow(1.0)
                .min_w(60.0)
                .truncate()
                .help(format!(
                    "{} · {} · {}",
                    active_title(state),
                    state.view().filter.help(),
                    if query_active {
                        "Search matches URL, title, meta description, H1, canonical, content type and finding text"
                    } else {
                        "No search query"
                    }
                )),
        )
        .child(run_details_button(k, state))
        .children(search_field(k, state))
        .child(check_filter(k, state))
        .child(export_button(k, state, shown))
        .child(copy_urls_button(k, state, shown))
        .child(details_toggle(k, state))
}

fn run_report_reason(state: &State) -> String {
    if state.phase.active() {
        state.message.clone()
    } else {
        state
            .run_outcome
            .clone()
            .unwrap_or_else(|| state.message.clone())
    }
}

fn run_view_noun(state: &State) -> &'static str {
    match state.tab {
        Tab::Sitemaps => match state.sitemap_view {
            SitemapView::Urls => "URLs",
            SitemapView::Files => "files",
        },
        _ => "URLs",
    }
}

fn run_sitemaps(state: &State) -> bool {
    state
        .run
        .as_ref()
        .map(|run| run.sitemaps)
        .unwrap_or(state.use_sitemaps)
}

fn run_details_button(k: &Kit, state: &State) -> El {
    Button::new("Run")
        .variant(if state.run_details_open {
            ButtonVariant::Neutral
        } else {
            ButtonVariant::Ghost
        })
        .control_size(ControlSize::Small)
        .disabled(state.run.is_none())
        .expanded(state.run_details_open)
        .help("Show this run's seed, limits, capture mode and coverage")
        .on_press(|state: &mut State| state.run_details_open = !state.run_details_open)
        .build(k)
        .key("run-details")
}

fn search_field(k: &Kit, state: &State) -> Option<El> {
    let has_data = if state.tab == Tab::Sitemaps {
        match state.sitemap_view {
            SitemapView::Urls => !state.sitemap.urls.is_empty(),
            SitemapView::Files => !state.sitemap.files.is_empty(),
        }
    } else {
        !state.pages.is_empty()
    };
    if !has_data {
        return None;
    }
    Some(
        rune_kit::TextInput::new(&state.view().query)
            .plain()
            .height(28.0)
            .width(Some(160.0))
            .leading_icon(k.glyph.search)
            .leading_icon_size(14.0)
            .text_style(TextStyle::new(11.5, 400.0).lh(16.0))
            .placeholder("Search results")
            .label("Search results")
            .on_edit(|state: &mut State| &mut state.view_mut().query)
            .on_change(|state: &mut State, _| refresh_visible_forced(state))
            .on_key(|state: &mut State, event: &KeyEvent, _| {
                if matches!(event.key, Key::Escape)
                    && !state.view().query.text.is_empty()
                {
                    state.view_mut().query.replace_text("");
                    refresh_visible_forced(state);
                    true
                } else {
                    false
                }
            })
            .build(k)
            .key("result-search")
            .help("Literal, case-insensitive search over URL, title, meta description, H1, canonical, content type and finding text in this view."),
    )
}

fn check_filter(k: &Kit, state: &State) -> El {
    let button = |filter: CheckFilter, key: &'static str| {
        Button::new(filter.label())
            .variant(if state.view().filter == filter {
                ButtonVariant::Neutral
            } else {
                ButtonVariant::Ghost
            })
            .control_size(ControlSize::Small)
            .help(filter.help())
            .on_press(move |state: &mut State| {
                state.view_mut().filter = filter;
                refresh_visible_forced(state);
            })
            .build(k)
            .key(key)
    };
    row()
        .h_full()
        .center_y()
        .gap(1.0)
        .child(button(CheckFilter::All, "filter-all"))
        .child(button(CheckFilter::Findings, "filter-findings"))
        .child(button(CheckFilter::Incomplete, "filter-incomplete"))
}

fn sitemap_section_header(k: &Kit, state: &State) -> El {
    let total = state.view_counts.total;
    let shown = visible_len(state);
    let query_active = !state.view().query.text.trim().is_empty();
    let view = |label: &'static str, view: SitemapView, key: &'static str| {
        Button::new(label)
            .variant(if state.sitemap_view == view {
                ButtonVariant::Neutral
            } else {
                ButtonVariant::Ghost
            })
            .control_size(ControlSize::Small)
            .on_press(move |state: &mut State| {
                state.sitemap_view = view;
                let (column, dir) = state.view().sort.clone();
                state.table.set_sort(column, dir);
                sort_sitemap(state);
            })
            .build(k)
            .key(key)
    };
    let noun = match state.sitemap_view {
        SitemapView::Urls => "URLs",
        SitemapView::Files => "files",
    };
    let mut summary = format!("{shown} of {total} {noun}");
    if state.view_counts.findings > 0 || state.view_counts.incomplete > 0 {
        summary.push_str(&format!(
            " · {} findings · {} incomplete",
            state.view_counts.findings, state.view_counts.incomplete
        ));
    }
    if state.sitemap.partial.is_some() {
        summary.push_str(" · Partial");
    }
    row()
        .w_full()
        .h(40.0)
        .px(GUTTER)
        .center_y()
        .gap(6.0)
        .child(
            row()
                .gap(2.0)
                .child(view("URLs", SitemapView::Urls, "sitemap-urls"))
                .child(view("Files", SitemapView::Files, "sitemap-files")),
        )
        .child(
            text(summary)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .grow(1.0)
                .min_w(40.0)
                .truncate()
                .help(format!(
                    "{} · {}{}",
                    sitemap_summary(state),
                    state.view().filter.help(),
                    if query_active {
                        " · search active"
                    } else {
                        ""
                    }
                )),
        )
        .child(run_details_button(k, state))
        .children(search_field(k, state))
        .child(check_filter(k, state))
        .child(export_button(k, state, shown))
        .child(copy_urls_button(k, state, shown))
        .child(details_toggle(k, state))
}

fn sitemap_coverage_line(state: &State) -> String {
    if !run_sitemaps(state) {
        return "Sitemaps: off for this run".into();
    }
    if state.sitemap_discovering || (state.phase.active() && !state.sitemap.discovery_done) {
        let summary = state.sitemap.summary();
        return format!(
            "Sitemaps: discovery pending · {} files · {} listed so far",
            summary.files, summary.listed
        );
    }
    if !state.sitemap.discovery_done {
        return match state.phase {
            RunPhase::Stopped => "Sitemaps: discovery stopped with the crawl".into(),
            RunPhase::Limited => "Sitemaps: discovery did not finish before the page limit".into(),
            RunPhase::Failed | RunPhase::Blocked => "Sitemaps: discovery did not run".into(),
            _ => "Sitemaps: not read for this run".into(),
        };
    }
    let summary = state.sitemap.summary();
    let mut parts = vec![format!(
        "Sitemaps: {} {} · {} listed · {} checked",
        summary.files,
        if summary.files == 1 { "file" } else { "files" },
        summary.listed,
        summary.checked
    )];
    for (count, label) in [
        (summary.blocked, "blocked"),
        (summary.skipped, "skipped"),
        (summary.not_checked, "not checked"),
        (summary.pending, "pending"),
        (summary.file_errors, "file errors"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    if let Some(partial) = &state.sitemap.partial {
        parts.push(format!("partial: {partial}"));
    }
    parts.join(" · ")
}

fn sitemap_summary(state: &State) -> String {
    let summary = state.sitemap.summary();
    if !state.sitemap.discovery_done {
        if !state.phase.active() && !state.sitemap_discovering {
            return "Sitemaps not read".to_string();
        }
        return if state.sitemap.files.is_empty() {
            "Reading sitemaps".to_string()
        } else {
            format!(
                "Reading sitemaps · {} files · {} listed",
                summary.files, summary.listed
            )
        };
    }
    if !run_sitemaps(state) {
        return "Sitemap discovery is off".to_string();
    }
    if summary.files == 0 {
        return match &state.sitemap.partial {
            Some(partial) => format!("Sitemaps incomplete · {partial}"),
            None => "No sitemaps found".to_string(),
        };
    }
    let mut parts = vec![
        format!(
            "{} {}",
            summary.files,
            if summary.files == 1 { "file" } else { "files" }
        ),
        format!("{} listed", summary.listed),
        format!("{} checked", summary.checked),
    ];
    for (count, label) in [
        (summary.blocked, "blocked"),
        (summary.skipped, "skipped"),
        (summary.not_checked, "not checked"),
        (summary.pending, "pending"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    if summary.file_errors > 0 {
        parts.push(format!("{} file errors", summary.file_errors));
    }
    if let Some(partial) = &state.sitemap.partial {
        parts.push(partial.clone());
    }
    parts.join(" · ")
}

fn copy_urls_button(k: &Kit, state: &State, shown: usize) -> El {
    IconButton::new(&rune_kit::glyphs::COPY)
        .variant(IconButtonVariant::GhostMuted)
        .label(format!("Copy {shown} visible URLs"))
        .help(format!(
            "Copy the {shown} visible URLs from {} in the current sort order",
            active_title(state)
        ))
        .disabled(shown == 0)
        .on_press_tx(copy_visible_urls)
        .build(k)
        .key("copy-urls")
}

fn visible_ids(state: &State) -> Vec<String> {
    (0..visible_len(state))
        .filter_map(|row| visible_id(state, row))
        .collect()
}

fn copy_visible_urls(state: &mut State, tx: &mut Tx) {
    let urls = visible_ids(state);
    if urls.is_empty() {
        return;
    }
    let count = urls.len();
    tx.copy(urls.join("\n"));
    state.action_notice = Some(format!("Copied {count} visible URLs"));
}

fn export_button(k: &Kit, state: &State, shown: usize) -> El {
    Button::new("Export")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .help(format!(
            "Save the {shown} visible rows from {} in the current sort order",
            active_title(state)
        ))
        .disabled(state.phase.active() || shown == 0)
        .on_press_tx(export_csv)
        .build(k)
        .key("export-csv")
}

fn details_toggle(k: &Kit, state: &State) -> El {
    IconButton::new(&DETAILS_PANEL)
        .variant(IconButtonVariant::GhostMuted)
        .label(if state.details_open {
            "Hide page details"
        } else {
            "Show page details"
        })
        .help(if state.details_open {
            "Hide page details"
        } else {
            "Show page details"
        })
        .expanded(state.details_open)
        .on_press(|state: &mut State| state.details_open = !state.details_open)
        .build(k)
        .key("details-toggle")
}

fn export_csv(state: &mut State, tx: &mut Tx) {
    if state.phase.active() {
        return;
    }
    state.action_notice = None;
    if state.tab == Tab::Sitemaps {
        let view = state.sitemap_view;
        let headers = sitemap_export_headers(view);
        let rows = match view {
            SitemapView::Urls => state
                .sitemap_visible
                .iter()
                .filter_map(|index| state.sitemap.urls.get(*index))
                .map(sitemap_url_row)
                .collect::<Vec<_>>(),
            SitemapView::Files => state
                .sitemap_file_visible
                .iter()
                .filter_map(|index| state.sitemap.files.get(*index))
                .map(sitemap_file_row)
                .collect::<Vec<_>>(),
        };
        if rows.is_empty() {
            return;
        }
        let (title, slug) = match view {
            SitemapView::Urls => ("Sitemap URLs", "sitemap-urls"),
            SitemapView::Files => ("Sitemap files", "sitemap-files"),
        };
        let scope = export_scope(state, rows.len(), title);
        save_csv(state, tx, headers, rows, title, slug, scope);
        return;
    }
    if state.visible.is_empty() {
        return;
    }

    let tab = state.tab;
    let headers = export_headers(tab);
    let rows = state
        .visible
        .iter()
        .filter_map(|index| state.pages.get(*index))
        .map(|page| export_row(tab, page, &state.audit))
        .collect::<Vec<_>>();
    let scope = export_scope(state, rows.len(), active_title(state));
    save_csv(
        state,
        tx,
        headers,
        rows,
        active_title(state),
        tab_slug(tab),
        scope,
    );
}

fn export_scope(state: &State, rows: usize, title: &str) -> String {
    if !state.view().query.text.trim().is_empty() || state.view().filter != CheckFilter::All {
        format!("{rows} filtered visible {title} rows")
    } else {
        format!("{rows} visible {title} rows")
    }
}

fn save_csv(
    state: &mut State,
    tx: &mut Tx,
    headers: &'static [&'static str],
    rows: Vec<Vec<String>>,
    _title: &'static str,
    slug: &'static str,
    scope: String,
) {
    let csv = calmtoad::export::to_csv(headers, &rows);
    let row_count = rows.len();
    let options = FileDialogOptions {
        title: format!("Export {scope} as CSV"),
        file_name: Some(format!("calmtoad-{slug}.csv")),
        ..Default::default()
    }
    .filter("CSV files", &["csv"]);

    state.export_notice = None;
    tx.save_file(
        options,
        move |state: &mut State, result, _tx| match result {
            Ok(Some(path)) => {
                let path = csv_path(path);
                match std::fs::write(&path, csv.as_bytes()) {
                    Ok(()) => {
                        let name = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("CSV file");
                        state.export_notice =
                            Some(Ok(format!("Exported {row_count} visible rows to {name}")));
                    }
                    Err(error) => {
                        state.export_notice = Some(Err(format!("Could not save CSV: {error}")));
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                state.export_notice = Some(Err(format!("Could not open save dialog: {error}")));
            }
        },
    );
}

fn csv_path(mut path: std::path::PathBuf) -> std::path::PathBuf {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
    {
        path.set_extension("csv");
    }
    path
}

fn tab_slug(tab: Tab) -> &'static str {
    match tab {
        Tab::Internal => "internal-urls",
        Tab::Titles => "titles",
        Tab::Meta => "meta-descriptions",
        Tab::H1 => "h1",
        Tab::Response => "response-codes",
        Tab::Canonical => "canonicals",
        Tab::Images => "images",
        Tab::Links => "links",
        Tab::Sitemaps => "sitemaps",
    }
}

fn export_headers(tab: Tab) -> &'static [&'static str] {
    match tab {
        Tab::Internal => &[
            "ADDRESS",
            "STATUS",
            "INDEXABILITY",
            "CAPTURE",
            "WORDS",
            "REASONS",
            "EVIDENCE",
        ],
        Tab::Titles => &["ADDRESS", "TITLE", "LENGTH", "ISSUE"],
        Tab::Meta => &["ADDRESS", "META DESCRIPTION", "LENGTH", "ISSUE"],
        Tab::H1 => &["ADDRESS", "H1", "COUNT", "ISSUE"],
        Tab::Response => &["ADDRESS", "STATUS", "CONTENT TYPE", "ISSUE"],
        Tab::Canonical => &["ADDRESS", "CANONICAL", "ISSUE"],
        Tab::Images => &["ADDRESS", "IMAGES", "NO ALT", "ISSUE"],
        Tab::Links => &["ADDRESS", "INBOUND", "OUTBOUND", "ISSUE"],
        Tab::Sitemaps => sitemap_export_headers(SitemapView::Urls),
    }
}

fn sitemap_export_headers(view: SitemapView) -> &'static [&'static str] {
    match view {
        SitemapView::Urls => &["ADDRESS", "STATE", "STATUS", "SOURCE", "CAPTURE", "ISSUE"],
        SitemapView::Files => &["ADDRESS", "ORIGIN", "STATE", "STATUS", "URLS", "ISSUE"],
    }
}

fn sitemap_url_row(entry: &SitemapUrl) -> Vec<String> {
    vec![
        entry.url.clone(),
        entry.state.label().to_string(),
        if entry.status == 0 {
            "—".into()
        } else {
            entry.status.to_string()
        },
        entry.sources.join(" · "),
        entry.assessment.capture.csv_label(),
        if entry.issue.is_empty() {
            "OK".into()
        } else {
            entry.issue.clone()
        },
    ]
}

fn sitemap_file_row(file: &SitemapFile) -> Vec<String> {
    vec![
        file.url.clone(),
        file.origin.label().to_string(),
        file.state.label().to_string(),
        if file.status == 0 {
            "—".into()
        } else {
            file.status.to_string()
        },
        file.urls.to_string(),
        if file.issue.is_empty() {
            "OK".into()
        } else {
            file.issue.clone()
        },
    ]
}

/// What a row's Issue cell can honestly say.
enum RowIssue {
    /// The capture did not finish the check, so no claim is made.
    Unclaimed,
    /// The completed check's issue text; empty means OK.
    Checked(String),
}

/// One rule shared by the on-screen row and the CSV so neither can drift.
fn row_issue(tab: Tab, page: &Page, audit: &Audit) -> RowIssue {
    if !auditable(page) {
        if tab == Tab::Links {
            // Keep a broken target the crawl actually observed; otherwise do not claim a
            // healthy link check from an incomplete capture.
            let issue = issue_for(tab, page, audit);
            return if issue.is_empty() {
                RowIssue::Unclaimed
            } else {
                RowIssue::Checked(issue)
            };
        }
        if matches!(
            tab,
            Tab::Titles | Tab::Meta | Tab::H1 | Tab::Canonical | Tab::Images
        ) {
            return RowIssue::Unclaimed;
        }
    }
    RowIssue::Checked(issue_for(tab, page, audit))
}

fn export_row(tab: Tab, page: &Page, audit: &Audit) -> Vec<String> {
    let issue = || match row_issue(tab, page, audit) {
        RowIssue::Unclaimed => "—".into(),
        RowIssue::Checked(issue) => {
            if issue.is_empty() {
                "OK".into()
            } else {
                issue
            }
        }
    };
    let count = |value: usize| {
        if auditable(page) {
            value.to_string()
        } else {
            "—".into()
        }
    };
    let mut row = vec![page.url.clone()];
    match tab {
        Tab::Internal => {
            row.push(if page.status == 0 {
                "ERR".into()
            } else {
                page.status.to_string()
            });
            row.push(page.indexability.label.text().to_string());
            row.push(page.indexability.capture.csv_label());
            row.push(if auditable(page) {
                page.words.to_string()
            } else {
                "—".into()
            });
            let reasons = page.indexability.all_reasons_text();
            row.push(if reasons.is_empty() {
                "—".into()
            } else {
                reasons
            });
            row.push(page.indexability.evidence_text());
        }
        Tab::Titles => {
            row.push(dash(&page.title));
            row.push(if auditable(page) || !page.title.is_empty() {
                page.title.chars().count().to_string()
            } else {
                "—".into()
            });
            row.push(issue());
        }
        Tab::Meta => {
            row.push(dash(&page.meta_description));
            row.push(if auditable(page) || !page.meta_description.is_empty() {
                page.meta_description.chars().count().to_string()
            } else {
                "—".into()
            });
            row.push(issue());
        }
        Tab::H1 => {
            row.push(dash(&page.h1()));
            row.push(count(page.h1s.len()));
            row.push(issue());
        }
        Tab::Response => {
            row.push(if page.status == 0 {
                "ERR".into()
            } else {
                page.status.to_string()
            });
            row.push(mime(&page.content_type));
            row.push(issue());
        }
        Tab::Canonical => {
            row.push(dash(&page.canonical));
            row.push(issue());
        }
        Tab::Images => {
            row.push(count(page.images));
            row.push(count(page.images_missing_alt));
            row.push(issue());
        }
        Tab::Links => {
            row.push(audit.inbound(page).to_string());
            row.push(count(page.links.len()));
            row.push(issue());
        }
        Tab::Sitemaps => {}
    }
    row
}

fn results(k: &Kit, state: &State) -> El {
    let frame = row().key("frame").w_full().grow(1.0).min_h(0.0);
    let has_data = if state.tab == Tab::Sitemaps {
        match state.sitemap_view {
            SitemapView::Urls => !state.sitemap.urls.is_empty(),
            SitemapView::Files => !state.sitemap.files.is_empty(),
        }
    } else {
        !state.pages.is_empty()
    };
    if !has_data {
        return frame.child(empty(k, state));
    }
    if visible_len(state) == 0 {
        return frame.child(filtered_empty(k, state));
    }
    let frame = frame.child(table(k, state).grow(1.0).min_w(0.0).h_full());
    if state.details_open {
        frame.child(detail(k, state).w(320.0).h_full())
    } else {
        frame
    }
}

fn filtered_empty(k: &Kit, state: &State) -> El {
    let query = state.view().query.text.trim();
    let filter = state.view().filter;
    let (title, copy) = if !query.is_empty() {
        let filter_note = if filter == CheckFilter::All {
            String::new()
        } else {
            format!(" with the {} filter", filter.label())
        };
        (
            "No matches".to_string(),
            format!(
                "No {} match “{query}”{filter_note}. Search covers URL, title, meta description, H1, canonical, content type and finding text.",
                active_title(state).to_lowercase()
            ),
        )
    } else if filter == CheckFilter::Findings {
        (
            "No findings in this view".to_string(),
            if state.view_counts.incomplete > 0 {
                format!(
                    "No known finding in this view. {} {} still incomplete, so this is not proof the site is clean.",
                    state.view_counts.incomplete,
                    if state.view_counts.incomplete == 1 {
                        "check is"
                    } else {
                        "checks are"
                    }
                )
            } else {
                "Every URL in this view has a completed check and no known finding here. This view does not cover checks outside this tab.".to_string()
            },
        )
    } else if filter == CheckFilter::Incomplete {
        (
            "Nothing incomplete in this view".to_string(),
            "Every URL in this view has a completed capture and check. Findings may still exist under Findings.".to_string(),
        )
    } else {
        (
            "No rows".to_string(),
            "This view has no rows to show.".to_string(),
        )
    };
    let mut actions = row().gap(8.0);
    if !query.is_empty() {
        actions = actions.child(
            Button::new("Clear search")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(|state: &mut State| {
                    state.view_mut().query.replace_text("");
                    refresh_visible_forced(state);
                })
                .build(k)
                .key("clear-search"),
        );
    }
    if filter != CheckFilter::All {
        actions = actions.child(
            Button::new("Show all")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(|state: &mut State| {
                    state.view_mut().filter = CheckFilter::All;
                    refresh_visible_forced(state);
                })
                .build(k)
                .key("clear-filter"),
        );
    }
    col()
        .w_full()
        .h_full()
        .center()
        .gap(8.0)
        .child(
            text(title)
                .font(TextStyle::new(13.0, 500.0).mono().tracking(1.2))
                .color(k.c.text_base)
                .text_case(TextCase::Uppercase)
                .key("empty-title"),
        )
        .child(
            text(copy)
                .font(k.ty.text_small)
                .color(k.c.text_muted)
                .w_full()
                .px(32.0)
                .wrap()
                .align(Align::Center)
                .key("empty-copy"),
        )
        .child(actions)
}

fn empty(k: &Kit, state: &State) -> El {
    let (title, copy) = if state.tab == Tab::Sitemaps {
        sitemap_empty(state)
    } else if state.phase == RunPhase::Preparing {
        (
            "Preparing".to_string(),
            "Reading robots.txt and sitemaps before the first page.".to_string(),
        )
    } else if state.phase == RunPhase::Crawling || state.phase == RunPhase::Stopping {
        (
            "Fetching pages".to_string(),
            "Results appear as pages are checked.".to_string(),
        )
    } else if state.rendering == RenderMode::Browser && !state.browser.status.is_ready() {
        (
            "Choose a browser".to_string(),
            "Use Chrome or Chromium, or switch to HTML.".to_string(),
        )
    } else if state.phase == RunPhase::Ready {
        (
            "Crawl a site".to_string(),
            "Enter a URL above to begin.".to_string(),
        )
    } else if state.phase == RunPhase::Blocked {
        ("Blocked".to_string(), state.message.clone())
    } else if state.phase == RunPhase::Limited {
        ("Page limit".to_string(), state.message.clone())
    } else if state.phase == RunPhase::Stopped {
        ("Stopped".to_string(), state.message.clone())
    } else {
        ("Couldn’t crawl".to_string(), state.message.clone())
    };
    col()
        .w_full()
        .h_full()
        .center()
        .gap(8.0)
        .child(
            text(title)
                .font(TextStyle::new(13.0, 500.0).mono().tracking(1.2))
                .color(k.c.text_base)
                .text_case(TextCase::Uppercase)
                .key("empty-title"),
        )
        .child(
            text(copy)
                .font(k.ty.text_small)
                .color(k.c.text_muted)
                .w_full()
                .px(32.0)
                .wrap()
                .align(Align::Center)
                .key("empty-copy"),
        )
}

fn sitemap_empty(state: &State) -> (String, String) {
    if !run_sitemaps(state) {
        return (
            "Sitemap discovery is off".into(),
            "This crawl still followed page links. Turn on Sitemaps and crawl again to include URLs listed in sitemap files.".into(),
        );
    }
    if state.phase.active() || state.sitemap_discovering {
        return (
            "Reading sitemaps".into(),
            "Checking robots.txt and the sitemap files this site serves.".into(),
        );
    }
    if !state.sitemap.discovery_done {
        return (
            "Sitemaps not read".into(),
            "Sitemap discovery did not run for this crawl.".into(),
        );
    }
    if state.sitemap.files.is_empty() {
        if let Some(partial) = &state.sitemap.partial {
            return (
                "Sitemaps stopped".into(),
                format!("Reading sitemaps did not finish: {partial}."),
            );
        }
        return if state.pages.is_empty() && failed(state) {
            ("Couldn’t crawl".into(), state.message.clone())
        } else {
            (
                "No sitemaps found".into(),
                "This site did not declare one, and /sitemap.xml and /sitemap_index.xml are missing."
                    .into(),
            )
        };
    }
    let failed_files = state
        .sitemap
        .files
        .iter()
        .filter(|file| file.state == FileState::Error)
        .count();
    if failed_files > 0 {
        return (
            "No URLs listed".into(),
            format!(
                "{} sitemap {} failed. Switch to Files to see why.",
                failed_files,
                if failed_files == 1 { "file" } else { "files" }
            ),
        );
    }
    let skipped_files = state
        .sitemap
        .files
        .iter()
        .filter(|file| file.state == FileState::Skipped)
        .count();
    if skipped_files > 0 {
        return (
            "No URLs listed".into(),
            format!(
                "{} sitemap {} skipped. Switch to Files to see why.",
                skipped_files,
                if skipped_files == 1 {
                    "file was"
                } else {
                    "files were"
                }
            ),
        );
    }
    (
        "No URLs listed".into(),
        "The sitemap files were read but did not list any page URLs.".into(),
    )
}

fn table(k: &Kit, state: &State) -> El {
    if state.tab == Tab::Sitemaps {
        return sitemap_table(k, state);
    }
    let kit = k.clone();
    let tab = state.tab;
    let mut table = Table::new(
        |state: &State| &state.table,
        |state: &mut State| &mut state.table,
        |state: &State| visible_len(state),
        |state: &State, row| Cow::Borrowed(visible_id_ref(state, row).unwrap_or("")),
    )
    .name(tab_title(tab))
    .style(table_style(k))
    .zebra(false)
    .on_sort(|state: &mut State, column, dir, _| set_page_sort(state, column, dir));
    table = table.on_select(|state: &mut State, id, _tx| {
        sync_relation_selection(state, Some(id));
        state.view_mut().selected = Some(id.to_string());
        state.details_open = true;
    });
    table = match tab {
        Tab::Internal => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(2.4)
                    .min(180.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("status", "HTTP")
                    .width(76.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(
                TableColumn::new("indexability", "INDEXABILITY")
                    .width(200.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("words", "WORDS")
                    .width(88.0)
                    .align(Align::End)
                    .sortable(),
            ),
        Tab::Titles => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(1.2)
                    .min(140.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("title", "TITLE")
                    .flex(2.0)
                    .min(160.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("len", "LENGTH")
                    .width(96.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(TableColumn::new("issue", "ISSUE").width(180.0).sortable()),
        Tab::Meta => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(1.1)
                    .min(120.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("meta", "META DESCRIPTION")
                    .flex(2.2)
                    .min(160.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("len", "LENGTH")
                    .width(96.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(TableColumn::new("issue", "ISSUE").width(180.0).sortable()),
        Tab::H1 => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(1.4)
                    .min(140.0)
                    .sortable(),
            )
            .column(TableColumn::new("h1", "H1").flex(2.0).min(140.0).sortable())
            .column(
                TableColumn::new("count", "COUNT")
                    .width(96.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(TableColumn::new("issue", "ISSUE").width(150.0).sortable()),
        Tab::Response => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(2.0)
                    .min(160.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("status", "HTTP")
                    .width(76.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(
                TableColumn::new("type", "CONTENT TYPE")
                    .flex(1.0)
                    .min(120.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("issue", "ISSUE")
                    .flex(1.2)
                    .min(140.0)
                    .sortable(),
            ),
        Tab::Canonical => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(1.5)
                    .min(150.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("canonical", "CANONICAL")
                    .flex(1.6)
                    .min(150.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("issue", "ISSUE")
                    .flex(1.2)
                    .min(140.0)
                    .sortable(),
            ),
        Tab::Images => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(2.0)
                    .min(160.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("images", "IMAGES")
                    .width(96.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(
                TableColumn::new("missing_alt", "NO ALT")
                    .width(96.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(
                TableColumn::new("issue", "ISSUE")
                    .flex(1.2)
                    .min(160.0)
                    .sortable(),
            ),
        Tab::Links => table
            .column(
                TableColumn::new("url", "ADDRESS")
                    .flex(2.0)
                    .min(160.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("inbound", "INBOUND")
                    .width(100.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(
                TableColumn::new("outbound", "OUTBOUND")
                    .width(104.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(
                TableColumn::new("issue", "ISSUE")
                    .flex(1.4)
                    .min(160.0)
                    .sortable(),
            ),
        Tab::Sitemaps => table,
    };
    table
        .cell(move |state: &State, row, column| {
            let Some(page) = state.pages.get(visible_index(state, row)) else {
                return Cell::text("");
            };
            cell(&kit, state.tab, page, &state.audit, column)
        })
        .build(state, k)
}

fn sitemap_table(k: &Kit, state: &State) -> El {
    let kit = k.clone();
    match state.sitemap_view {
        SitemapView::Urls => Table::new(
            |state: &State| &state.table,
            |state: &mut State| &mut state.table,
            |state: &State| state.sitemap_visible.len(),
            |state: &State, row| Cow::Borrowed(visible_id_ref(state, row).unwrap_or("")),
        )
        .name("Sitemap URLs")
        .style(table_style(k))
        .zebra(false)
        .column(
            TableColumn::new("url", "ADDRESS")
                .flex(2.4)
                .min(180.0)
                .sortable(),
        )
        .column(TableColumn::new("state", "STATE").width(118.0).sortable())
        .column(
            TableColumn::new("status", "STATUS")
                .width(90.0)
                .align(Align::End)
                .sortable(),
        )
        .column(
            TableColumn::new("source", "SOURCE")
                .flex(1.2)
                .min(120.0)
                .sortable(),
        )
        .column(
            TableColumn::new("issue", "ISSUE")
                .flex(1.8)
                .min(160.0)
                .sortable(),
        )
        .on_sort(|state: &mut State, column, dir, _| set_sitemap_sort(state, column, dir))
        .on_select(|state: &mut State, id, _tx| {
            sync_relation_selection(state, Some(id));
            state.view_mut().selected = Some(id.to_string());
            state.details_open = true;
        })
        .cell(move |state: &State, row, column| {
            let Some(entry) = state
                .sitemap_visible
                .get(row)
                .and_then(|index| state.sitemap.urls.get(*index))
            else {
                return Cell::text("");
            };
            cell_sitemap_url(&kit, entry, column)
        })
        .build(state, k),
        SitemapView::Files => Table::new(
            |state: &State| &state.table,
            |state: &mut State| &mut state.table,
            |state: &State| state.sitemap_file_visible.len(),
            |state: &State, row| Cow::Borrowed(visible_id_ref(state, row).unwrap_or("")),
        )
        .name("Sitemap files")
        .style(table_style(k))
        .zebra(false)
        .column(
            TableColumn::new("url", "ADDRESS")
                .flex(2.4)
                .min(180.0)
                .sortable(),
        )
        .column(TableColumn::new("origin", "ORIGIN").width(100.0))
        .column(TableColumn::new("state", "STATE").width(118.0).sortable())
        .column(
            TableColumn::new("status", "STATUS")
                .width(90.0)
                .align(Align::End)
                .sortable(),
        )
        .column(
            TableColumn::new("urls", "URLS")
                .width(84.0)
                .align(Align::End)
                .sortable(),
        )
        .column(
            TableColumn::new("issue", "ISSUE")
                .flex(1.8)
                .min(160.0)
                .sortable(),
        )
        .on_sort(|state: &mut State, column, dir, _| set_sitemap_sort(state, column, dir))
        .on_select(|state: &mut State, id, _tx| {
            sync_relation_selection(state, Some(id));
            state.view_mut().selected = Some(id.to_string());
            state.details_open = true;
        })
        .cell(move |state: &State, row, column| {
            let Some(file) = state
                .sitemap_file_visible
                .get(row)
                .and_then(|index| state.sitemap.files.get(*index))
            else {
                return Cell::text("");
            };
            cell_sitemap_file(&kit, file, column)
        })
        .build(state, k),
    }
}

fn cell_sitemap_url(k: &Kit, entry: &SitemapUrl, column: &str) -> Cell {
    match column {
        "url" => Cell::text(entry.url.clone()),
        "state" => url_state_cell(k, entry.state),
        "status" => {
            if entry.status == 0 {
                Cell::text("—").mono().muted()
            } else {
                status_cell(k, entry.status)
            }
        }
        "source" => Cell::text(source_label(&entry.sources)).muted(),
        "issue" => {
            if entry.state == UrlState::Checked
                && !entry.issue.is_empty()
                && !entry.assessment.needs_attention()
            {
                // Informational context, such as a successful non-HTML response. It stays
                // readable but muted because it is not an issue.
                Cell::text(entry.issue.clone()).muted()
            } else {
                issue_cell(k, &entry.issue)
            }
        }
        _ => Cell::text(""),
    }
}

fn cell_sitemap_file(k: &Kit, file: &SitemapFile, column: &str) -> Cell {
    match column {
        "url" => Cell::text(file.url.clone()),
        "origin" => Cell::text(file.origin.label()).muted(),
        "state" => file_state_cell(k, file.state),
        "status" => {
            if file.status == 0 {
                Cell::text("—").mono().muted()
            } else {
                status_cell(k, file.status)
            }
        }
        "urls" => Cell::text(file.urls.to_string()).mono().muted(),
        "issue" if file.state == FileState::Absent => Cell::text(&file.issue).muted(),
        "issue" => issue_cell(k, &file.issue),
        _ => Cell::text(""),
    }
}

fn url_state_cell(k: &Kit, state: UrlState) -> Cell {
    let color = match state {
        UrlState::Checked => k.c.text_muted,
        UrlState::Blocked => k.c.danger.fg,
        UrlState::Pending | UrlState::Skipped | UrlState::NotChecked => k.c.warning.fg,
    };
    Cell::text(state.label()).color(color)
}

fn file_state_cell(k: &Kit, state: FileState) -> Cell {
    let color = match state {
        FileState::Ok => k.c.text_muted,
        FileState::Error => k.c.danger.fg,
        FileState::Absent | FileState::Skipped | FileState::Pending => k.c.warning.fg,
    };
    Cell::text(state.label()).color(color)
}

fn source_label(sources: &[String]) -> String {
    match sources {
        [] => "—".into(),
        [one] => file_path(one),
        many if many.len() == 2 => format!("2 files · {}", file_path(&many[0])),
        many => format!("{} files", many.len()),
    }
}

fn file_path(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    match rest.split_once('/') {
        Some((_, path)) => format!("/{path}"),
        None => rest.to_string(),
    }
}

fn table_style(k: &Kit) -> TableStyle {
    let mut style = TableStyle::from_kit(k);
    style.row_height = 30.0;
    style.header_height = 32.0;
    style.cell_pad = GUTTER;
    style.font = k.ty.text_small;
    style.mono = k.ty.mono;
    style.header_font = k.ty.small;
    style.text = k.c.text_base;
    style.muted = k.c.text_muted;
    style.header_text = k.c.text_muted;
    style.rule = k.c.border_weak;
    style.selected = k.alpha(k.c.text_base, if k.dark { 0.08 } else { 0.06 });
    style.stripe = k.alpha(k.c.text_base, if k.dark { 0.03 } else { 0.02 });
    style
}

fn cell(k: &Kit, tab: Tab, page: &Page, audit: &Audit, column: &str) -> Cell {
    match (tab, column) {
        (_, "url") => Cell::text(page.url.clone()),
        (_, "status") => status_cell(k, page.status),
        (Tab::Internal, "indexability") => Cell::text(page.indexability.label.text())
            .color(label_color(k, page.indexability.label)),
        (Tab::Internal, "words") => len_cell(page.words, auditable(page)),
        (Tab::Titles, "title") => Cell::text(dash(&page.title)),
        (Tab::Titles, "len") => len_cell(
            page.title.chars().count(),
            auditable(page) || !page.title.is_empty(),
        ),
        (Tab::Meta, "meta") => Cell::text(dash(&page.meta_description)),
        (Tab::Meta, "len") => len_cell(
            page.meta_description.chars().count(),
            auditable(page) || !page.meta_description.is_empty(),
        ),
        (Tab::H1, "h1") => Cell::text(dash(&page.h1())),
        (Tab::H1, "count") => len_cell(page.h1s.len(), auditable(page)),
        (Tab::Response, "type") => Cell::text(mime(&page.content_type)).muted(),
        (Tab::Canonical, "canonical") => Cell::text(dash(&page.canonical)),
        (Tab::Images, "images") => len_cell(page.images, auditable(page)),
        (Tab::Images, "missing_alt") => len_cell(page.images_missing_alt, auditable(page)),
        (Tab::Links, "inbound") => len_cell(audit.inbound(page), true),
        (Tab::Links, "outbound") => len_cell(page.links.len(), auditable(page)),
        (_, "issue") => match row_issue(tab, page, audit) {
            RowIssue::Unclaimed => Cell::text("—").muted(),
            RowIssue::Checked(_) if tab == Tab::Canonical => {
                canonical_issue_cell(k, &canonical_issue(page))
            }
            RowIssue::Checked(issue) => issue_cell(k, &issue),
        },
        _ => Cell::text(""),
    }
}

fn len_cell(n: usize, known: bool) -> Cell {
    Cell::text(if known { n.to_string() } else { "—".into() })
        .mono()
        .muted()
}

fn status_cell(k: &Kit, status: u16) -> Cell {
    let (label, color, flagged) = match status {
        0 => ("ERR".to_string(), k.c.danger.fg, true),
        200..=299 => (status.to_string(), k.c.text_muted, false),
        300..=399 => (status.to_string(), k.c.warning.fg, true),
        _ => (status.to_string(), k.c.danger.fg, true),
    };
    if flagged {
        Cell::el(
            row()
                .gap(7.0)
                .center_y()
                .child(dot(color))
                .child(text(label).font(k.ty.mono).color(color)),
        )
    } else {
        Cell::text(label).mono().muted()
    }
}

fn label_color(k: &Kit, label: Label) -> Color {
    match label {
        Label::FetchFailed | Label::HttpError | Label::Noindex => k.c.danger.fg,
        Label::Redirect | Label::ReviewCanonical | Label::CanonicalElsewhere => k.c.warning.fg,
        Label::NotAssessed => k.c.text_muted,
        Label::NoBlockFound => k.c.success.fg,
    }
}

fn canonical_issue_cell(k: &Kit, issue: &str) -> Cell {
    if issue.is_empty() {
        Cell::text("OK").muted()
    } else {
        // Missing, repeated, malformed and elsewhere canonicals are all preferences or
        // absences, not proof that a URL cannot be indexed.
        Cell::text(issue).color(k.c.warning.fg)
    }
}

fn issue_cell(k: &Kit, issue: &str) -> Cell {
    if issue.is_empty() {
        Cell::text("OK").muted()
    } else {
        let color = if issue.starts_with("Missing")
            || issue.contains("error")
            || issue.starts_with("Noindex")
            || issue.starts_with("Not ")
        {
            k.c.danger.fg
        } else {
            k.c.warning.fg
        };
        Cell::text(issue).color(color)
    }
}

fn selected_page(state: &State) -> Option<&Page> {
    state
        .view()
        .selected
        .as_deref()
        .and_then(|id| page_by_url(state, id))
}

fn detail(k: &Kit, state: &State) -> El {
    if state.tab == Tab::Sitemaps {
        return sitemap_detail(k, state);
    }
    let panel = col()
        .key("detail")
        .w(320.0)
        .h_full()
        .gap(10.0)
        .px(14.0)
        .py(12.0)
        .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_weak);
    let Some(page) = selected_page(state) else {
        return panel.child(micro(k, "Page details")).child(
            text("Select a visible result to inspect its page details.")
                .font(k.ty.body)
                .color(k.c.text_muted)
                .wrap()
                .w_full(),
        );
    };
    let page_url = page.url.clone();
    let position = state
        .table
        .cursor()
        .and_then(|id| (0..visible_len(state)).find(|row| visible_id_ref(state, *row) == Some(id)));
    let prev = Button::new("Previous")
        .variant(ButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .disabled(position.is_none_or(|position| position == 0))
        .help("Previous visible result")
        .on_press_tx(|state: &mut State, tx: &mut Tx| move_selection(state, tx, -1))
        .build(k)
        .key("detail-prev");
    let next = Button::new("Next")
        .variant(ButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .disabled(position.is_none_or(|position| position + 1 >= visible_len(state)))
        .help("Next visible result")
        .on_press_tx(|state: &mut State, tx: &mut Tx| move_selection(state, tx, 1))
        .build(k)
        .key("detail-next");
    let close = IconButton::new(k.glyph.close)
        .variant(IconButtonVariant::GhostMuted)
        .label("Close page details")
        .help("Close page details")
        .on_press(|state: &mut State| state.details_open = false)
        .build(k)
        .key("detail-close");
    let actions = row()
        .gap(4.0)
        .child(
            IconButton::new(&rune_kit::glyphs::COPY)
                .variant(IconButtonVariant::GhostMuted)
                .label("Copy page URL")
                .help("Copy page URL")
                .on_press_tx({
                    let url = page_url.clone();
                    move |_: &mut State, tx: &mut Tx| tx.copy(url.clone())
                })
                .build(k),
        )
        .child(
            IconButton::new(&OPEN_LINK)
                .variant(IconButtonVariant::GhostMuted)
                .label("Open page in browser")
                .help("Open page in browser")
                .on_press_tx({
                    let url = page_url.clone();
                    move |_: &mut State, tx: &mut Tx| {
                        let _ = tx.open_url(&url);
                    }
                })
                .build(k),
        );
    let body = col()
        .key(format!("detail-body-{}-{}", tab_short(state.tab), page.url))
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .gap(12.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(inspector_lead(k, state, page))
        .child(el().w_full().h(1.0).fill(k.c.border_weak))
        .child(page_fields(k, state, page))
        .child(indexability_disclosure(k, state, page));
    panel
        .child(
            row()
                .w_full()
                .center_y()
                .gap(4.0)
                .child(micro(k, active_title(state)))
                .child(el().grow(1.0))
                .child(prev)
                .child(next)
                .child(close),
        )
        .child(
            row()
                .w_full()
                .center_y()
                .gap(6.0)
                .child(
                    text(page.url.clone())
                        .key("detail-url")
                        .font(k.ty.text_small)
                        .color(k.c.text_muted)
                        .wrap()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(actions),
        )
        .child(el().w_full().h(1.0).fill(k.c.border_weak))
        .child(body)
}

fn move_selection(state: &mut State, tx: &mut Tx, delta: i32) {
    let count = visible_len(state);
    if count == 0 {
        return;
    }
    let current = state
        .table
        .cursor()
        .and_then(|id| (0..count).find(|row| visible_id_ref(state, *row) == Some(id)));
    let target = match (current, delta < 0) {
        (Some(0), true) | (None, true) => return,
        (Some(position), true) => position - 1,
        (Some(position), false) => (position + 1).min(count - 1),
        (None, false) => 0,
    };
    let Some(id) = visible_id(state, target) else {
        return;
    };
    sync_relation_selection(state, Some(&id));
    state.view_mut().selected = Some(id.clone());
    state.table.select(id.clone());
    state.table.reveal(id, tx);
    state.details_open = true;
}

/// Show a known URL, clearing only the filters that would hide it.
fn inspect_url(state: &mut State, tx: &mut Tx, url: &str) {
    if inspect_url_state(state, url) {
        state.table.reveal(url.to_string(), tx);
    }
}

fn inspect_url_state(state: &mut State, url: &str) -> bool {
    if !state.pages.iter().any(|page| page.url == url) {
        return false;
    }
    let mut cleared = false;
    let mut switched = false;
    if state.tab == Tab::Sitemaps {
        state.tab = Tab::Internal;
        let (column, dir) = state.view().sort.clone();
        state.table.set_sort(column, dir);
        switched = true;
    }
    state.view_mut().selected = Some(url.to_string());
    refresh_visible_forced(state);
    let visible = state
        .visible
        .iter()
        .any(|index| state.pages.get(*index).is_some_and(|page| page.url == url));
    if !visible {
        state.view_mut().query.replace_text("");
        state.view_mut().filter = CheckFilter::All;
        // `refresh_visible_forced` keeps a selection only while it is visible. Set the target
        // after restoring All so the inspector follows the requested URL, not the old first row.
        state.view_mut().selected = Some(url.to_string());
        refresh_visible_forced(state);
        cleared = true;
    }
    state.view_mut().selected = Some(url.to_string());
    sync_selection(state);
    state.table.select(url.to_string());
    state.details_open = true;
    if cleared {
        state.action_notice = Some(format!("Cleared filters to show {url}"));
    } else if switched {
        state.action_notice = Some(format!("Showing {url} in Internal URLs"));
    }
    true
}

fn copy_icon(k: &Kit, label: &str, value: String) -> El {
    IconButton::new(&rune_kit::glyphs::COPY)
        .variant(IconButtonVariant::GhostMuted)
        .label(label)
        .help(label)
        .on_press_tx(move |_: &mut State, tx: &mut Tx| tx.copy(value.clone()))
        .build(k)
}

fn value_with_copy(k: &Kit, label: &str, value: &str, copy_label: &str) -> El {
    let shown = dash(value);
    let mut line = row().w_full().gap(6.0).child(
        text(shown.clone())
            .font(k.ty.text_small)
            .color(k.c.text_base)
            .wrap()
            .grow(1.0)
            .min_w(0.0),
    );
    if !value.is_empty() {
        line = line.child(copy_icon(k, copy_label, value.to_string()));
    }
    col()
        .w_full()
        .min_w(0.0)
        .gap(3.0)
        .child(micro(k, label))
        .child(line)
}

fn detail_field_copy(k: &Kit, label: &str, value: &str) -> El {
    value_with_copy(k, label, value, &format!("Copy {label}"))
}

fn primary_issue(tab: Tab, page: &Page, audit: &Audit) -> String {
    if !auditable(page) && tab != Tab::Links && tab != Tab::Internal {
        if page.status == 0 {
            return "No response · content checks not completed".into();
        }
        if page.status != 200 {
            return format!("HTTP {} · content checks not completed", page.status);
        }
        if !page.content_type.to_ascii_lowercase().contains("html") {
            return "Non-HTML content · content checks not applicable".into();
        }
        if page_incomplete(page) {
            return "Capture incomplete · content checks not completed".into();
        }
    }
    let issue = issue_for(tab, page, audit);
    if !issue.is_empty() {
        return issue;
    }
    match tab {
        Tab::Titles => "Title length is within this tool's 30–60 character heuristic".into(),
        Tab::Meta => {
            "Meta description length is within this tool's 70–160 character heuristic".into()
        }
        Tab::H1 => "One H1".into(),
        Tab::Response => match page.status {
            200..=299 => format!("HTTP {} · no response finding", page.status),
            _ => "No response finding".into(),
        },
        Tab::Canonical => "No canonical finding".into(),
        Tab::Images => "No missing image alt text".into(),
        Tab::Links => "No broken link observed in this crawl".into(),
        Tab::Internal => page.indexability.label.text().to_string(),
        Tab::Sitemaps => "No issue".into(),
    }
}

fn inspector_lead(k: &Kit, state: &State, page: &Page) -> El {
    let tab = state.tab;
    let issue = primary_issue(tab, page, &state.audit);
    let color = if (page_incomplete(page) && !auditable(page))
        || matches!(row_issue(tab, page, &state.audit), RowIssue::Checked(ref issue) if !issue.is_empty())
        || (tab == Tab::Internal && page.indexability.needs_attention())
    {
        k.c.warning.fg
    } else {
        k.c.text_base
    };
    let mut lead = col().w_full().min_w(0.0).gap(8.0).child(
        text(issue)
            .font(k.ty.body_medium)
            .color(color)
            .wrap()
            .w_full()
            .key("inspector-lead"),
    );
    match tab {
        Tab::Internal => {
            lead = lead.child(assessment_detail(
                k,
                page,
                &state.pages,
                state.evidence_open,
                state.capture_open,
            ));
        }
        Tab::Titles | Tab::Meta => {
            let (label, value) = if tab == Tab::Titles {
                ("Title", page.title.as_str())
            } else {
                ("Meta description", page.meta_description.as_str())
            };
            lead = lead.child(value_with_copy(k, label, value, &format!("Copy {label}")));
            if auditable(page) {
                lead = lead.child(muted_note(
                    k,
                    format!(
                        "{} characters · this tool's heuristic, not a search-engine limit",
                        value.chars().count()
                    ),
                ));
            } else {
                lead = lead.child(muted_note(
                    k,
                    "The content check did not complete for this URL, so no length claim is made.",
                ));
            }
            lead = lead.child(peer_block(
                k,
                "Duplicate peers",
                state,
                page,
                tab,
                state.relation_pages.peers,
            ));
        }
        Tab::H1 => {
            let known = auditable(page);
            lead = lead.child(micro(k, "All captured H1s"));
            if page.h1s.is_empty() {
                lead = lead.child(muted_note(
                    k,
                    if known {
                        "No H1 captured."
                    } else {
                        "Not captured."
                    },
                ));
            } else {
                let (start, end) = relation_window(state.relation_pages.h1, page.h1s.len());
                lead = lead.child(muted_note(
                    k,
                    format!("Showing {}-{} of {}", start + 1, end, page.h1s.len()),
                ));
                if start > 0 {
                    lead = lead.child(
                        Button::new("Previous")
                            .variant(ButtonVariant::GhostMuted)
                            .control_size(ControlSize::Small)
                            .on_press(|state: &mut State| {
                                state.relation_pages.h1 = state.relation_pages.h1.saturating_sub(1)
                            })
                            .build(k)
                            .key("h1-prev"),
                    );
                }
                if end < page.h1s.len() {
                    lead = lead.child(
                        Button::new(format!(
                            "Show next {} H1s",
                            (page.h1s.len() - end).min(RELATION_PAGE)
                        ))
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(ControlSize::Small)
                        .on_press(|state: &mut State| state.relation_pages.h1 += 1)
                        .build(k)
                        .key("h1-more"),
                    );
                }
                for (index, h1) in page.h1s.iter().enumerate().skip(start).take(RELATION_PAGE) {
                    lead = lead.child(
                        value_with_copy(
                            k,
                            &format!("H1 {}", index + 1),
                            h1,
                            &format!("Copy H1 {}", index + 1),
                        )
                        .key(format!("h1-item-{index}")),
                    );
                }
                let source_url = page.url.clone();
                lead = lead.child(
                    Button::new(format!("Copy all {} H1s", page.h1s.len()))
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(ControlSize::Small)
                        .on_press_tx(move |state: &mut State, tx: &mut Tx| {
                            tx.copy(relation_copy(state, &source_url, RelationCopy::H1))
                        })
                        .build(k)
                        .key("h1-copy-all"),
                );
            }
            if page.h1s.len() == 1 {
                lead = lead.child(peer_block(
                    k,
                    "Duplicate H1 peers",
                    state,
                    page,
                    tab,
                    state.relation_pages.peers,
                ));
            }
        }
        Tab::Response => {
            let status = if page.status == 0 {
                "—".to_string()
            } else {
                page.status.to_string()
            };
            lead = lead.child(detail_field_copy(k, "HTTP status", &status));
            if !page.error.is_empty() {
                lead = lead.child(detail_field_copy(k, "Error", &page.error));
            }
            if !page.redirect_to.is_empty() {
                lead = lead.child(value_with_copy(
                    k,
                    "Redirect destination",
                    &page.redirect_to,
                    "Copy redirect destination",
                ));
                let chain = state.audit.redirect_chain(page);
                if chain.len() > 1 {
                    lead = lead.child(micro(k, "Known redirect chain"));
                    for target in chain {
                        lead = lead.child(link_target_row(k, state, &target));
                    }
                } else {
                    lead = lead.child(link_target_row(k, state, &page.redirect_to));
                }
            }
        }
        Tab::Canonical => {
            let canonical = page.canonical.as_str();
            lead = lead.child(value_with_copy(
                k,
                "Assessed canonical",
                canonical,
                "Copy canonical",
            ));
            lead = lead.child(muted_note(
                k,
                "A canonical preference is not an indexing block. Source and rendered declarations stay under evidence.",
            ));
            let target = page.indexability.first_valid_canonical().unwrap_or("");
            if !target.is_empty() {
                lead = lead.child(link_target_row(k, state, target));
            }
        }
        Tab::Images => {
            if auditable(page) {
                lead = lead.child(
                    row()
                        .w_full()
                        .gap(20.0)
                        .child(
                            col().gap(3.0).child(micro(k, "Images")).child(
                                text(page.images.to_string())
                                    .font(k.ty.text_small)
                                    .color(k.c.text_base),
                            ),
                        )
                        .child(
                            col().gap(3.0).child(micro(k, "Missing alt")).child(
                                text(page.images_missing_alt.to_string())
                                    .font(k.ty.text_small)
                                    .color(k.c.text_base),
                            ),
                        ),
                );
                lead = lead.child(muted_note(
                    k,
                    "Image elements are not retained in this version, so this is a count, not an element list.",
                ));
            } else {
                lead = lead.child(muted_note(
                    k,
                    "The image count was not captured for this URL. No zero is claimed.",
                ));
            }
        }
        Tab::Links => {
            lead = lead.child(links_lead(k, state, page));
        }
        Tab::Sitemaps => {}
    }
    lead
}

fn peer_block(k: &Kit, title: &str, state: &State, page: &Page, tab: Tab, expanded: usize) -> El {
    let relations = inspector_relations(state, page, tab);
    let peers = &relations.peers;
    if peers.is_empty() {
        return col();
    }
    let mut block = col()
        .w_full()
        .min_w(0.0)
        .gap(4.0)
        .child(micro(k, format!("{title} ({})", peers.len())));
    let source_url = page.url.clone();
    let (start, end) = relation_window(expanded, peers.len());
    block = block.child(muted_note(
        k,
        format!("Showing {}-{} of {}", start + 1, end, peers.len()),
    ));
    if start > 0 {
        block = block.child(
            Button::new("Previous")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(|state: &mut State| {
                    state.relation_pages.peers = state.relation_pages.peers.saturating_sub(1)
                })
                .build(k)
                .key("peers-prev"),
        );
    }
    if end < peers.len() {
        block = block.child(
            Button::new(format!(
                "Show next {}",
                (peers.len() - end).min(RELATION_PAGE)
            ))
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press(|state: &mut State| state.relation_pages.peers += 1)
            .build(k)
            .key("peers-more"),
        );
    }
    for (index, page_index) in peers.iter().enumerate().skip(start).take(RELATION_PAGE) {
        let page = &state.pages[*page_index];
        block = block.child(
            row()
                .w_full()
                .gap(6.0)
                .center_y()
                .child(
                    text(page.url.clone())
                        .font(k.ty.small)
                        .color(k.c.text_base)
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0)
                        .help(page.url.clone()),
                )
                .child(
                    Button::new("Inspect")
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(ControlSize::Small)
                        .on_press_tx({
                            let url = page.url.clone();
                            move |state: &mut State, tx: &mut Tx| inspect_url(state, tx, &url)
                        })
                        .build(k)
                        .key(format!("peer-{index}")),
                ),
        );
    }
    let copy_label = match peers.len() {
        1 => "Copy 1 URL".to_string(),
        count => format!("Copy all {count} URLs"),
    };
    block.child(
        Button::new(copy_label)
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press_tx(move |state: &mut State, tx: &mut Tx| {
                tx.copy(relation_copy(state, &source_url, RelationCopy::Peers(tab)))
            })
            .build(k),
    )
}

fn page_by_url<'a>(state: &'a State, url: &str) -> Option<&'a Page> {
    let index = page_positions(state).get(url).copied()?;
    state.pages.get(index).filter(|page| page.url == url)
}

fn link_target_row(k: &Kit, state: &State, url: &str) -> El {
    link_target_row_keyed(k, state, url, None)
}

fn link_target_row_keyed(k: &Kit, state: &State, url: &str, inspect_key: Option<String>) -> El {
    let known = page_by_url(state, url);
    let status = known
        .map(|page| {
            if page.status == 0 {
                "ERR".to_string()
            } else {
                page.status.to_string()
            }
        })
        .unwrap_or_else(|| "not checked".into());
    let mut row = row()
        .w_full()
        .gap(6.0)
        .center_y()
        .child(
            text(url.to_string())
                .font(k.ty.small)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0)
                .min_w(0.0)
                .help(url.to_string()),
        )
        .child(text(status).font(k.ty.mono).color(k.c.text_muted));
    if known.is_some() {
        let mut button = Button::new("Inspect")
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press_tx({
                let url = url.to_string();
                move |state: &mut State, tx: &mut Tx| inspect_url(state, tx, &url)
            })
            .build(k);
        if let Some(key) = inspect_key {
            button = button.key(key);
        }
        row = row.child(button);
    } else {
        row = row.child(
            IconButton::new(&OPEN_LINK)
                .variant(IconButtonVariant::GhostMuted)
                .label("Open link target")
                .help("This URL was not checked in this crawl")
                .on_press_tx({
                    let url = url.to_string();
                    move |_: &mut State, tx: &mut Tx| {
                        let _ = tx.open_url(&url);
                    }
                })
                .build(k),
        );
    }
    row.child(copy_icon(k, "Copy link target", url.to_string()))
}

fn links_lead(k: &Kit, state: &State, page: &Page) -> El {
    let relations = inspector_relations(state, page, Tab::Links);
    let inbound = &relations.inbound;
    let outbound = &relations.outbound;
    let mut block = col()
        .w_full()
        .min_w(0.0)
        .gap(6.0)
        .child(muted_note(
            k,
            if page_incomplete(page) {
                "Capture incomplete; links below are partial observations, not a complete site backlink map."
            } else {
                "Links observed in this crawl, not a complete site backlink map."
            },
        ))
        .child(micro(
            k,
            format!("{} incoming · {} outgoing", inbound.len(), outbound.len()),
        ));
    if !inbound.is_empty() {
        block = block.child(micro(k, "Incoming sources").key("incoming-label"));
        let (start, end) = relation_window(state.relation_pages.inbound, inbound.len());
        block = block.child(muted_note(
            k,
            format!("Showing {}-{} of {}", start + 1, end, inbound.len()),
        ));
        if start > 0 {
            block = block.child(
                Button::new("Previous")
                    .variant(ButtonVariant::GhostMuted)
                    .control_size(ControlSize::Small)
                    .on_press(|state: &mut State| {
                        state.relation_pages.inbound =
                            state.relation_pages.inbound.saturating_sub(1)
                    })
                    .build(k)
                    .key("inbound-prev"),
            );
        }
        if end < inbound.len() {
            block = block.child(
                Button::new(format!(
                    "Show next {} incoming",
                    (inbound.len() - end).min(RELATION_PAGE)
                ))
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(|state: &mut State| state.relation_pages.inbound += 1)
                .build(k)
                .key("inbound-more"),
            );
        }
        for (index, source_index) in inbound.iter().enumerate().skip(start).take(RELATION_PAGE) {
            let source = &state.pages[*source_index];
            block = block.child(link_target_row_keyed(
                k,
                state,
                &source.url,
                Some(format!("inbound-inspect-{index}")),
            ));
        }
        let source_url = page.url.clone();
        block = block.child(
            Button::new(format!("Copy all {} incoming", inbound.len()))
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press_tx(move |state: &mut State, tx: &mut Tx| {
                    tx.copy(relation_copy(state, &source_url, RelationCopy::Incoming))
                })
                .build(k)
                .key("inbound-copy-all"),
        );
    }
    if !outbound.is_empty() {
        block = block.child(
            micro(
                k,
                if relations.has_broken {
                    "Outgoing targets · broken first"
                } else {
                    "Outgoing targets"
                },
            )
            .key("outgoing-label"),
        );
        let (start, end) = relation_window(state.relation_pages.outbound, outbound.len());
        block = block.child(muted_note(
            k,
            format!("Showing {}-{} of {}", start + 1, end, outbound.len()),
        ));
        if start > 0 {
            block = block.child(
                Button::new("Previous")
                    .variant(ButtonVariant::GhostMuted)
                    .control_size(ControlSize::Small)
                    .on_press(|state: &mut State| {
                        state.relation_pages.outbound =
                            state.relation_pages.outbound.saturating_sub(1)
                    })
                    .build(k)
                    .key("outbound-prev"),
            );
        }
        if end < outbound.len() {
            block = block.child(
                Button::new(format!(
                    "Show next {} outgoing",
                    (outbound.len() - end).min(RELATION_PAGE)
                ))
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(|state: &mut State| state.relation_pages.outbound += 1)
                .build(k)
                .key("outbound-more"),
            );
        }
        for (index, target_index) in outbound.iter().enumerate().skip(start).take(RELATION_PAGE) {
            let target = &page.links[*target_index];
            block = block.child(link_target_row_keyed(
                k,
                state,
                target,
                Some(format!("outbound-inspect-{index}")),
            ));
        }
        let source_url = page.url.clone();
        block = block.child(
            Button::new(format!("Copy all {} targets", outbound.len()))
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press_tx(move |state: &mut State, tx: &mut Tx| {
                    tx.copy(relation_copy(state, &source_url, RelationCopy::Outgoing))
                })
                .build(k),
        );
    }
    block
}

fn page_fields(k: &Kit, state: &State, page: &Page) -> El {
    let known = auditable(page);
    let tab = state.tab;
    let h1 = page.h1();
    let mut fields = col()
        .w_full()
        .min_w(0.0)
        .gap(10.0)
        .child(micro(k, "Page fields"));
    if tab != Tab::Titles {
        fields = fields.child(detail_field_copy(
            k,
            "Title",
            if known { &page.title } else { "" },
        ));
    }
    if tab != Tab::Meta {
        fields = fields.child(detail_field_copy(
            k,
            "Meta description",
            if known { &page.meta_description } else { "" },
        ));
    }
    if tab != Tab::H1 {
        fields = fields.child(detail_field_copy(k, "H1", if known { &h1 } else { "" }));
    }
    if tab != Tab::Canonical {
        fields = fields.child(detail_field_copy(
            k,
            "Canonical",
            if known { &page.canonical } else { "" },
        ));
    }
    fields = fields
        .child(detail_field_copy(
            k,
            "HTTP status",
            &if page.status == 0 {
                "—".to_string()
            } else {
                page.status.to_string()
            },
        ))
        .child(detail_field_copy(
            k,
            "Content type",
            &mime(&page.content_type),
        ))
        .child(detail_field_copy(
            k,
            "Words",
            &if known {
                page.words.to_string()
            } else {
                "—".to_string()
            },
        ))
        .child(detail_field_copy(
            k,
            "Images · missing alt",
            &if known {
                format!("{} · {}", page.images, page.images_missing_alt)
            } else {
                "—".to_string()
            },
        ));
    fields
}

fn indexability_disclosure(k: &Kit, state: &State, page: &Page) -> El {
    if state.tab == Tab::Internal {
        return col();
    }
    detail_disclosure(
        k,
        "Indexability & evidence",
        state.evidence_open,
        "indexability-disclosure",
        "indexability-toggle",
        if state.evidence_open {
            assessment_detail(k, page, &state.pages, true, state.capture_open)
        } else {
            col()
        },
        |state| state.evidence_open ^= true,
    )
}

fn severity_color(k: &Kit, severity: Severity) -> Color {
    match severity {
        Severity::Issue => k.c.danger.fg,
        Severity::Advisory => k.c.warning.fg,
        Severity::Info => k.c.text_muted,
    }
}

fn muted_note(k: &Kit, value: impl Into<String>) -> El {
    text(value.into())
        .font(k.ty.small)
        .color(k.c.text_muted)
        .wrap()
        .w_full()
}

fn assessment_detail(
    k: &Kit,
    page: &Page,
    pages: &[Page],
    evidence_open: bool,
    capture_open: bool,
) -> El {
    let assessment = &page.indexability;
    let mut block = col().w_full().min_w(0.0).gap(8.0).child(
        text(assessment.label.text().to_string())
            .font(k.ty.body_medium)
            .color(label_color(k, assessment.label))
            .wrap()
            .w_full(),
    );

    let mut findings = col().w_full().min_w(0.0).gap(6.0);
    if assessment.findings.is_empty() {
        findings = findings.child(muted_note(k, "No supporting findings recorded."));
    } else {
        for finding in &assessment.findings {
            let mut item = col().w_full().min_w(0.0).gap(2.0).child(
                text(format!("{} · {}", finding.severity.label(), finding.text))
                    .font(k.ty.small)
                    .color(severity_color(k, finding.severity))
                    .wrap()
                    .w_full(),
            );
            if !finding.guidance.is_empty() {
                item = item.child(muted_note(k, finding.guidance.clone()));
            }
            findings = findings.child(item);
        }
    }
    // Keep the primary actionable finding visible; full explanations and raw observations
    // stay one disclosure away instead of pushing the page's own fields below guidance.
    if let Some(finding) = assessment
        .findings
        .iter()
        .find(|finding| finding.severity != Severity::Info)
    {
        block = block.child(
            text(finding.text.clone())
                .font(k.ty.text_xsmall)
                .color(severity_color(k, finding.severity))
                .wrap()
                .w_full(),
        );
    }
    let evidence_copy = format!(
        "{}\n{}\n{}",
        assessment.label.text(),
        assessment.all_reasons_text(),
        assessment.evidence_text()
    );
    let mut evidence = col()
        .key("evidence-content")
        .w_full()
        .min_w(0.0)
        .gap(12.0)
        .pb(8.0)
        .child(
            Button::new("Copy evidence")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press_tx(move |_: &mut State, tx: &mut Tx| tx.copy(evidence_copy.clone()))
                .build(k)
                .key("copy-evidence"),
        )
        .child(findings);

    let mut canonical = col()
        .w_full()
        .min_w(0.0)
        .gap(4.0)
        .child(micro(k, "Canonical declarations"));
    if assessment.evidence.canonicals.is_empty() {
        canonical = canonical.child(muted_note(k, "Not observed"));
    } else {
        for declaration in &assessment.evidence.canonicals {
            let line = match declaration.state {
                calmtoad::indexability::CanonicalState::Valid => {
                    let resolved = declaration.resolved.as_deref().unwrap_or("");
                    if declaration.raw == resolved {
                        declaration.raw.clone()
                    } else {
                        format!("{} → {resolved}", declaration.raw)
                    }
                }
                calmtoad::indexability::CanonicalState::Empty => "Empty href".to_string(),
                calmtoad::indexability::CanonicalState::Invalid => {
                    format!("Invalid href: {}", declaration.raw)
                }
            };
            canonical = canonical.child(
                text(if assessment.capture.mode == CaptureMode::Browser {
                    format!("{line} · {}", declaration.stage.label())
                } else {
                    line
                })
                .font(k.ty.small)
                .color(k.c.text_base)
                .wrap()
                .w_full(),
            );
            if declaration.resolved.is_some() {
                let check = if declaration.matches_url(&page.url) {
                    "Self · this URL was fetched".to_string()
                } else if let Some(other) = pages
                    .iter()
                    .find(|other| declaration.matches_url(&other.url))
                {
                    format!(
                        "Target fetched · HTTP {} · {}",
                        other.status,
                        other.indexability.label.text()
                    )
                } else {
                    "Target not checked in this crawl".to_string()
                };
                canonical = canonical.child(muted_note(
                    k,
                    if declaration.truncated {
                        format!("{check} · target text truncated")
                    } else {
                        check
                    },
                ));
            }
        }
    }
    evidence = evidence.child(canonical);

    let mut robots = col()
        .w_full()
        .min_w(0.0)
        .gap(4.0)
        .child(micro(k, "Robots directives"));
    if assessment.evidence.directives.is_empty() {
        robots = robots.child(muted_note(k, "Not observed"));
    } else {
        for directive in &assessment.evidence.directives {
            let applies = directive.applies_to_google();
            let color = if applies && directive.noindex {
                k.c.danger.fg
            } else if !applies {
                k.c.text_muted
            } else {
                k.c.text_base
            };
            let stage = if assessment.capture.mode == CaptureMode::Browser {
                format!(" · {}", directive.stage.label())
            } else {
                String::new()
            };
            robots = robots.child(
                text(format!(
                    "{} · {}{stage}",
                    directive.source_label(),
                    directive.raw
                ))
                .font(k.ty.small)
                .color(color)
                .wrap()
                .w_full(),
            );
            if !applies {
                robots = robots.child(muted_note(
                    k,
                    format!(
                        "Scoped to {} · not applied to Google Web Search",
                        directive.agent.as_deref().unwrap_or("another agent")
                    ),
                ));
            } else if directive.nofollow && !directive.noindex {
                robots = robots.child(muted_note(
                    k,
                    "nofollow does not by itself prevent indexing.",
                ));
            }
        }
    }
    evidence = evidence.child(robots);

    let status = if page.status == 0 {
        "No response".to_string()
    } else {
        page.status.to_string()
    };
    evidence = evidence
        .child(detail_field(k, "HTML read", assessment.read_label()))
        .child(detail_field(k, "HTTP status", &status));
    block = block.child(detail_disclosure(
        k,
        "Evidence & guidance",
        evidence_open,
        "evidence-disclosure",
        "evidence-toggle",
        evidence,
        |state| state.evidence_open ^= true,
    ));
    if assessment.capture.render.is_some() {
        block = block.child(detail_disclosure(
            k,
            "Browser capture",
            capture_open,
            "capture-disclosure",
            "capture-toggle",
            capture_detail(k, &assessment.capture, &page.url).key("capture-content"),
            |state| state.capture_open ^= true,
        ));
    }
    block
}

fn detail_disclosure(
    k: &Kit,
    title: &'static str,
    open: bool,
    key: &'static str,
    toggle_key: &'static str,
    content: El,
    toggle: impl Fn(&mut State) + 'static,
) -> El {
    let glyph = if open {
        k.glyph.triangle_down
    } else {
        k.glyph.triangle_right
    };
    let font = k.ty.text_small;
    let mut section = col().w_full().min_w(0.0).key(key).child(
        Button::new(title)
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .expanded(open)
            .label_with(move |_, ink| {
                row()
                    .w_full()
                    .gap(6.0)
                    .center_y()
                    .child(icon(glyph, 12.0).color(ink))
                    .child(text(title).font(font).color(ink))
            })
            .on_press(toggle)
            .build(k)
            .w_full()
            .key(toggle_key),
    );
    if open {
        section = section.child(content);
    }
    section
}

fn capture_detail(k: &Kit, capture: &CaptureInfo, page_url: &str) -> El {
    let Some(render) = capture.render.as_ref() else {
        return detail_field(k, "Capture", "HTML · no browser");
    };
    let color = match render.state {
        RenderState::Captured => k.c.success.fg,
        RenderState::TimedOut
        | RenderState::Redirected
        | RenderState::Navigated
        | RenderState::Truncated => k.c.warning.fg,
        RenderState::Failed | RenderState::Blocked | RenderState::Cancelled => k.c.danger.fg,
    };
    let mut block = col()
        .w_full()
        .min_w(0.0)
        .gap(4.0)
        .child(
            row().w_full().gap(8.0).center_y().child(dot(color)).child(
                text(capture.state_label())
                    .font(k.ty.small)
                    .color(k.c.text_base),
            ),
        )
        .child(muted_note(
            k,
            format!(
                "{} · {} requests · {} ms",
                if render.browser.is_empty() {
                    "Local browser"
                } else {
                    &render.browser
                },
                render.requests,
                render.elapsed_ms
            ),
        ));
    let stages = match (render.rendered_captured, render.source_state) {
        (true, SourceState::Captured) => "Source HTML and rendered DOM captured".to_string(),
        (true, SourceState::NotApplicable) => "Rendered DOM captured".to_string(),
        (true, other) => format!(
            "Rendered DOM captured; pre-script source {}",
            other.label().to_lowercase()
        ),
        (false, SourceState::Captured) => {
            "Source HTML captured; rendered DOM incomplete".to_string()
        }
        (false, other) => format!(
            "No complete document captured (source {})",
            other.label().to_lowercase()
        ),
    };
    block = block.child(muted_note(k, stages));
    if !render.reason.is_empty() {
        block = block.child(muted_note(k, render.reason.clone()));
    }
    if !render.observed_url.is_empty() && render.observed_url != page_url {
        block = block.child(detail_field(k, "Observed URL", &render.observed_url));
    }
    if !render.navigation_to.is_empty() {
        block = block.child(detail_field(k, "Queued navigation", &render.navigation_to));
    }
    if !render.source_note.is_empty() {
        block = block.child(muted_note(k, render.source_note.clone()));
    }
    block
}

fn sitemap_detail(k: &Kit, state: &State) -> El {
    let panel = col()
        .key("detail")
        .w(320.0)
        .h_full()
        .gap(10.0)
        .px(14.0)
        .py(12.0)
        .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_weak);
    let cursor = state.view().selected.as_deref();

    let url_entry = state
        .sitemap
        .urls
        .iter()
        .find(|entry| cursor == Some(entry.url.as_str()));
    let file = state
        .sitemap
        .files
        .iter()
        .find(|file| cursor == Some(file.url.as_str()));

    let (heading, selected, fields) = match state.sitemap_view {
        SitemapView::Urls => match url_entry {
            Some(entry) => (
                "Sitemap URL",
                entry.url.clone(),
                sitemap_url_fields(entry, &state.pages),
            ),
            None => return sitemap_detail_empty(k, panel, "URL"),
        },
        SitemapView::Files => match file {
            Some(file) => ("Sitemap file", file.url.clone(), sitemap_file_fields(file)),
            None => return sitemap_detail_empty(k, panel, "file"),
        },
    };

    let copy_url = selected.clone();
    let open_url = selected.clone();
    let actions = row()
        .gap(4.0)
        .child(
            Button::new("Previous")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .help("Previous visible sitemap row")
                .on_press_tx(|state: &mut State, tx: &mut Tx| move_selection(state, tx, -1))
                .build(k)
                .key("sitemap-prev"),
        )
        .child(
            Button::new("Next")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .help("Next visible sitemap row")
                .on_press_tx(|state: &mut State, tx: &mut Tx| move_selection(state, tx, 1))
                .build(k)
                .key("sitemap-next"),
        )
        .child(
            IconButton::new(&rune_kit::glyphs::COPY)
                .variant(IconButtonVariant::GhostMuted)
                .label("Copy URL")
                .help("Copy URL")
                .on_press_tx(move |_: &mut State, tx: &mut Tx| tx.copy(copy_url.clone()))
                .build(k),
        )
        .child(
            IconButton::new(&OPEN_LINK)
                .variant(IconButtonVariant::GhostMuted)
                .label("Open in browser")
                .help("Open in browser")
                .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                    let _ = tx.open_url(&open_url);
                })
                .build(k),
        )
        .child(
            IconButton::new(k.glyph.close)
                .variant(IconButtonVariant::GhostMuted)
                .label("Close sitemap details")
                .help("Close sitemap details")
                .on_press(|state: &mut State| state.details_open = false)
                .build(k)
                .key("sitemap-close"),
        );
    let header = panel
        .child(
            row()
                .w_full()
                .center_y()
                .gap(4.0)
                .child(micro(k, heading))
                .child(el().grow(1.0))
                .child(actions),
        )
        .child(
            text(selected)
                .key("detail-url")
                .font(k.ty.text_small)
                .color(k.c.text_muted)
                .wrap()
                .w_full(),
        )
        .child(el().w_full().h(1.0).fill(k.c.border_weak));
    let mut body = col()
        .key(format!(
            "sitemap-detail-body-{}-{}",
            state.selection_epoch,
            state.view().selected.as_deref().unwrap_or("")
        ))
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .gap(12.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        });
    if let Some(entry) = url_entry
        && entry.state == UrlState::Checked
        && state.pages.iter().any(|page| page.url == entry.url)
    {
        body = body.child(
            Button::new("Inspect fetched page")
                .variant(ButtonVariant::Neutral)
                .control_size(ControlSize::Small)
                .on_press_tx({
                    let url = entry.url.clone();
                    move |state: &mut State, tx: &mut Tx| inspect_url(state, tx, &url)
                })
                .build(k)
                .key("sitemap-inspect-page"),
        );
    }
    if let Some(entry) = url_entry
        && let Some(source) = entry.sources.first()
        && state.sitemap.files.iter().any(|file| &file.url == source)
    {
        body = body.child(
            Button::new("Inspect source sitemap")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx({
                    let url = source.clone();
                    move |state: &mut State, tx: &mut Tx| inspect_sitemap_file(state, tx, &url)
                })
                .build(k)
                .key("sitemap-inspect-source"),
        );
    }
    for (label, value) in fields {
        body = body.child(detail_field(k, label, &value));
    }
    header.child(body)
}

fn inspect_sitemap_file(state: &mut State, tx: &mut Tx, url: &str) {
    let Some(index) = state.sitemap.files.iter().position(|file| file.url == url) else {
        return;
    };
    state.tab = Tab::Sitemaps;
    state.sitemap_view = SitemapView::Files;
    let (column, dir) = state.view().sort.clone();
    state.table.set_sort(column, dir);
    state.view_mut().selected = Some(url.to_string());
    refresh_sitemap_visible_inner(state, true);
    let visible = state.sitemap_file_visible.iter().any(|visible| {
        state
            .sitemap
            .files
            .get(*visible)
            .is_some_and(|file| file.url == url)
    });
    if !visible {
        state.view_mut().query.replace_text("");
        state.view_mut().filter = CheckFilter::All;
        state.view_mut().selected = Some(url.to_string());
        refresh_sitemap_visible_inner(state, true);
    }
    let id = state.sitemap.files[index].url.clone();
    sync_selection(state);
    state.table.select(id.clone());
    state.table.reveal(id, tx);
    state.details_open = true;
}

fn sitemap_detail_empty(k: &Kit, panel: El, noun: &str) -> El {
    panel.child(micro(k, "Sitemap details")).child(
        text(format!("Select a sitemap {noun} to inspect it."))
            .font(k.ty.body)
            .color(k.c.text_muted)
            .wrap()
            .w_full(),
    )
}

fn sitemap_url_fields(entry: &SitemapUrl, pages: &[Page]) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("State", entry.state.label().to_string()),
        (
            "Status",
            if entry.status == 0 {
                "—".into()
            } else {
                entry.status.to_string()
            },
        ),
        (
            "Source",
            if entry.sources.is_empty() {
                "—".into()
            } else {
                entry.sources.join(" · ")
            },
        ),
        (
            "Finding",
            if entry.issue.is_empty() {
                "OK".into()
            } else {
                entry.issue.clone()
            },
        ),
    ];
    if entry.state == UrlState::Checked {
        fields.push(("Indexability", entry.assessment.label.text().to_string()));
        let reasons = entry.assessment.all_reasons_text();
        fields.push((
            "Reasons",
            if reasons.is_empty() {
                "—".into()
            } else {
                reasons
            },
        ));
        fields.push(("Evidence", entry.assessment.evidence_text()));
        fields.push(("Read", entry.assessment.read_label().to_string()));
        let mut seen_targets = Vec::new();
        for declaration in &entry.assessment.evidence.canonicals {
            let Some(target) = declaration.resolved.as_deref() else {
                continue;
            };
            if declaration.state != calmtoad::indexability::CanonicalState::Valid {
                continue;
            }
            let Some(hash) = declaration.resolved_hash else {
                continue;
            };
            if seen_targets.contains(&hash) {
                continue;
            }
            seen_targets.push(hash);
            let check = if declaration.matches_url(&entry.url) {
                "Self · fetched".to_string()
            } else if let Some(other) = pages.iter().find(|page| declaration.matches_url(&page.url))
            {
                format!(
                    "Fetched · HTTP {} · {}",
                    other.status,
                    other.indexability.label.text()
                )
            } else {
                "Not checked in this crawl".to_string()
            };
            fields.push((
                "Canonical target",
                format!(
                    "{target} · {check}{}",
                    if declaration.truncated {
                        " · target text truncated"
                    } else {
                        ""
                    }
                ),
            ));
        }
    }
    if !entry.redirect_to.is_empty() {
        fields.push(("Redirect to", entry.redirect_to.clone()));
    }
    if !entry.canonical.is_empty() {
        fields.push(("Canonical", entry.canonical.clone()));
    }
    if !entry.content_type.is_empty() {
        fields.push(("Content type", mime(&entry.content_type)));
    }
    if !entry.error.is_empty() {
        fields.push(("Error", entry.error.clone()));
    }
    fields
}

fn sitemap_file_fields(file: &SitemapFile) -> Vec<(&'static str, String)> {
    vec![
        ("Origin", file.origin.label().to_string()),
        ("State", file.state.label().to_string()),
        (
            "HTTP status",
            if file.status == 0 {
                "—".into()
            } else {
                file.status.to_string()
            },
        ),
        ("Document", sitemap_kind_label(file.kind).to_string()),
        ("Depth", file.depth.to_string()),
        ("Entries", file.entries.to_string()),
        ("Page URLs", file.urls.to_string()),
        (
            "Issue",
            if file.issue.is_empty() {
                "OK".into()
            } else {
                file.issue.clone()
            },
        ),
    ]
}

fn sitemap_kind_label(kind: SitemapKind) -> &'static str {
    match kind {
        SitemapKind::Unknown => "—",
        SitemapKind::Urlset => "URL set",
        SitemapKind::Index => "Index",
    }
}

fn detail_field(k: &Kit, label: &str, value: &str) -> El {
    col()
        .w_full()
        .min_w(0.0)
        .gap(3.0)
        .child(micro(k, label))
        .child(
            text(value.to_string())
                .font(k.ty.text_small)
                .color(k.c.text_base)
                .wrap()
                .w_full(),
        )
}

fn status_message(state: &State) -> String {
    if let Some(notice) = &state.action_notice {
        return notice.clone();
    }
    let elapsed = state
        .crawl_started
        .map(|started| started.elapsed())
        .unwrap_or_default();
    match state.phase {
        RunPhase::Preparing => format!(
            "Preparing {} · {} pages · {}",
            state
                .run
                .as_ref()
                .map(|run| run.host.as_str())
                .unwrap_or("the site"),
            state.pages.len(),
            format_duration(elapsed)
        ),
        RunPhase::Crawling => format!("{} pages · {}", state.pages.len(), format_duration(elapsed)),
        RunPhase::Stopping => format!(
            "Stopping · {} pages · {}",
            state.pages.len(),
            format_duration(elapsed)
        ),
        _ => {
            if let Some(notice) = &state.export_notice {
                return match notice {
                    Ok(message) | Err(message) => message.clone(),
                };
            }
            if let Some(elapsed) = state.crawl_elapsed {
                format!("{} · {}", state.message, format_duration(elapsed))
            } else {
                state.message.clone()
            }
        }
    }
}

fn status_line(k: &Kit, state: &State) -> El {
    let message = status_message(state);
    let color =
        if state.phase.active() || matches!(state.phase, RunPhase::Stopped | RunPhase::Limited) {
            k.c.warning.fg
        } else if state.export_notice.as_ref().is_some_and(Result::is_err) || failed(state) {
            k.c.danger.fg
        } else if state.phase == RunPhase::Completed && !state.pages.is_empty() {
            k.c.success.fg
        } else {
            k.c.border_strong
        };
    row()
        .key("status")
        .w_full()
        .h(30.0)
        .px(GUTTER)
        .center_y()
        .gap(9.0)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_weak)
        .child(
            if !state.phase.active()
                && state.pages.is_empty()
                && state.export_notice.is_none()
                && state.message.starts_with("Ready")
            {
                row()
            } else {
                row()
                    .grow(1.0)
                    .min_w(0.0)
                    .center_y()
                    .gap(9.0)
                    .child(dot(color))
                    .child(
                        text(message.clone())
                            .font(TextStyle::new(10.5, 400.0).mono().lh(15.0).tracking(0.2))
                            .color(k.c.text_muted)
                            .grow(1.0)
                            .min_w(0.0)
                            .truncate()
                            .help_when_truncated(message),
                    )
            },
        )
        .child(el().grow(1.0))
        .child(
            text(update_status(state))
                .font(TextStyle::new(10.5, 400.0).mono().lh(15.0).tracking(0.2))
                .color(k.c.text_muted),
        )
        .child(
            text(format!("v{}", update::CURRENT_VERSION))
                .font(TextStyle::new(10.5, 400.0).mono().lh(15.0).tracking(0.2))
                .color(k.c.text_faint),
        )
}

fn update_status(state: &State) -> String {
    match &state.update {
        UpdateUi::Checking => "Checking for updates".into(),
        UpdateUi::UpToDate => "Up to date".into(),
        UpdateUi::Available(offered) => format!("{} available", offered.version),
        UpdateUi::Downloading { .. } => "Downloading update".into(),
        UpdateUi::Ready(_) => "Restart to update".into(),
        UpdateUi::Failed(_) => "Update failed".into(),
        UpdateUi::Installing => "Installing update".into(),
        UpdateUi::Idle => String::new(),
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    }
}

fn mime(content_type: &str) -> String {
    let mime = content_type.split(';').next().unwrap_or("").trim();
    if mime.is_empty() {
        "—".into()
    } else {
        mime.to_string()
    }
}

fn dash(value: &str) -> String {
    if value.is_empty() {
        "—".into()
    } else {
        value.to_string()
    }
}

fn switch_tab(state: &mut State, tab: Tab) {
    state.tab = tab;
    state.relation_pages = RelationPages::default();
    state.selection_epoch = state.selection_epoch.wrapping_add(1);
    let (column, dir) = state.view().sort.clone();
    state.table.set_sort(column, dir);
    sort_pages(state);
}

fn select_scene_url(state: &mut State, url: &str) {
    state.relation_pages = RelationPages::default();
    state.selection_epoch = state.selection_epoch.wrapping_add(1);
    state.view_mut().selected = Some(url.to_string());
    sort_pages(state);
    state.table.select(url.to_string());
}

fn scene_empty(_state: &mut State) {}
fn scene_sample(state: &mut State) {
    load_sample(state);
}
fn scene_titles(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Titles);
}
fn scene_meta(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Meta);
}
fn scene_h1(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::H1);
}
fn scene_response(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Response);
}
fn scene_canonical(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Canonical);
}
fn scene_images(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Images);
}
fn scene_links(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Links);
}
fn scene_sitemaps(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Sitemaps);
}
fn scene_sitemap_files(state: &mut State) {
    scene_sitemaps(state);
    state.sitemap_view = SitemapView::Files;
    let (column, dir) = state.view().sort.clone();
    state.table.set_sort(column, dir);
    sort_sitemap(state);
}

fn scene_sitemap_files_findings(state: &mut State) {
    scene_sitemap_files(state);
    state.view_mut().filter = CheckFilter::Findings;
    sort_sitemap(state);
}
fn scene_indexability(state: &mut State) {
    load_indexability_sample(state);
}

fn scene_filter_empty(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Titles);
    state.view_mut().query.replace_text("no-such-result");
    refresh_visible_forced(state);
}

fn scene_filter_findings(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Titles);
    state.view_mut().filter = CheckFilter::Findings;
    sort_pages(state);
}

fn scene_filter_incomplete(state: &mut State) {
    load_browser_sample(state);
    switch_tab(state, Tab::Internal);
    state.view_mut().filter = CheckFilter::Incomplete;
    sort_pages(state);
}

fn scene_titles_duplicate(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Titles);
    select_scene_url(state, "https://example.com/pricing");
    state.details_open = true;
}

fn scene_h1_multiple(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::H1);
    select_scene_url(state, "https://example.com/guides/launch");
    state.details_open = true;
}

fn scene_links_detail(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Links);
    select_scene_url(state, "https://example.com/");
    state.details_open = true;
}

fn scene_response_redirect(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Response);
    select_scene_url(state, "https://example.com/old-pricing");
    state.details_open = true;
}

fn scene_canonical_detail(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Canonical);
    select_scene_url(state, "https://example.com/blog/notes");
    state.details_open = true;
}

fn scene_images_detail(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Images);
    select_scene_url(state, "https://example.com/guides/launch");
    state.details_open = true;
}

fn scene_run_details(state: &mut State) {
    load_sample(state);
    state.run_details_open = true;
}

fn scene_run_snapshot(state: &mut State) {
    load_sample(state);
    state.use_sitemaps = false;
    state.url.replace_text("not a url");
    state.pages_limit.replace_text("0");
    state.depth_limit.replace_text("11");
    state.field_errors = validate_start(state).expect_err("invalid next setup");
    state.message = state.field_errors.summary();
    state.run_details_open = true;
}

fn scene_invalid_input(state: &mut State) {
    load_sample(state);
    state.pages_limit.replace_text("0");
    state.depth_limit.replace_text("11");
    state.url.replace_text("not a url");
    state.field_errors = validate_start(state).expect_err("invalid setup");
    state.message = state.field_errors.summary();
    focus_first_error(state);
}

fn scene_preparing(state: &mut State) {
    load_sample(state);
    state.phase = RunPhase::Preparing;
    state.crawl_started = Some(Instant::now());
    state.message = "Preparing https://example.com/…".into();
    state.pages.clear();
    state.visible.clear();
    state.search_index.clear();
    state.counts = Counts::default();
    state.view_counts = ViewCounts::default();
    state.table.clear_selection();
}

fn scene_stopping(state: &mut State) {
    load_sample(state);
    state.phase = RunPhase::Stopping;
    state.crawl_started = Some(Instant::now());
    state.message = "Stopping…".into();
}

fn scene_limited(state: &mut State) {
    load_sample(state);
    state.phase = RunPhase::Limited;
    state.crawl_elapsed = Some(Duration::from_millis(2_140));
    state.message = "9 pages on example.com · stopped at the 40 page limit".into();
}

fn scene_failed(state: &mut State) {
    state.phase = RunPhase::Failed;
    state.pages.clear();
    reset_inspector_dataset(state);
    state.search_index.clear();
    state.visible.clear();
    state.counts = Counts::default();
    state.view_counts = ViewCounts::default();
    state.sitemap = SitemapReport::default();
    state.run = Some(RunContext {
        seed: "https://example.com/".into(),
        host: "example.com".into(),
        limits: Limits {
            max_pages: DEFAULT_MAX_PAGES,
            max_depth: MAX_DEPTH,
        },
        rendering: RenderMode::Html,
        sitemaps: true,
    });
    state.message = "Crawl failed · no response".into();
}
fn scene_indexability_details(state: &mut State) {
    load_indexability_sample(state);
    select_scene_url(state, "https://example.com/wholesale");
    state.details_open = true;
}
fn browser_sample_status() -> BrowserStatus {
    BrowserStatus::Ready {
        path: PathBuf::from("/Applications/Google Chrome.app"),
        version: "Chrome/157.0.8103.40".into(),
    }
}

fn recapture(page: &mut Page, state: RenderState) {
    let complete = state == RenderState::Captured;
    let summary = RenderSummary {
        state,
        reason: match state {
            RenderState::TimedOut => {
                "The page did not reach DOMContentLoaded before the deadline.".into()
            }
            RenderState::Blocked => {
                "The document navigation was outside the crawl scope or blocked by robots.txt."
                    .into()
            }
            RenderState::Navigated => {
                "A script changed the document URL; the destination was queued separately.".into()
            }
            RenderState::Truncated => {
                "A resource limit stopped loading before the page was complete.".into()
            }
            _ => String::new(),
        },
        browser: "Chrome/157.0.8103.40".into(),
        observed_url: page.url.clone(),
        source_captured: true,
        source_state: SourceState::Captured,
        source_note: if complete {
            String::new()
        } else {
            "The rendered DOM did not finish; the source capture is shown instead.".into()
        },
        rendered_captured: complete,
        rendered_truncated: state == RenderState::Truncated,
        resource_failures: Vec::new(),
        navigation_to: String::new(),
        elapsed_ms: if complete { 1_840 } else { 15_000 },
        requests: 18,
    };
    page.indexability = calmtoad::indexability::assess(
        &page.url,
        page.status,
        &page.content_type,
        &page.redirect_to,
        &page.error,
        page.indexability.evidence.clone(),
        CaptureInfo {
            mode: CaptureMode::Browser,
            render: Some(summary),
        },
    );
    page.noindex = page.indexability.has_noindex();
    page.canonical = page
        .indexability
        .first_valid_canonical()
        .unwrap_or_default()
        .to_string();
}

fn load_browser_sample(state: &mut State) {
    load_indexability_sample(state);
    for page in &mut state.pages {
        // The sample's declarations came from a plain fetch. In a browser crawl the ones that
        // survive are rendered evidence.
        for directive in &mut page.indexability.evidence.directives {
            directive.stage = EvidenceStage::Rendered;
        }
        for declaration in &mut page.indexability.evidence.canonicals {
            declaration.stage = EvidenceStage::Rendered;
        }
    }
    for page in &mut state.pages {
        let capture = match page.url.as_str() {
            // A clean page that never finished rendering must not read as clean.
            "https://example.com/nofollow" => RenderState::TimedOut,
            "https://example.com/feed" => RenderState::Blocked,
            _ => RenderState::Captured,
        };
        recapture(page, capture);
    }
    // A declaration scripts replaced stays visible as source evidence.
    if let Some(page) = state
        .pages
        .iter_mut()
        .find(|page| page.url.ends_with("/campaign"))
        && let Ok(base) = url::Url::parse(&page.url)
    {
        page.indexability.evidence.canonicals.push(
            calmtoad::indexability::canonical_declaration_staged(
                Some("/campaign-source"),
                &base,
                EvidenceStage::Document,
            ),
        );
        recapture(page, RenderState::Captured);
    }
    for page in &mut state.pages {
        // Capture state changes affect completeness and canonical text; refresh the cached
        // search facts, audit and counts together.
        page.noindex = page.indexability.has_noindex();
        page.canonical = page
            .indexability
            .first_valid_canonical()
            .unwrap_or_default()
            .to_string();
    }
    state.search_index = state.pages.iter().map(page_search_text).collect();
    state.audit = Audit::from_pages(&state.pages);
    invalidate_inspector(state);
    state.counts_dirty = true;
    let _ = refresh_counts(state, true);
    sort_pages(state);
    refresh_sitemap_visible(state);
    state.rendering = RenderMode::Browser;
    if let Some(run) = &mut state.run {
        run.rendering = RenderMode::Browser;
    }
    state.browser_probe = None;
    state.browser = BrowserChoice {
        path: None,
        status: browser_sample_status(),
    };
}

fn scene_browser(state: &mut State) {
    load_browser_sample(state);
}

fn scene_browser_details(state: &mut State) {
    load_browser_sample(state);
    select_scene_url(state, "https://example.com/nofollow");
    state.details_open = true;
}

fn scene_browser_evidence(state: &mut State) {
    scene_browser_details(state);
    state.evidence_open = true;
    state.capture_open = true;
}

fn scene_browser_unavailable(state: &mut State) {
    state.rendering = RenderMode::Browser;
    state.browser_probe = None;
    state.browser = BrowserChoice {
        path: None,
        status: BrowserStatus::Unavailable {
            reason: "No Chrome or Chromium was found on this machine.".into(),
        },
    };
    state.message = "Ready · enter a site URL to begin. Up to 40 pages and 3 levels deep.".into();
}

fn scene_title_issues(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Titles);
    state.view_mut().filter = CheckFilter::Findings;
    sort_pages(state);
}
fn scene_error(state: &mut State) {
    state.phase = RunPhase::Failed;
    state.pages.clear();
    state.message = "Couldn’t read “not a url” as a URL.".into();
}
fn scene_crawling(state: &mut State) {
    state.pages.clear();
    state.phase = RunPhase::Crawling;
    state.message = "Crawling https://example.com/…".into();
}

fn scene_update_with_results(state: &mut State) {
    load_sample(state);
    switch_tab(state, Tab::Titles);
    state.view_mut().query.replace_text("example");
    refresh_visible_forced(state);
    state.update = UpdateUi::Available(update::AvailableUpdate {
        version: "0.2.0".into(),
        notes: "Faster crawls and a quieter update strip.".into(),
        release_url: Some("https://github.com/abrudtkuhl/calmtoad/releases/tag/v0.2.0".into()),
        asset: update::manifest::PlatformAsset {
            url: "https://github.com/abrudtkuhl/calmtoad/releases/download/v0.2.0/Calm-Toad-Mac-Apple-Silicon.zip".into(),
            sha256: "a".repeat(64),
            size: 8_123_456,
        },
    });
}

fn scene_update_ready_with_results(state: &mut State) {
    scene_update_with_results(state);
    state.update = UpdateUi::Ready(update::DownloadedUpdate::placeholder("0.2.0"));
}

fn scene_update_available(state: &mut State) {
    state.update = UpdateUi::Available(update::AvailableUpdate {
        version: "0.2.0".into(),
        notes: "Faster crawls and a quieter update strip.".into(),
        release_url: Some(
            "https://github.com/abrudtkuhl/calmtoad/releases/tag/v0.2.0".into(),
        ),
        asset: update::manifest::PlatformAsset {
            url: "https://github.com/abrudtkuhl/calmtoad/releases/download/v0.2.0/Calm-Toad-Mac-Apple-Silicon.zip".into(),
            sha256: "a".repeat(64),
            size: 8_123_456,
        },
    });
}

fn scene_update_downloading(state: &mut State) {
    state.update = UpdateUi::Downloading {
        version: "0.2.0".into(),
        received: 3_100_000,
        total: Some(8_123_456),
    };
}

fn scene_update_ready(state: &mut State) {
    state.update = UpdateUi::Ready(update::DownloadedUpdate::placeholder("0.2.0"));
}

fn scene_update_failed(state: &mut State) {
    state.update =
        UpdateUi::Failed("The update’s checksum did not match; it was discarded.".into());
}

fn scene_pmprompt(state: &mut State) {
    const SEED: &str = "https://pmprompt.com/";
    state.url.replace_text(SEED);
    state
        .pages_limit
        .replace_text(DEFAULT_MAX_PAGES.to_string());
    state.depth_limit.replace_text(MAX_DEPTH.to_string());
    let started = Instant::now();
    let outcome = crawl::crawl_site(SEED);
    state.max_pages = DEFAULT_MAX_PAGES;
    set_pages(state, outcome.pages, outcome.message);
    state.crawl_elapsed = Some(started.elapsed());
}

fn main() {
    if let Some(code) = update::install::apply_from_env() {
        std::process::exit(code);
    }
    if let Err(error) = rune::set_app_icon(include_bytes!("../assets/toad-app-icon.png").to_vec()) {
        eprintln!("Could not set Calm Toad app icon: {error}");
    }
    rune::App::new(AppSpec {
        scenes: &[
            ("empty", scene_empty),
            ("sample", scene_sample),
            ("titles", scene_titles),
            ("meta", scene_meta),
            ("h1", scene_h1),
            ("response", scene_response),
            ("canonical", scene_canonical),
            ("images", scene_images),
            ("links", scene_links),
            ("sitemaps", scene_sitemaps),
            ("sitemap-files", scene_sitemap_files),
            ("sitemap-files-findings", scene_sitemap_files_findings),
            ("indexability", scene_indexability),
            ("indexability-details", scene_indexability_details),
            ("filter-empty", scene_filter_empty),
            ("filter-findings", scene_filter_findings),
            ("filter-incomplete", scene_filter_incomplete),
            ("titles-duplicate", scene_titles_duplicate),
            ("h1-multiple", scene_h1_multiple),
            ("links-detail", scene_links_detail),
            ("response-redirect", scene_response_redirect),
            ("canonical-detail", scene_canonical_detail),
            ("images-detail", scene_images_detail),
            ("run-details", scene_run_details),
            ("run-snapshot", scene_run_snapshot),
            ("invalid-input", scene_invalid_input),
            ("preparing", scene_preparing),
            ("stopping", scene_stopping),
            ("limited", scene_limited),
            ("failed", scene_failed),
            ("browser", scene_browser),
            ("browser-details", scene_browser_details),
            ("browser-evidence", scene_browser_evidence),
            ("browser-unavailable", scene_browser_unavailable),
            ("title-issues", scene_title_issues),
            ("error", scene_error),
            ("crawling", scene_crawling),
            ("update-available", scene_update_available),
            ("update-with-results", scene_update_with_results),
            ("update-ready-with-results", scene_update_ready_with_results),
            ("update-downloading", scene_update_downloading),
            ("update-ready", scene_update_ready),
            ("update-failed", scene_update_failed),
            ("pmprompt", scene_pmprompt),
        ],
        setup: Some(setup),
        poll: Some(poll),
        min_size: Some((880.0, 560.0)),
        ..AppSpec::new("Calm Toad", (1180.0, 740.0), init, view)
    })
    .menu_item(
        MenuItem::new("Find in Results", focus_search_results)
            .menu("Edit")
            .key_equivalent("mod+f".parse::<Shortcut>().expect("valid shortcut"))
            .keys_first(),
    )
    .menu_item(
        MenuItem::new("Focus Site URL", focus_site_url)
            .menu("Edit")
            .key_equivalent("mod+l".parse::<Shortcut>().expect("valid shortcut"))
            .keys_first(),
    )
    .menu_item(
        MenuItem::new("Close Details", close_context)
            .menu("Edit")
            .key_equivalent("escape".parse::<Shortcut>().expect("valid shortcut")),
    )
    .menu_item(MenuItem::new("Check for Updates…", check_for_updates_menu))
    .on_close(app_close)
    .run();
    // `Tx::quit` closes without running `on_close`; this is the final ownership sweep.
    render::kill_owned_browsers();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Mods, Runtime};

    fn test_runtime(mut state: State) -> Runtime<State> {
        state.waker = None;
        Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        )
    }

    #[test]
    fn tabs_switch_and_issues_only_filters_clean_title_rows() {
        let mut state = init();
        load_sample(&mut state);
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        assert!(
            rt.find("toad-mark").is_some(),
            "the header shows the Calm Toad mark"
        );
        assert!(
            rt.find("export-csv").is_some(),
            "the results header exposes CSV export"
        );
        assert!(rt.find("grid").is_some(), "the results table is present");
        assert_eq!(rt.state.visible.len(), rt.state.pages.len());

        let (x, y) = rt.find("tab-response").expect("response tab");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.tab, Tab::Response);

        let (x, y) = rt.find("tab-titles").expect("titles tab");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.tab, Tab::Titles);

        let (x, y) = rt.find("filter-findings").expect("findings filter");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(rt.state.view().filter == CheckFilter::Findings);
        assert!(rt.state.visible.len() < rt.state.pages.len());
        assert!(rt.state.visible.iter().all(|index| {
            !issue_for(Tab::Titles, &rt.state.pages[*index], &rt.state.audit).is_empty()
        }));
    }

    #[test]
    fn a_rejected_url_replaces_the_empty_invitation() {
        let mut state = init();
        scene_error(&mut state);
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        assert!(rt.find("frame").is_some(), "results frame");
        assert!(rt.find("empty-title").is_some(), "error state is rendered");
        assert!(rt.state.pages.is_empty());
        assert!(!rt.state.phase.active());
    }

    #[test]
    fn csv_export_uses_visible_rows_in_the_active_tab_order() {
        let mut state = init();
        load_sample(&mut state);
        state.tab = Tab::Titles;
        state.view_mut().filter = CheckFilter::Findings;
        state.table.set_sort("title", SortDir::Descending);
        sort_pages(&mut state);

        let exported = state
            .visible
            .iter()
            .map(|index| export_row(state.tab, &state.pages[*index], &state.audit))
            .collect::<Vec<_>>();

        assert!(!exported.is_empty());
        assert_eq!(
            export_headers(state.tab),
            &["ADDRESS", "TITLE", "LENGTH", "ISSUE"]
        );
        assert_eq!(exported.len(), state.visible.len());
        assert_eq!(exported[0][0], state.pages[state.visible[0]].url);
        assert!(exported.iter().all(|row| row[3] != "OK"));
    }

    #[test]
    fn csv_export_path_always_uses_a_csv_extension() {
        assert_eq!(
            csv_path("crawl".into()),
            std::path::PathBuf::from("crawl.csv")
        );
        assert_eq!(
            csv_path("crawl.CSV".into()),
            std::path::PathBuf::from("crawl.CSV")
        );
        assert_eq!(
            csv_path("crawl.txt".into()),
            std::path::PathBuf::from("crawl.csv")
        );
    }

    #[test]
    fn install_waits_for_a_finished_crawl() {
        let mut state = init();
        scene_update_ready(&mut state);
        assert!(can_install(&state));

        state.phase = RunPhase::Crawling;
        assert!(!can_install(&state));

        state.phase = RunPhase::Completed;
        state.update = UpdateUi::Idle;
        assert!(!can_install(&state));
    }

    #[test]
    fn sitemaps_tab_switches_views_filters_and_exports() {
        let mut state = init();
        load_sample(&mut state);
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);

        let (x, y) = rt.find("tab-sitemaps").expect("sitemaps tab");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.tab, Tab::Sitemaps);
        assert_eq!(rt.state.sitemap_view, SitemapView::Urls);
        assert_eq!(rt.state.sitemap_visible.len(), rt.state.sitemap.urls.len());
        assert!(rt.find("sitemap-files").is_some(), "file view toggle");

        let (x, y) = rt.find("filter-findings").expect("findings filter");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(rt.state.view().filter == CheckFilter::Findings);
        assert!(!rt.state.sitemap_visible.is_empty());
        assert!(
            rt.state
                .sitemap_visible
                .iter()
                .all(|index| rt.state.sitemap.urls[*index].needs_attention())
        );
        assert_eq!(
            sitemap_export_headers(SitemapView::Urls),
            &["ADDRESS", "STATE", "STATUS", "SOURCE", "CAPTURE", "ISSUE"]
        );
        let url_rows = rt
            .state
            .sitemap_visible
            .iter()
            .map(|index| sitemap_url_row(&rt.state.sitemap.urls[*index]))
            .collect::<Vec<_>>();
        assert!(url_rows.iter().all(|row| row.len() == 6));
        assert!(
            url_rows.iter().all(|row| row[4] == "HTML"),
            "an HTML crawl has no browser provenance: {url_rows:?}"
        );

        let (x, y) = rt.find("sitemap-files").expect("file view toggle");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.sitemap_view, SitemapView::Files);
        // Each sitemap view keeps its own filter; apply it to Files deliberately.
        let (x, y) = rt.find("filter-findings").expect("findings filter");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(
            rt.state
                .sitemap_file_visible
                .iter()
                .all(|index| rt.state.sitemap.files[*index].needs_attention())
        );

        assert_eq!(
            sitemap_export_headers(rt.state.sitemap_view),
            &["ADDRESS", "ORIGIN", "STATE", "STATUS", "URLS", "ISSUE"]
        );
        let rows = rt
            .state
            .sitemap_file_visible
            .iter()
            .map(|index| sitemap_file_row(&rt.state.sitemap.files[*index]))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), rt.state.sitemap_file_visible.len());
        assert!(rows.iter().all(|row| row.len() == 6));
    }

    #[test]
    fn the_sample_sitemaps_show_mixed_truthful_outcomes() {
        let mut state = init();
        load_sample(&mut state);
        let summary = state.sitemap.summary();
        assert!(summary.files >= 5);
        assert!(summary.listed >= 8);
        assert!(summary.checked >= 4);
        assert!(summary.blocked >= 1);
        assert!(summary.skipped >= 1);
        assert!(summary.not_checked >= 1);
        assert!(summary.file_errors >= 2);
        assert!(state.sitemap.partial.is_some());
        assert!(state.sitemap.files.iter().any(|file| file.origin
            == calmtoad::sitemap::SitemapOrigin::Guessed
            && file.state == FileState::Absent));

        state.tab = Tab::Sitemaps;
        sort_pages(&mut state);
        assert_eq!(state.sitemap_visible.len(), state.sitemap.urls.len());
        state.view_mut().filter = CheckFilter::Findings;
        refresh_visible(&mut state);
        assert!(state.sitemap_visible.len() < state.sitemap.urls.len());
    }

    #[test]
    fn file_errors_are_explained_when_no_sitemap_urls_were_read() {
        let mut state = init();
        state.sitemap.discovery_done = true;
        state.sitemap.insert_file(SitemapFile {
            url: "https://example.com/sitemap.xml".into(),
            origin: calmtoad::sitemap::SitemapOrigin::Guessed,
            state: FileState::Error,
            status: 500,
            issue: "HTTP 500".into(),
            ..SitemapFile::default()
        });
        let (title, copy) = sitemap_empty(&state);
        assert_eq!(title, "No URLs listed");
        assert!(copy.contains("1 sitemap file failed"), "{copy}");
        assert!(copy.contains("Switch to Files"), "{copy}");
    }

    #[test]
    fn a_stopped_sitemap_read_is_not_reported_as_missing() {
        let mut state = init();
        state.sitemap.discovery_done = true;
        state.sitemap.partial = Some("Stopped".into());
        assert_eq!(sitemap_summary(&state), "Sitemaps incomplete · Stopped");
        let (title, copy) = sitemap_empty(&state);
        assert_eq!(title, "Sitemaps stopped");
        assert!(copy.contains("Stopped"), "{copy}");
    }

    #[test]
    fn an_unread_sitemap_report_does_not_claim_discovery_is_running() {
        let mut state = init();
        assert!(state.use_sitemaps);
        assert!(!state.sitemap.discovery_done);
        assert_eq!(sitemap_summary(&state), "Sitemaps not read");

        state.phase = RunPhase::Crawling;
        assert_eq!(sitemap_summary(&state), "Reading sitemaps");

        state.phase = RunPhase::Completed;
        state.use_sitemaps = false;
        state.sitemap.discovery_done = true;
        assert_eq!(sitemap_summary(&state), "Sitemap discovery is off");
    }

    #[test]
    fn indexability_drives_filtering_sorting_and_csv_columns() {
        let mut state = init();
        load_indexability_sample(&mut state);
        let clean = state
            .pages
            .iter()
            .position(|page| page.indexability.label == Label::NoBlockFound)
            .expect("the mixed fixture keeps a clean page");
        state.view_mut().filter = CheckFilter::Findings;
        refresh_visible(&mut state);
        assert!(!state.visible.contains(&clean));
        assert!(state.visible.len() < state.pages.len());
        assert!(
            state
                .visible
                .iter()
                .all(|index| state.pages[*index].indexability.needs_attention())
        );

        state.view_mut().filter = CheckFilter::All;
        state.table.set_sort("indexability", SortDir::Ascending);
        sort_pages(&mut state);
        let ranks: Vec<u8> = state
            .visible
            .iter()
            .map(|index| label_rank(state.pages[*index].indexability.label))
            .collect();
        assert!(ranks.windows(2).all(|pair| pair[0] <= pair[1]));

        assert_eq!(
            export_headers(Tab::Internal),
            &[
                "ADDRESS",
                "STATUS",
                "INDEXABILITY",
                "CAPTURE",
                "WORDS",
                "REASONS",
                "EVIDENCE",
            ]
        );
        let wholesale = state
            .pages
            .iter()
            .find(|page| page.url.ends_with("/wholesale"))
            .expect("wholesale fixture");
        let row = export_row(Tab::Internal, wholesale, &state.audit);
        assert_eq!(row[2], "Noindex");
        assert_eq!(row[3], "HTML", "an HTTP capture has no browser provenance");
        assert!(row[5].contains("noindex"));
        assert!(
            row[6].contains("Googlebot meta") && row[6].contains("Bingbot meta"),
            "{}",
            row[6]
        );
    }

    #[test]
    fn non_html_is_context_not_an_issue_in_rows_csv_sitemaps_or_counts() {
        let mut state = init();
        load_indexability_sample(&mut state);

        let pdf_index = state
            .pages
            .iter()
            .position(|page| page.url.ends_with("/report-2026.pdf"))
            .expect("PDF fixture");
        {
            let pdf = &state.pages[pdf_index];
            assert_eq!(pdf.indexability.label, Label::NotAssessed);
            assert!(!pdf.indexability.needs_attention());
            assert!(!pdf.indexability.has_confirmed_issue());
        }

        // Findings excludes it even though its context is still readable.
        state.view_mut().filter = CheckFilter::Findings;
        refresh_visible(&mut state);
        assert!(!state.visible.contains(&pdf_index));
        assert!(
            state
                .visible
                .iter()
                .all(|index| state.pages[*index].indexability.needs_attention())
        );

        // CSV keeps the Not assessed label and explains it.
        let row = export_row(Tab::Internal, &state.pages[pdf_index], &state.audit);
        assert_eq!(row[2], "Not assessed");
        assert!(row[5].contains("Non-HTML content"), "{}", row[5]);

        // Sitemaps use the same severity: the row still explains itself, but it is not an
        // issue row and does not inflate the issue count.
        let entry = state
            .sitemap
            .url("https://example.com/report.pdf")
            .expect("sitemap PDF entry");
        assert_eq!(entry.state, UrlState::Checked);
        assert!(!entry.needs_attention());
        assert!(entry.issue.contains("Non-HTML content"), "{}", entry.issue);
        let summary = state.sitemap.summary();
        let issue_count = state
            .sitemap
            .urls
            .iter()
            .filter(|entry| entry.needs_attention())
            .count();
        assert_eq!(summary.issue_urls, issue_count);

        // Confirmed-error counts exclude it too.
        let blocked = counts_with(&state.pages, &state.audit).blocked;
        let confirmed = state
            .pages
            .iter()
            .filter(|page| page.indexability.has_confirmed_issue())
            .count();
        assert_eq!(blocked, confirmed);
        assert!(confirmed > 0);
    }

    #[test]
    fn rendering_mode_is_chosen_in_the_bar_and_reported_in_csv() {
        let mut state = init();
        scene_browser(&mut state);
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        assert_eq!(rt.state.rendering, RenderMode::Browser);
        assert!(rt.find("browser-strip").is_some());
        assert!(rt.find("choose-browser").is_some());

        let (x, y) = rt.find("segment0").expect("HTML segment");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.rendering, RenderMode::Html);
        assert!(
            rt.find("browser-strip").is_none(),
            "the setup strip belongs to Browser mode"
        );

        let nofollow = rt
            .state
            .pages
            .iter()
            .position(|page| page.url.ends_with("/nofollow"))
            .expect("timed-out browser sample page");
        let row = export_row(Tab::Internal, &rt.state.pages[nofollow], &rt.state.audit);
        assert_eq!(row[2], "Not assessed");
        assert!(row[3].starts_with("Browser · Timed out"), "{}", row[3]);
        assert!(
            row[3].contains("deadline"),
            "the export keeps the stop reason: {}",
            row[3]
        );
        assert!(
            rt.state.pages[nofollow]
                .indexability
                .findings
                .iter()
                .any(|finding| {
                    finding.code == calmtoad::indexability::FindingCode::CaptureIncomplete
                })
        );
    }

    #[test]
    fn detail_disclosures_reveal_evidence_and_capture_without_leaving_the_page() {
        let mut state = init();
        scene_browser_details(&mut state);
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        assert!(rt.find("evidence-content").is_none());
        assert!(rt.find("capture-content").is_none());
        let (x, y) = rt.find("capture-toggle").expect("capture disclosure");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(rt.find("capture-content").is_some());
        let (x, y) = rt.find("evidence-toggle").expect("evidence disclosure");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(rt.find("evidence-content").is_some());
        assert!(rt.state.details_open);
        assert_eq!(
            rt.state.table.cursor(),
            Some("https://example.com/nofollow")
        );
    }

    #[test]
    fn footer_keeps_the_stop_reason_when_showing_elapsed_time() {
        let mut state = init();
        state.crawl_elapsed = Some(Duration::from_secs(2));
        for reason in [
            "Stopped · 12 pages on example.com",
            "40 pages on example.com · page limit reached",
        ] {
            state.message = reason.into();
            let message = status_message(&state);
            assert!(message.starts_with(reason), "{message}");
            assert!(message.ends_with("2.0s"), "{message}");
        }
    }

    #[test]
    fn rendering_mode_cannot_change_mid_crawl() {
        let mut state = init();
        scene_browser(&mut state);
        state.phase = RunPhase::Crawling;
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        let (x, y) = rt.find("segment0").expect("HTML segment");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(
            rt.state.rendering,
            RenderMode::Browser,
            "the running crawl's capture mode stays fixed"
        );
    }

    #[test]
    fn browser_executable_cannot_change_mid_crawl() {
        let mut state = init();
        scene_browser(&mut state);
        state.phase = RunPhase::Crawling;
        let message = state.message.clone();
        let rendering = state.rendering;
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        let (x, y) = rt
            .find("choose-browser")
            .expect("the browser chooser keeps its place");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(
            rt.state.rendering, rendering,
            "the running crawl keeps its browser setting"
        );
        assert_eq!(
            rt.state.message, message,
            "the disabled chooser does not change the run's message"
        );
        assert!(rt.state.browser_probe.is_none());
    }

    #[test]
    fn closing_the_app_stops_the_running_crawl() {
        let mut state = init();
        let (_tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        state.job = Some(CrawlJob {
            rx,
            cancel: cancel.clone(),
            stopping: false,
        });
        stop_owned_work(&mut state);
        assert!(
            cancel.load(Ordering::Relaxed),
            "app close cancels the detached crawl worker"
        );
    }

    #[test]
    fn incomplete_browser_rows_do_not_export_content_as_ok() {
        let mut state = init();
        load_browser_sample(&mut state);
        let page = state
            .pages
            .iter()
            .find(|page| page.url.ends_with("/nofollow"))
            .expect("incomplete browser fixture");
        assert!(!auditable(page));
        let row = export_row(Tab::H1, page, &state.audit);
        assert_eq!(row[2], "—", "the H1 count stays unclaimed");
        assert_eq!(row[3], "—", "the H1 audit stays unclaimed");
        let internal = export_row(Tab::Internal, page, &state.audit);
        assert!(
            internal[3].contains("Timed out"),
            "the capture column carries the incomplete state: {}",
            internal[3]
        );
    }

    #[test]
    fn a_rendered_page_with_missing_source_does_not_export_content_as_ok() {
        use calmtoad::indexability::{
            CaptureInfo, CaptureMode, PageEvidence, ReadState, RenderSummary,
        };
        use calmtoad::render::{RenderState, SourceState};

        let url = "https://example.com/source-missing".to_string();
        let assessment = calmtoad::indexability::assess(
            &url,
            200,
            "text/html",
            "",
            "",
            PageEvidence {
                read: ReadState::Complete,
                ..PageEvidence::default()
            },
            CaptureInfo {
                mode: CaptureMode::Browser,
                render: Some(RenderSummary {
                    state: RenderState::Captured,
                    reason: String::new(),
                    browser: "Chrome/test".into(),
                    observed_url: url.clone(),
                    source_captured: false,
                    source_state: SourceState::Unavailable,
                    source_note: "The source body was not captured.".into(),
                    rendered_captured: true,
                    rendered_truncated: false,
                    resource_failures: Vec::new(),
                    navigation_to: String::new(),
                    elapsed_ms: 120,
                    requests: 4,
                }),
            },
        );
        let page = Page {
            url: url.clone(),
            status: 200,
            content_type: "text/html".into(),
            title: "Rendered title".into(),
            meta_description: String::new(),
            h1s: vec!["Rendered heading".into()],
            noindex: assessment.has_noindex(),
            words: 2,
            redirect_to: String::new(),
            error: String::new(),
            depth: 0,
            canonical: String::new(),
            images: 0,
            images_missing_alt: 0,
            links: Vec::new(),
            indexability: assessment,
        };
        assert_eq!(page.indexability.label, Label::NotAssessed);
        assert!(!auditable(&page));
        let row = export_row(Tab::H1, &page, &Audit::default());
        assert_eq!(row[2], "—", "the H1 count is not claimed");
        assert_eq!(row[3], "—", "the H1 audit is not claimed");
        let internal = export_row(Tab::Internal, &page, &Audit::default());
        assert!(
            internal[3].contains("incomplete") && internal[3].contains("source"),
            "the capture column names the missing source stage: {}",
            internal[3]
        );
    }

    #[test]
    fn incomplete_links_rows_do_not_export_or_display_ok() {
        let mut state = init();
        load_browser_sample(&mut state);
        let page = state
            .pages
            .iter()
            .find(|page| page.url.ends_with("/nofollow"))
            .expect("incomplete browser fixture");
        assert!(!auditable(page));
        assert!(matches!(
            row_issue(Tab::Links, page, &state.audit),
            RowIssue::Unclaimed
        ));
        let row = export_row(Tab::Links, page, &state.audit);
        assert_eq!(row[2], "—", "the outbound count stays unclaimed");
        assert_eq!(row[3], "—", "an incomplete link check is not OK");

        // A source-only capture (rendered DOM absent) is equally unclaimed.
        let mut source_only = (*page).clone();
        if let Some(render) = source_only.indexability.capture.render.as_mut() {
            render.state = RenderState::Navigated;
            render.rendered_captured = false;
            render.source_state = SourceState::Captured;
        }
        assert!(!auditable(&source_only));
        assert!(matches!(
            row_issue(Tab::Links, &source_only, &state.audit),
            RowIssue::Unclaimed
        ));
        let source_row = export_row(Tab::Links, &source_only, &state.audit);
        assert_eq!(source_row[2], "—");
        assert_eq!(source_row[3], "—");
    }

    #[test]
    fn an_incomplete_page_keeps_an_observed_broken_link() {
        let mut state = init();
        load_browser_sample(&mut state);
        let mut page = state
            .pages
            .iter()
            .find(|page| page.url.ends_with("/nofollow"))
            .expect("incomplete browser fixture")
            .clone();
        page.links = vec!["https://example.com/missing-target".into()];
        let mut broken = page.clone();
        broken.url = "https://example.com/missing-target".into();
        broken.status = 404;
        let audit = Audit::from_pages(&[page.clone(), broken]);

        let issue = match row_issue(Tab::Links, &page, &audit) {
            RowIssue::Checked(issue) => issue,
            RowIssue::Unclaimed => panic!("an observed broken link is not hidden"),
        };
        assert!(issue.contains("Broken link to"), "{issue}");
        let row = export_row(Tab::Links, &page, &audit);
        assert_eq!(row[2], "—", "the outbound count stays unclaimed");
        assert_eq!(row[3], issue, "the observed broken link is exported");
    }

    #[test]
    fn a_failed_script_does_not_export_content_as_ok() {
        use calmtoad::indexability::{CaptureMode, PageEvidence, ReadState, RenderSummary};
        use calmtoad::render::{RenderState, ResourceFailure, SourceState};

        let url = "https://example.com/failed-script".to_string();
        let assessment = calmtoad::indexability::assess(
            &url,
            200,
            "text/html",
            "",
            "",
            PageEvidence {
                read: ReadState::Complete,
                ..PageEvidence::default()
            },
            CaptureInfo {
                mode: CaptureMode::Browser,
                render: Some(RenderSummary {
                    state: RenderState::Captured,
                    reason: String::new(),
                    browser: "Chrome/test".into(),
                    observed_url: url.clone(),
                    source_captured: true,
                    source_state: SourceState::Captured,
                    source_note: String::new(),
                    rendered_captured: true,
                    rendered_truncated: false,
                    resource_failures: vec![ResourceFailure {
                        url: "https://example.com/app.js".into(),
                        resource_type: "Script".into(),
                        error: "net::ERR_CONTENT_LENGTH_MISMATCH".into(),
                    }],
                    navigation_to: String::new(),
                    elapsed_ms: 200,
                    requests: 5,
                }),
            },
        );
        let page = Page {
            url: url.clone(),
            status: 200,
            content_type: "text/html".into(),
            title: "Unhydrated shell".into(),
            meta_description: String::new(),
            h1s: vec!["Shell heading".into()],
            noindex: assessment.has_noindex(),
            words: 2,
            redirect_to: String::new(),
            error: String::new(),
            depth: 0,
            canonical: String::new(),
            images: 0,
            images_missing_alt: 0,
            links: Vec::new(),
            indexability: assessment,
        };
        assert_eq!(page.indexability.label, Label::NotAssessed);
        assert!(!auditable(&page));
        assert!(matches!(
            row_issue(Tab::Titles, &page, &Audit::default()),
            RowIssue::Unclaimed
        ));
        assert!(matches!(
            row_issue(Tab::Links, &page, &Audit::default()),
            RowIssue::Unclaimed
        ));
        let h1 = export_row(Tab::H1, &page, &Audit::default());
        assert_eq!(h1[2], "—");
        assert_eq!(h1[3], "—");
        let internal = export_row(Tab::Internal, &page, &Audit::default());
        assert!(
            internal[3].contains("script failed"),
            "the capture column names the failure: {}",
            internal[3]
        );
    }

    #[test]
    fn browser_mode_cannot_start_without_a_usable_browser() {
        let mut state = init();
        scene_browser_unavailable(&mut state);
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        assert!(rt.find("browser-strip").is_some());
        assert!(!rt.state.browser.status.is_ready());
        let (x, y) = rt.find("crawl").expect("crawl button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(
            !rt.state.phase.active(),
            "Crawl stays disabled until Browser mode can launch"
        );
    }

    #[test]
    fn invalid_setup_keeps_results_and_reports_fields() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        select_scene_url(&mut state, "https://example.com/pricing");
        let before = state.pages.len();
        let selected = state.table.cursor().map(str::to_string);
        let run_pages = state.run.as_ref().map(|run| run.limits.max_pages);
        state.url.replace_text("not a url");
        state.pages_limit.replace_text("0");
        state.depth_limit.replace_text("11");
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        let (x, y) = rt.find("crawl").expect("crawl button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.pages.len(), before, "results are not cleared");
        assert_eq!(rt.state.table.cursor().map(str::to_string), selected);
        assert_eq!(
            rt.state.run.as_ref().map(|run| run.limits.max_pages),
            run_pages,
            "the previous run context is preserved"
        );
        assert!(!rt.state.field_errors.url.is_empty());
        assert!(!rt.state.field_errors.pages.is_empty());
        assert!(!rt.state.field_errors.depth.is_empty());
        assert!(!rt.state.phase.active(), "no work was started");
        assert!(rt.find("setup-error").is_some(), "the error is visible");
        assert!(rt.state.url.focused, "the first invalid field takes focus");
    }

    #[test]
    fn unavailable_browser_blocks_every_start_path_without_clearing_results() {
        let mut state = init();
        load_sample(&mut state);
        let before = state.pages.len();
        state.rendering = RenderMode::Browser;
        state.browser_probe = None;
        state.browser = BrowserChoice {
            path: None,
            status: BrowserStatus::Unavailable {
                reason: "No browser".into(),
            },
        };
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        let (x, y) = rt.find("crawl").expect("crawl button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.pages.len(), before);
        assert!(!rt.state.phase.active());
        start_crawl(&mut rt.state);
        assert!(
            !rt.state.field_errors.setup.is_empty(),
            "the shared start action reports the unusable browser"
        );
        assert!(
            !rt.state.phase.active(),
            "keyboard/direct start uses the same guard"
        );
        assert_eq!(rt.state.pages.len(), before);
    }

    #[test]
    fn search_filters_sort_and_exports_share_one_visible_order() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        state.view_mut().query.replace_text("CALM");
        refresh_visible_forced(&mut state);
        assert!(!state.visible.is_empty());
        assert!(state.visible.iter().all(|index| {
            page_search_text(&state.pages[*index]).contains("calm")
                || issue_for(Tab::Titles, &state.pages[*index], &state.audit)
                    .to_lowercase()
                    .contains("calm")
        }));

        // Unicode case-insensitive matching uses the cached lowercase facts.
        state.pages[0].title = "Éclair guide".into();
        state.search_index = state.pages.iter().map(page_search_text).collect();
        state.view_mut().query.replace_text("éclair");
        refresh_visible_forced(&mut state);
        assert_eq!(state.visible.len(), 1);
        assert_eq!(state.pages[state.visible[0]].url, "https://example.com/");

        state.view_mut().query.replace_text("calm");
        state.view_mut().filter = CheckFilter::Findings;
        state.view_mut().sort = ("url".into(), SortDir::Descending);
        sort_pages(&mut state);
        assert!(state.visible.iter().all(|index| page_has_finding(
            Tab::Titles,
            &state.pages[*index],
            &state.audit
        )));
        let urls = visible_ids(&state);
        let mut expected = urls.clone();
        expected.sort_by(|a, b| b.cmp(a));
        assert_eq!(urls, expected, "the visible order is the sorted order");
        let exported: Vec<String> = state
            .visible
            .iter()
            .map(|index| export_row(Tab::Titles, &state.pages[*index], &state.audit)[0].clone())
            .collect();
        assert_eq!(exported, urls, "CSV and Copy use the same rows");

        // New streamed rows join the visible set without rebuilding static facts.
        state.phase = RunPhase::Crawling;
        let mut page = state.pages[0].clone();
        page.url = "https://example.com/streamed".into();
        page.title = "A streamed CALM page".into();
        state.search_index.push(page_search_text(&page));
        state.audit.push(&page);
        state.pages.push(page);
        refresh_visible(&mut state);
        assert!(visible_ids(&state).contains(&"https://example.com/streamed".into()));
    }

    #[test]
    fn zero_matches_and_zero_findings_offer_recovery_not_health_claims() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        state.view_mut().query.replace_text("no-such-result");
        refresh_visible_forced(&mut state);
        assert!(state.visible.is_empty());
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        assert!(rt.find("clear-search").is_some());
        assert!(rt.find("empty-title").is_some());

        rt.state.view_mut().query.replace_text("");
        rt.state.view_mut().filter = CheckFilter::Findings;
        refresh_visible_forced(&mut rt.state);
        rt.frame(0.016);
        if rt.state.visible.is_empty() {
            assert!(rt.find("clear-filter").is_some());
        }
    }

    #[test]
    fn selection_survives_sort_and_filter_changes_and_stays_in_the_visible_set() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        select_scene_url(&mut state, "https://example.com/pricing");
        let pages_before: Vec<String> = state.pages.iter().map(|page| page.url.clone()).collect();
        state.view_mut().sort = ("url".into(), SortDir::Descending);
        sort_pages(&mut state);
        assert_eq!(
            pages_before,
            state
                .pages
                .iter()
                .map(|page| page.url.clone())
                .collect::<Vec<_>>(),
            "sorting does not permute the underlying page store"
        );
        assert_eq!(
            state.table.cursor(),
            Some("https://example.com/pricing"),
            "a still-visible selection survives sorting"
        );
        state.view_mut().query.replace_text("example.com/blog/calm");
        refresh_visible_forced(&mut state);
        assert_ne!(state.table.cursor(), Some("https://example.com/pricing"));
        assert_eq!(
            state.table.cursor(),
            visible_id(&state, 0).as_deref(),
            "a hidden selection moves to the first visible row"
        );
        assert_eq!(
            state.view().selected.as_deref(),
            state.table.cursor(),
            "the stored selection agrees with the table"
        );
    }

    #[test]
    fn sitemap_files_keep_file_selection_and_never_select_a_page() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Sitemaps);
        state.sitemap_view = SitemapView::Files;
        let file = state
            .sitemap
            .files
            .first()
            .map(|file| file.url.clone())
            .expect("sample sitemap file");
        state.view_mut().selected = Some(file.clone());
        sort_sitemap(&mut state);
        assert_eq!(state.table.cursor(), Some(file.as_str()));
        let before = state.table.cursor().map(str::to_string);
        let (tx, rx) = mpsc::channel();
        tx.send(CrawlEvent::Done(
            "done".into(),
            Box::new(calmtoad::sitemap::sample_report()),
            CrawlOutcome::Completed,
        ))
        .unwrap();
        state.job = Some(CrawlJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
        });
        poll_crawl(&mut state);
        assert_eq!(state.phase, RunPhase::Completed);
        assert_eq!(state.table.cursor().map(str::to_string), before);
        assert!(
            state
                .sitemap
                .files
                .iter()
                .any(|file| Some(file.url.as_str()) == state.table.cursor())
        );
    }

    #[test]
    fn each_view_keeps_its_own_query_filter_sort_and_selection() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        state.view_mut().query.replace_text("calm");
        state.view_mut().filter = CheckFilter::Findings;
        state.view_mut().sort = ("url".into(), SortDir::Descending);
        sort_pages(&mut state);
        let titles_visible = state.visible.clone();
        let titles_selected = state.view().selected.clone();
        assert!(titles_visible.is_empty(), "the sample has no calm finding");
        assert!(titles_selected.is_none());

        switch_tab(&mut state, Tab::Meta);
        assert!(state.view().query.text.is_empty());
        assert_eq!(state.view().filter, CheckFilter::All);
        state.view_mut().query.replace_text("description");
        refresh_visible_forced(&mut state);

        switch_tab(&mut state, Tab::Titles);
        assert_eq!(state.view().query.text, "calm");
        assert_eq!(state.view().filter, CheckFilter::Findings);
        assert_eq!(state.view().sort, ("url".into(), SortDir::Descending));
        assert_eq!(state.visible, titles_visible);
        assert_eq!(
            state.view().selected,
            titles_selected,
            "the view's selection is restored"
        );

        switch_tab(&mut state, Tab::Sitemaps);
        state.sitemap_view = SitemapView::Urls;
        state.view_mut().query.replace_text("pricing");
        refresh_sitemap_visible_inner(&mut state, true);
        let urls_visible = state.sitemap_visible.clone();
        state.sitemap_view = SitemapView::Files;
        sort_sitemap(&mut state);
        assert!(state.view().query.text.is_empty());
        state.view_mut().filter = CheckFilter::Findings;
        refresh_sitemap_visible_inner(&mut state, true);
        state.sitemap_view = SitemapView::Urls;
        sort_sitemap(&mut state);
        assert_eq!(state.view().query.text, "pricing");
        assert_eq!(state.sitemap_visible, urls_visible);

        // A new run clears transient result searches and selections in every view.
        state.view_mut().query.replace_text("pricing");
        state.view_mut().filter = CheckFilter::Findings;
        reset_view_searches(&mut state);
        assert!(state.views.iter().all(|view| view.query.text.is_empty()));
        assert!(
            state
                .views
                .iter()
                .all(|view| view.filter == CheckFilter::All && view.selected.is_none())
        );
        switch_tab(&mut state, Tab::Titles);
        assert_eq!(
            state.view().sort,
            ("url".into(), SortDir::Descending),
            "intentional sort choices are not transient"
        );
    }

    #[test]
    fn copy_visible_urls_matches_the_visible_order_and_scope() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        state.view_mut().filter = CheckFilter::Findings;
        state.view_mut().sort = ("url".into(), SortDir::Ascending);
        sort_pages(&mut state);
        let expected = visible_ids(&state);
        assert!(!expected.is_empty());
        let clipboard = rune::clipboard::memory();
        let mut rt = test_runtime(state);
        rt.use_clipboard(&clipboard);
        rt.frame(0.016);
        let (x, y) = rt.find("copy-urls").expect("copy visible URLs");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        let copied = clipboard
            .text()
            .map(|text| text.to_string())
            .unwrap_or_default();
        assert_eq!(copied.lines().collect::<Vec<_>>(), expected);
        assert!(
            rt.state
                .action_notice
                .as_deref()
                .is_some_and(|notice| notice.contains(&expected.len().to_string()))
        );
    }

    #[test]
    fn sitemap_search_filter_and_exports_share_each_view_order() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Sitemaps);
        state.view_mut().query.replace_text("example");
        state.view_mut().filter = CheckFilter::Findings;
        state.view_mut().sort = ("url".into(), SortDir::Ascending);
        sort_sitemap(&mut state);
        let urls = visible_ids(&state);
        let rows: Vec<String> = state
            .sitemap_visible
            .iter()
            .map(|index| sitemap_url_row(&state.sitemap.urls[*index])[0].clone())
            .collect();
        assert_eq!(urls, rows, "sitemap URLs export the visible order");

        state.sitemap_view = SitemapView::Files;
        state.view_mut().query.replace_text("sitemap");
        state.view_mut().filter = CheckFilter::Findings;
        sort_sitemap(&mut state);
        let files = visible_ids(&state);
        let file_rows: Vec<String> = state
            .sitemap_file_visible
            .iter()
            .map(|index| sitemap_file_row(&state.sitemap.files[*index])[0].clone())
            .collect();
        assert_eq!(files, file_rows, "sitemap files export the visible order");

        // A query that matches nothing is an empty view, not an empty dataset.
        state.view_mut().query.replace_text("no-such-sitemap");
        refresh_sitemap_visible_inner(&mut state, true);
        assert!(visible_ids(&state).is_empty());
        assert!(!state.sitemap.files.is_empty());
    }

    #[test]
    fn crawl_completion_preserves_a_still_visible_selection() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        select_scene_url(&mut state, "https://example.com/pricing");
        let selected = state.view().selected.clone();
        let (tx, rx) = mpsc::channel();
        tx.send(CrawlEvent::Done(
            "completed".into(),
            Box::new(calmtoad::sitemap::sample_report()),
            CrawlOutcome::Completed,
        ))
        .unwrap();
        state.job = Some(CrawlJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
        });
        poll_crawl(&mut state);
        assert_eq!(state.phase, RunPhase::Completed);
        assert_eq!(state.view().selected, selected);
        assert_eq!(
            state.table.cursor(),
            selected.as_deref(),
            "completion does not jump to the first page"
        );
    }

    fn click_key(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
    }

    #[test]
    fn sitemap_header_clicks_and_streamed_rows_keep_each_view_order() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Sitemaps);
        let mut rt = test_runtime(state);
        rt.frame(0.016);

        click_key(&mut rt, "header/status");
        assert_eq!(rt.state.view().sort, ("status".into(), SortDir::Ascending));
        assert_eq!(rt.state.table.sort(), Some(("status", SortDir::Ascending)));
        click_key(&mut rt, "header/status");
        assert_eq!(rt.state.view().sort, ("status".into(), SortDir::Descending));
        let before = visible_ids(&rt.state);
        rt.state.phase = RunPhase::Crawling;
        rt.state.crawl_started = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        rt.state.job = Some(CrawlJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
        });
        let mut entry = rt.state.sitemap.urls[0].clone();
        entry.url = "https://example.com/sitemap-streamed".into();
        entry.issue = "streamed".into();
        let _ = tx.send(CrawlEvent::SitemapUrl(entry.clone()));
        poll_crawl(&mut rt.state);
        assert_eq!(
            visible_ids(&rt.state)[..before.len()],
            before[..],
            "a streamed sitemap URL does not reorder existing rows"
        );

        click_key(&mut rt, "sitemap-files");
        assert_eq!(rt.state.sitemap_view, SitemapView::Files);
        click_key(&mut rt, "header/state");
        assert_eq!(rt.state.view().sort, ("state".into(), SortDir::Ascending));
        let before = visible_ids(&rt.state);
        let mut file = rt.state.sitemap.files[0].clone();
        file.url = "https://example.com/sitemap-streamed.xml".into();
        file.state = FileState::Ok;
        let _ = tx.send(CrawlEvent::SitemapFile(file));
        poll_crawl(&mut rt.state);
        assert_eq!(
            visible_ids(&rt.state)[..before.len()],
            before[..],
            "a streamed sitemap file does not reorder existing rows"
        );

        let _ = tx.send(CrawlEvent::Done(
            "completed".into(),
            Box::new(calmtoad::sitemap::sample_report()),
            CrawlOutcome::Completed,
        ));
        poll_crawl(&mut rt.state);
        assert_eq!(rt.state.phase, RunPhase::Completed);
        assert!(
            rt.state
                .sitemap
                .files
                .iter()
                .any(|file| Some(file.url.as_str()) == rt.state.table.cursor())
        );
    }
    #[test]
    fn header_clicks_set_the_effective_sort_and_keep_streamed_order_stable() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        let mut rt = test_runtime(state);
        rt.frame(0.016);

        // Numeric column: both click directions reach the view model and the visible order.
        click_key(&mut rt, "header/len");
        assert_eq!(rt.state.table.sort(), Some(("len", SortDir::Ascending)));
        assert_eq!(rt.state.view().sort, ("len".into(), SortDir::Ascending));
        let lengths: Vec<usize> = rt
            .state
            .visible
            .iter()
            .map(|index| rt.state.pages[*index].title.chars().count())
            .collect();
        assert!(lengths.windows(2).all(|pair| pair[0] <= pair[1]));
        click_key(&mut rt, "header/len");
        assert_eq!(rt.state.table.sort(), Some(("len", SortDir::Descending)));
        assert_eq!(rt.state.view().sort, ("len".into(), SortDir::Descending));
        let changed_order = visible_ids(&rt.state);
        let exported_order: Vec<String> = rt
            .state
            .visible
            .iter()
            .map(|index| {
                export_row(Tab::Titles, &rt.state.pages[*index], &rt.state.audit)[0].clone()
            })
            .collect();
        assert_eq!(
            exported_order, changed_order,
            "CSV follows the header-clicked order"
        );

        // URL and issue headers use the same seam.
        click_key(&mut rt, "header/url");
        assert_eq!(rt.state.view().sort, ("url".into(), SortDir::Ascending));
        assert_eq!(rt.state.table.sort(), Some(("url", SortDir::Ascending)));
        click_key(&mut rt, "header/issue");
        assert_eq!(rt.state.view().sort, ("issue".into(), SortDir::Ascending));

        // Per-view restoration: Titles keeps its column when another tab changes its own.
        click_key(&mut rt, "header/len");
        let expected_titles_sort = rt.state.view().sort.clone();
        let titles_rows = visible_ids(&rt.state);
        click_key(&mut rt, "tab-meta");
        click_key(&mut rt, "header/issue");
        assert_eq!(rt.state.view().sort, ("issue".into(), SortDir::Descending));
        click_key(&mut rt, "tab-titles");
        assert_eq!(rt.state.view().sort, expected_titles_sort);
        assert_eq!(
            rt.state.table.sort(),
            Some((expected_titles_sort.0.as_str(), expected_titles_sort.1))
        );
        assert_eq!(visible_ids(&rt.state), titles_rows);
        // Make the chosen direction descending before observing streamed arrivals.
        click_key(&mut rt, "header/len");
        assert_eq!(rt.state.view().sort, ("len".into(), SortDir::Descending));

        // Streaming bursts append without resetting the existing order.
        rt.state.phase = RunPhase::Crawling;
        rt.state.crawl_started = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        rt.state.job = Some(CrawlJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
        });
        let before = visible_ids(&rt.state);
        let mut page = rt.state.pages[0].clone();
        page.url = "https://example.com/zz-streamed".into();
        page.title = "A streamed title".into();
        let _ = tx.send(CrawlEvent::Page(Box::new(page.clone())));
        poll_crawl(&mut rt.state);
        assert_eq!(
            visible_ids(&rt.state)[..before.len()],
            before[..],
            "the first burst does not reorder existing rows"
        );
        let after_first = visible_ids(&rt.state);
        let mut second = page.clone();
        second.url = "https://example.com/zz-streamed-1".into();
        let _ = tx.send(CrawlEvent::Page(Box::new(second)));
        poll_crawl(&mut rt.state);
        assert_eq!(
            visible_ids(&rt.state)[..after_first.len()],
            after_first[..],
            "a later burst keeps earlier order"
        );

        // A real filter click deliberately re-sorts, then keeps that order while streaming.
        click_key(&mut rt, "filter-findings");
        let filtered = visible_ids(&rt.state);
        let mut page = rt.state.pages[0].clone();
        page.url = "https://example.com/zz-streamed-2".into();
        page.title = "Another streamed title".into();
        let _ = tx.send(CrawlEvent::Page(Box::new(page)));
        poll_crawl(&mut rt.state);
        assert_eq!(
            visible_ids(&rt.state)[..filtered.len()],
            filtered[..],
            "a new event does not undo the clicked filter order"
        );

        // Completion applies the chosen order across every row and preserves selection.
        let selected = rt.state.view().selected.clone();
        let _ = tx.send(CrawlEvent::Done(
            "completed".into(),
            Box::new(calmtoad::sitemap::sample_report()),
            CrawlOutcome::Completed,
        ));
        poll_crawl(&mut rt.state);
        assert_eq!(rt.state.phase, RunPhase::Completed);
        assert_eq!(rt.state.view().selected, selected);
        let lengths: Vec<usize> = rt
            .state
            .visible
            .iter()
            .map(|index| rt.state.pages[*index].title.chars().count())
            .collect();
        assert!(lengths.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn confirmed_findings_survive_an_incomplete_capture() {
        let mut state = init();
        load_browser_sample(&mut state);
        let private = state
            .pages
            .iter_mut()
            .find(|page| page.url == "https://example.com/private")
            .expect("noindex sample");
        recapture(private, RenderState::TimedOut);
        assert!(page_incomplete(private));
        assert_eq!(private.indexability.label, Label::Noindex);
        assert!(page_has_finding(Tab::Internal, private, &state.audit));

        let mut rt = test_runtime(state);
        rt.frame(0.016);
        click_key(&mut rt, "filter-findings");
        assert!(rt.find("https://example.com/private").is_some());

        let promo = rt
            .state
            .pages
            .iter_mut()
            .find(|page| page.url == "https://example.com/promo")
            .expect("404 sample");
        recapture(promo, RenderState::TimedOut);
        assert!(page_incomplete(promo));
        assert!(page_has_finding(Tab::Response, promo, &rt.state.audit));

        let home = rt
            .state
            .pages
            .iter_mut()
            .find(|page| page.url == "https://example.com/")
            .expect("home sample");
        recapture(home, RenderState::TimedOut);
        assert!(page_incomplete(home));
        assert!(page_has_finding(Tab::Links, home, &rt.state.audit));

        let report = crawl::indexability_sample_pages()
            .into_iter()
            .find(|page| page.url == "https://example.com/report-2026.pdf")
            .expect("non-HTML sample");
        assert!(!page_incomplete(&report));
        assert!(
            !page_has_finding(Tab::Internal, &report, &Audit::default()),
            "a successful non-HTML page is context, not a finding"
        );

        let report = calmtoad::sitemap::sample_report();
        let blocked = report
            .urls
            .iter()
            .find(|entry| entry.state == UrlState::Blocked)
            .expect("blocked sitemap URL");
        assert!(sitemap_url_has_finding(blocked));
        let skipped = report
            .urls
            .iter()
            .find(|entry| entry.state == UrlState::Skipped)
            .expect("skipped sitemap URL");
        assert!(sitemap_url_has_finding(skipped));
        let mut pending = blocked.clone();
        pending.state = UrlState::Pending;
        pending.issue.clear();
        assert!(!sitemap_url_has_finding(&pending));
    }

    #[test]
    fn internal_csv_does_not_invent_word_counts_for_unknown_captures() {
        let mut state = init();
        load_browser_sample(&mut state);
        let nofollow = state
            .pages
            .iter()
            .find(|page| page.url == "https://example.com/nofollow")
            .expect("incomplete browser page");
        assert!(
            nofollow.words > 0,
            "the fixture retains a partial observation"
        );
        let row = export_row(Tab::Internal, nofollow, &state.audit);
        assert_eq!(
            row[4], "—",
            "a partial word count is not exported as measured"
        );

        let report = crawl::indexability_sample_pages()
            .into_iter()
            .find(|page| page.url == "https://example.com/report-2026.pdf")
            .expect("non-HTML page");
        assert_eq!(
            export_row(Tab::Internal, &report, &Audit::default())[4],
            "—"
        );

        let mut known_zero = state
            .pages
            .iter()
            .find(|page| page.url == "https://example.com/")
            .expect("auditable page")
            .clone();
        known_zero.words = 0;
        assert_eq!(
            export_row(Tab::Internal, &known_zero, &state.audit)[4],
            "0",
            "an observed zero is distinguishable from unknown"
        );
    }

    #[test]
    fn run_panel_exposes_reconciled_coverage_without_hover() {
        let mut state = init();
        load_sample(&mut state);
        state.view_mut().filter = CheckFilter::Findings;
        let coverage = sitemap_coverage_line(&state);
        for needle in ["listed", "checked", "blocked", "skipped", "file errors"] {
            assert!(
                coverage.contains(needle),
                "coverage omits {needle}: {coverage}"
            );
        }
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        click_key(&mut rt, "run-details");
        assert!(rt.state.run_details_open);
        assert!(rt.find("run-details-panel").is_some());

        rt.state.sitemap.discovery_done = false;
        rt.state.sitemap_discovering = true;
        rt.state.phase = RunPhase::Crawling;
        assert!(sitemap_coverage_line(&rt.state).contains("pending"));
        rt.state.sitemap_discovering = false;
        rt.state.phase = RunPhase::Stopped;
        assert!(sitemap_coverage_line(&rt.state).contains("stopped"));
        let before = sitemap_coverage_line(&rt.state);
        rt.state.use_sitemaps = false;
        assert_eq!(
            sitemap_coverage_line(&rt.state),
            before,
            "the next-run checkbox does not rewrite completed coverage"
        );
        if let Some(run) = rt.state.run.as_mut() {
            run.sitemaps = false;
        }
        assert!(sitemap_coverage_line(&rt.state).contains("off"));
    }

    #[test]
    fn deferred_counts_are_woken_and_reported_as_a_visible_change() {
        let mut state = init();
        load_sample(&mut state);
        state.phase = RunPhase::Crawling;
        state.counts_at = Some(Instant::now());
        state.counts_dirty = true; // force the next refresh into the throttle window
        let (tx, rx) = mpsc::channel();
        state.job = Some(CrawlJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
        });
        let mut page = state.pages[0].clone();
        page.url = "https://example.com/counts-deferred".into();
        let _ = tx.send(CrawlEvent::Page(Box::new(page)));
        let before = state.counts.pages;
        let changed = poll_crawl(&mut state);
        assert!(
            changed,
            "a deferred count refresh still requests a redraw/wake"
        );
        assert!(state.counts_deferred);
        assert_eq!(
            state.counts.pages, before,
            "the cached count is not rebuilt yet"
        );
        let wait = deferred_counts_wait(&state).expect("a deferred wake time");
        assert!(wait <= Duration::from_millis(100));

        std::thread::sleep(Duration::from_millis(105));
        let changed = poll_crawl(&mut state);
        assert!(changed, "the woken poll reports the count change");
        assert!(!state.counts_deferred);
        assert_eq!(state.counts.pages, before + 1);
    }

    #[test]
    fn inspector_relations_reuse_indices_and_refresh_after_streaming_and_replacement() {
        let template = crawl::sample_pages()[0].clone();
        let mut hub = template.clone();
        hub.url = "https://example.com/hub".into();
        hub.links = vec![
            "https://example.com/healthy".into(),
            "https://example.com/late".into(),
            "https://example.com/healthy".into(),
        ];
        let mut healthy = template.clone();
        healthy.url = hub.links[0].clone();
        healthy.links.clear();
        let mut state = init();
        set_pages(
            &mut state,
            vec![hub.clone(), healthy.clone()],
            "fixture".into(),
        );
        let original = relation_copy(&state, &hub.url, RelationCopy::Outgoing);
        assert_eq!(
            original.lines().collect::<Vec<_>>(),
            vec![healthy.url.as_str(), "https://example.com/late"]
        );
        let builds = state.inspector_relations.borrow().builds;
        for _ in 0..5 {
            state.relation_pages.outbound += 1;
            assert_eq!(
                relation_copy(&state, &hub.url, RelationCopy::Outgoing),
                original
            );
        }
        assert_eq!(
            state.inspector_relations.borrow().builds,
            builds,
            "paging/repaint reuse relation indices"
        );
        let mut late = template.clone();
        late.url = hub.links[1].clone();
        late.status = 404;
        late.links.clear();
        let mut source = template;
        source.url = "https://example.com/source".into();
        source.links = vec![hub.url.clone(), hub.url.clone()];
        let (tx, rx) = mpsc::channel();
        tx.send(CrawlEvent::Page(Box::new(late))).unwrap();
        tx.send(CrawlEvent::Page(Box::new(source.clone()))).unwrap();
        state.phase = RunPhase::Crawling;
        state.job = Some(CrawlJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
        });
        assert!(poll_crawl(&mut state));
        assert_eq!(
            relation_copy(&state, &hub.url, RelationCopy::Outgoing)
                .lines()
                .collect::<Vec<_>>(),
            vec!["https://example.com/late", healthy.url.as_str()]
        );
        assert_eq!(
            relation_copy(&state, &hub.url, RelationCopy::Incoming),
            source.url,
            "repeated links list one incoming source"
        );
        let mut replacement = state.pages.clone();
        replacement[3].url = "https://example.com/replaced-source".into();
        set_pages(&mut state, replacement, "replacement".into());
        assert_eq!(
            relation_copy(&state, &hub.url, RelationCopy::Incoming),
            "https://example.com/replaced-source"
        );
        assert!(
            page_by_url(&state, &source.url).is_none(),
            "same-length replacement invalidates URL indices"
        );
    }

    #[test]
    fn related_inspection_and_row_navigation_reset_relation_windows() {
        let mut long = crawl::sample_pages()[0].clone();
        long.url = "https://example.com/a-long".into();
        long.h1s = (0..1000).map(|index| format!("Heading {index}")).collect();
        let mut short = long.clone();
        short.url = "https://example.com/b-short".into();
        short.h1s.truncate(50);
        let mut state = init();
        set_pages(
            &mut state,
            vec![long.clone(), short.clone()],
            "fixture".into(),
        );
        switch_tab(&mut state, Tab::H1);
        assert!(inspect_url_state(&mut state, &long.url));
        state.relation_pages.h1 = 39;
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        assert!(rt.find("h1-item-999").is_some());
        click_key(&mut rt, "detail-next");
        assert_eq!(rt.state.table.cursor(), Some(short.url.as_str()));
        assert_eq!(rt.state.relation_pages.h1, 0);
        assert!(rt.find("h1-item-0").is_some());
        rt.state.relation_pages.h1 = 1;
        assert!(inspect_url_state(&mut rt.state, &long.url));
        assert_eq!(
            rt.state.relation_pages.h1, 0,
            "Inspect uses the same identity reset"
        );
        rt.state.relation_pages.h1 = 39;
        let mut replacement = long.clone();
        replacement.h1s.truncate(50);
        set_pages(
            &mut rt.state,
            vec![replacement],
            "new dataset, same URL".into(),
        );
        rt.frame(0.032);
        assert_eq!(
            rt.state.relation_pages.h1, 0,
            "a new dataset resets paging even for the same selected URL"
        );
        assert!(rt.find("h1-item-0").is_some());
        assert_eq!(relation_window(usize::MAX, 50), (25, 50));
        assert_eq!(relation_window(39, 0), (0, 0));
    }

    #[test]
    fn outgoing_relations_page_beyond_bounded_rows_and_keep_broken_first() {
        let template = crawl::sample_pages()
            .into_iter()
            .find(|page| page.url == "https://example.com/")
            .expect("template");
        let hub_url = "https://example.com/outbound-hub".to_string();
        let broken_url = "https://example.com/broken-outbound".to_string();
        let mut hub = template.clone();
        hub.url = hub_url.clone();
        hub.title = "Outbound hub title that is long enough".into();
        hub.links = (0..50)
            .map(|index| format!("https://example.com/target-{index}"))
            .collect();
        hub.links.push(broken_url.clone());
        let mut pages = vec![hub];
        for index in 0..50 {
            let mut target = template.clone();
            target.url = format!("https://example.com/target-{index}");
            target.links.clear();
            pages.push(target);
        }
        let mut broken = template.clone();
        broken.url = broken_url.clone();
        broken.status = 404;
        broken.links.clear();
        pages.push(broken);

        let mut state = init();
        let message = format!("{} pages", pages.len());
        set_pages(&mut state, pages, message);
        switch_tab(&mut state, Tab::Links);
        select_scene_url(&mut state, &hub_url);
        state.details_open = true;
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        assert!(rt.find("incoming-label").is_none());
        assert!(rt.find("outbound-inspect-0").is_some());
        assert!(rt.find("outbound-inspect-30").is_none());
        assert!(inspect_url_state(&mut rt.state, &broken_url));
        assert_eq!(rt.state.table.cursor(), Some(broken_url.as_str()));

        select_scene_url(&mut rt.state, &hub_url);
        rt.frame(0.016);
        click_key(&mut rt, "outbound-more");
        rt.frame(0.016);
        assert!(rt.find("outbound-inspect-30").is_some());
        assert!(
            rt.find("outbound-inspect-0").is_none(),
            "the broken-first previous window is unmounted"
        );
        assert!(rt.find("outbound-prev").is_some());
        assert!(inspect_url_state(
            &mut rt.state,
            "https://example.com/target-29"
        ));
        assert_eq!(
            rt.state.table.cursor(),
            Some("https://example.com/target-29")
        );
    }
    #[test]
    fn relation_lists_page_beyond_their_first_bounded_rows() {
        let template = crawl::sample_pages()
            .into_iter()
            .find(|page| page.status == 200 && page.url == "https://example.com/")
            .expect("template");
        let hub_url = "https://example.com/hub".to_string();
        let broken_url = "https://example.com/broken-target".to_string();
        let mut hub = template.clone();
        hub.url = hub_url.clone();
        hub.title = "Shared relation title".into();
        hub.h1s = (0..1000)
            .map(|index| format!("Hub heading {index}"))
            .collect();
        hub.links = (0..50)
            .map(|index| format!("https://example.com/source-{index}"))
            .collect();
        hub.links.push(broken_url.clone());
        let mut pages = vec![hub];
        for index in 0..50 {
            let mut source = template.clone();
            source.url = format!("https://example.com/source-{index}");
            source.title = if index < 30 {
                "Shared relation title".into()
            } else {
                format!("Source title {index}")
            };
            source.h1s = vec![format!("Source heading {index}")];
            source.links = vec![hub_url.clone()];
            pages.push(source);
        }
        let mut broken = template.clone();
        broken.url = broken_url.clone();
        broken.status = 404;
        pages.push(broken);

        let mut state = init();
        let message = format!("{} pages", pages.len());
        set_pages(&mut state, pages, message);
        switch_tab(&mut state, Tab::H1);
        select_scene_url(&mut state, &hub_url);
        state.details_open = true;
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        assert!(rt.find("h1-item-24").is_some());
        assert!(rt.find("h1-item-25").is_none());
        let mut clicks = 0;
        while rt.find("h1-more").is_some() && clicks < 50 {
            click_key(&mut rt, "h1-more");
            clicks += 1;
        }
        rt.frame(0.016);

        assert!(
            rt.find("h1-item-999").is_some(),
            "the 1,000th H1 is reachable"
        );
        assert!(
            rt.find("h1-item-0").is_none(),
            "earlier H1 rows are no longer mounted"
        );
        assert!(rt.find("h1-prev").is_some());
        assert!(rt.find("h1-copy-all").is_some());

        click_key(&mut rt, "tab-links");
        select_scene_url(&mut rt.state, &hub_url);
        rt.frame(0.016);
        assert!(rt.find("inbound-inspect-24").is_some());
        click_key(&mut rt, "inbound-more");
        rt.frame(0.016);
        assert!(rt.find("inbound-inspect-30").is_some());
        assert!(
            rt.find("inbound-inspect-0").is_none(),
            "the previous inbound window is unmounted"
        );
        assert!(rt.find("inbound-prev").is_some());
        assert!(inspect_url_state(
            &mut rt.state,
            "https://example.com/source-30"
        ));
        assert_eq!(
            rt.state.table.cursor(),
            Some("https://example.com/source-30")
        );

        click_key(&mut rt, "tab-titles");
        select_scene_url(&mut rt.state, &hub_url);
        rt.frame(0.016);
        assert!(rt.find("peers-more").is_some());
        assert!(rt.find("peer-25").is_none());
        click_key(&mut rt, "peers-more");
        rt.frame(0.016);
        assert!(rt.find("peer-25").is_some());
        assert!(
            rt.find("peer-0").is_none(),
            "the previous peer window is unmounted"
        );
        assert!(rt.find("peers-prev").is_some());
        assert!(inspect_url_state(
            &mut rt.state,
            "https://example.com/source-25"
        ));
        assert_eq!(
            rt.state.table.cursor(),
            Some("https://example.com/source-25")
        );
    }

    #[test]
    fn skipped_sitemap_files_are_findings_in_the_files_view() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Sitemaps);
        state.sitemap_view = SitemapView::Files;
        let (column, dir) = state.view().sort.clone();
        state.table.set_sort(column, dir);
        sort_sitemap(&mut state);
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        click_key(&mut rt, "filter-findings");
        assert!(rt.state.view().filter == CheckFilter::Findings);
        assert!(rt.state.view_counts.findings >= 1);
        let skipped = rt
            .state
            .sitemap
            .files
            .iter()
            .find(|file| file.state == FileState::Skipped)
            .expect("skipped sitemap file");
        assert!(sitemap_file_has_finding(skipped));
        assert!(
            rt.find(&skipped.url).is_some(),
            "the skipped file is visible"
        );
        assert!(
            rt.state
                .sitemap_file_visible
                .iter()
                .all(|index| sitemap_file_has_finding(&rt.state.sitemap.files[*index]))
        );
        let rows: Vec<String> = rt
            .state
            .sitemap_file_visible
            .iter()
            .map(|index| sitemap_file_row(&rt.state.sitemap.files[*index])[0].clone())
            .collect();
        assert_eq!(rows, visible_ids(&rt.state));
        let clipboard = rune::clipboard::memory();
        rt.use_clipboard(&clipboard);
        click_key(&mut rt, "copy-urls");
        let copied = clipboard
            .text()
            .map(|text| text.to_string())
            .unwrap_or_default();
        assert_eq!(copied.lines().collect::<Vec<_>>(), rows);
    }

    #[test]
    fn completed_run_reporting_ignores_next_run_controls_and_validation_notices() {
        let mut state = init();
        load_sample(&mut state);
        let coverage = sitemap_coverage_line(&state);
        let summary = sitemap_summary(&state);
        let reason = run_report_reason(&state);
        let pages = state.pages.len();
        assert!(coverage.contains("listed"));
        assert!(!summary.contains("off"));

        switch_tab(&mut state, Tab::Sitemaps);
        state.sitemap_view = SitemapView::Files;
        let (column, dir) = state.view().sort.clone();
        state.table.set_sort(column, dir);
        sort_sitemap(&mut state);
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        // A next-run control change never rewrites the completed run's coverage.
        click_key(&mut rt, "use-sitemaps");
        assert!(!rt.state.use_sitemaps);
        assert_eq!(sitemap_coverage_line(&rt.state), coverage);
        assert!(!sitemap_summary(&rt.state).contains("off"));
        assert!(!sitemap_empty(&rt.state).0.contains("off"));

        // The Files noun belongs only to the active Sitemaps Files view.
        assert_eq!(run_view_noun(&rt.state), "files");
        click_key(&mut rt, "tab-titles");
        assert_eq!(rt.state.tab, Tab::Titles);
        assert_eq!(run_view_noun(&rt.state), "URLs");
        assert_eq!(run_report_reason(&rt.state), reason);

        // Rejecting the next-run setup keeps the old terminal reason and results intact.
        rt.state.url.replace_text("not a url");
        rt.state.pages_limit.replace_text("0");
        rt.state.depth_limit.replace_text("11");
        click_key(&mut rt, "crawl");
        assert!(rt.state.field_errors.any());
        assert_eq!(rt.state.pages.len(), pages);
        assert_eq!(rt.state.run_outcome.as_deref(), Some(reason.as_str()));
        assert_eq!(run_report_reason(&rt.state), reason);
        assert_eq!(sitemap_coverage_line(&rt.state), coverage);
        assert_eq!(rt.state.phase, RunPhase::Completed);

        rt.state.phase = RunPhase::Stopped;
        rt.state.run_outcome = Some("Stopped · 3 pages on example.com".into());
        rt.state.url.replace_text("still not a url");
        click_key(&mut rt, "crawl");
        assert_eq!(
            run_report_reason(&rt.state),
            "Stopped · 3 pages on example.com"
        );
    }
    #[test]
    fn sample_fixture_separates_known_findings_from_incomplete_checks() {
        let mut state = init();
        load_sample(&mut state);
        assert_eq!(state.view_counts.total, 9);
        assert_eq!(
            state.view_counts.incomplete, 0,
            "no sample page is incomplete"
        );
        assert!(
            state.view_counts.findings >= 3,
            "known indexability findings are counted: {:?}",
            state.view_counts
        );
        switch_tab(&mut state, Tab::Titles);
        assert_eq!(state.view_counts.incomplete, 0);
        assert_eq!(state.view_counts.findings, 3);
    }
    #[test]
    fn contextual_inspector_evidence_is_available_and_honest() {
        let mut state = init();
        load_sample(&mut state);
        let pages = state.pages.clone();
        let pricing = pages
            .iter()
            .find(|page| page.url == "https://example.com/pricing")
            .unwrap();
        assert!(
            !state
                .audit
                .title_duplicate_peers(&pages, pricing)
                .is_empty()
        );
        assert!(primary_issue(Tab::Titles, pricing, &state.audit).contains("Duplicate"));

        let launch = pages
            .iter()
            .find(|page| page.url == "https://example.com/guides/launch")
            .unwrap();
        assert_eq!(launch.h1s.len(), 2);
        assert_eq!(primary_issue(Tab::H1, launch, &state.audit), "Multiple");

        let redirect = pages
            .iter()
            .find(|page| page.url == "https://example.com/old-pricing")
            .unwrap();
        assert_eq!(
            state.audit.redirect_chain(redirect),
            vec!["https://example.com/pricing".to_string()]
        );

        let inbound = state.audit.inbound_sources(&pages, pricing);
        assert!(
            inbound
                .iter()
                .any(|page| page.url == "https://example.com/")
        );
        assert_eq!(pricing.links.len(), 0);

        let non_html = pages
            .iter()
            .find(|page| page.url == "https://example.com/api/health")
            .unwrap();
        assert!(primary_issue(Tab::Response, non_html, &state.audit).contains("500"));

        let canonical = pages
            .iter()
            .find(|page| page.url == "https://example.com/blog/notes")
            .unwrap();
        let target = canonical
            .indexability
            .first_valid_canonical()
            .expect("canonical target");
        assert!(pages.iter().any(|page| page.url == target));

        let mut browser = init();
        load_browser_sample(&mut browser);
        let incomplete = browser
            .pages
            .iter()
            .find(|page| page.url == "https://example.com/nofollow")
            .expect("incomplete browser fixture");
        assert!(page_incomplete(incomplete));
        assert!(
            primary_issue(Tab::Titles, incomplete, &browser.audit).contains("Capture incomplete")
        );
        assert!(matches!(
            row_issue(Tab::H1, incomplete, &browser.audit),
            RowIssue::Unclaimed
        ));

        let non_html = crawl::indexability_sample_pages()
            .into_iter()
            .find(|page| page.url == "https://example.com/report-2026.pdf")
            .expect("non-HTML sample");
        assert!(
            primary_issue(
                Tab::Titles,
                &non_html,
                &Audit::from_pages(std::slice::from_ref(&non_html))
            )
            .contains("Non-HTML")
        );
    }

    #[test]
    fn related_navigation_and_previous_next_follow_visible_rows() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        select_scene_url(&mut state, "https://example.com/pricing");
        state.details_open = true;
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        assert!(rt.find("detail-prev").is_some());
        assert!(rt.find("detail-next").is_some());
        assert!(rt.find("detail-close").is_some());
        assert!(rt.find("peer-0").is_some(), "duplicate peers are listed");
        let before = rt.state.table.cursor().map(str::to_string);
        let (x, y) = rt.find("peer-0").expect("peer inspect");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_ne!(rt.state.table.cursor().map(str::to_string), before);
        assert!(rt.state.details_open);
        assert_eq!(rt.state.view().selected.as_deref(), rt.state.table.cursor());

        // A related URL hidden by filters is revealed by clearing only those filters.
        rt.state.view_mut().filter = CheckFilter::Findings;
        rt.state.view_mut().query.replace_text("pricing");
        refresh_visible_forced(&mut rt.state);
        assert!(inspect_url_state(&mut rt.state, "https://example.com/"));
        assert_eq!(rt.state.view().filter, CheckFilter::All);
        assert!(rt.state.view().query.text.is_empty());
        assert_eq!(rt.state.table.cursor(), Some("https://example.com/"));
        assert_eq!(
            rt.state.view().selected.as_deref(),
            Some("https://example.com/"),
            "the inspector follows the related URL, not the first row"
        );
    }

    #[test]
    fn keyboard_help_does_not_hijack_typing_or_stop_work() {
        let mut state = init();
        load_sample(&mut state);
        switch_tab(&mut state, Tab::Titles);
        state.details_open = true;
        state.evidence_open = true;
        let mut rt = test_runtime(state);
        rt.frame(0.016);
        // Headless Runtime has no native menu, so exercise the same action the Escape key
        // equivalent invokes. The menu item is what reaches users while a table has focus.
        assert!(close_context_state(&mut rt.state));
        assert!(!rt.state.details_open, "closing details first");
        assert!(rt.state.evidence_open, "one close handles one layer");
        assert!(!rt.state.phase.active(), "closing never stops a crawl");
        assert!(close_context_state(&mut rt.state));
        assert!(!rt.state.evidence_open);

        rt.state.details_open = true;
        assert!(focus_search_state(&mut rt.state));
        rt.frame(0.016);
        for ch in "calm".chars() {
            rt.key(KeyEvent {
                key: Key::Text(ch.to_string()),
                mods: Mods::default(),
            });
            rt.frame(0.016);
        }
        assert_eq!(rt.state.view().query.text, "calm");
        assert!(!rt.state.visible.is_empty());
        assert!(!rt.state.phase.active());
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(0.016);
        assert!(
            rt.state.view().query.text.is_empty(),
            "Escape clears the query"
        );
        assert!(
            rt.state.details_open,
            "the focused field kept the inspector open"
        );
        assert!(!rt.state.phase.active());

        assert!(focus_url_state(&mut rt.state));
        rt.frame(0.016);
        assert!(rt.state.url.focused && !rt.state.view().query.focused);
        let (x, y) = rt.find("url").expect("site URL field");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        let before = rt.state.url.text.clone();
        rt.key(KeyEvent {
            key: Key::Text("x".into()),
            mods: Mods::default(),
        });
        rt.frame(0.016);
        assert_ne!(rt.state.url.text, before, "typing still edits the URL");

        // Table arrow keys move the visible selection and the inspector follows it.
        switch_tab(&mut rt.state, Tab::Titles);
        select_scene_url(&mut rt.state, "https://example.com/pricing");
        rt.frame(0.016);
        let (x, y) = rt
            .find("https://example.com/pricing")
            .expect("selected row");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        let before = rt.state.table.cursor().map(str::to_string);
        rt.key(KeyEvent {
            key: Key::Down,
            mods: Mods::default(),
        });
        rt.frame(0.016);
        assert_ne!(
            rt.state.table.cursor().map(str::to_string),
            before,
            "table arrow keys move the selection"
        );
        assert_eq!(rt.state.view().selected.as_deref(), rt.state.table.cursor());
    }

    #[test]
    fn polling_is_budgeted_without_losing_events_or_the_final_report() {
        let mut state = init();
        state.phase = RunPhase::Crawling;
        state.crawl_started = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        let template = crawl::sample_pages()[0].clone();
        for index in 0..600 {
            let mut page = template.clone();
            page.url = format!("https://example.com/page-{index}");
            let _ = tx.send(CrawlEvent::Page(Box::new(page)));
        }
        let _ = tx.send(CrawlEvent::Done(
            "stopped · 600 pages".into(),
            Box::new(calmtoad::sitemap::sample_report()),
            CrawlOutcome::Stopped,
        ));
        state.job = Some(CrawlJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
        });
        let changed = poll_crawl(&mut state);
        assert!(changed);
        assert!(
            state.crawl_more,
            "the poll yields before draining everything"
        );
        assert!(state.pages.len() < 600);
        let mut polls = 0;
        while state.job.is_some() && polls < 10 {
            poll_crawl(&mut state);
            polls += 1;
        }
        assert_eq!(state.pages.len(), 600, "every event is preserved");
        assert_eq!(state.phase, RunPhase::Stopped);
        assert!(
            state
                .pages
                .iter()
                .enumerate()
                .all(|(index, page)| page.url == format!("https://example.com/page-{index}")),
            "event order is preserved"
        );
    }

    fn synthetic_pages(count: usize) -> Vec<Page> {
        let template = crawl::sample_pages();
        let base = template
            .iter()
            .find(|page| page.status == 200 && !page.title.is_empty())
            .expect("sample template")
            .clone();
        (0..count)
            .map(|index| {
                let mut page = base.clone();
                page.url = format!("https://example.com/page-{index}");
                page.depth = (index % 4) as u8;
                page.title = match index % 7 {
                    0 => String::new(),
                    1 => "Shared duplicate title for measurement".into(),
                    _ => format!("A representative page title number {index} for the workflow test"),
                };
                page.meta_description = match index % 5 {
                    0 => String::new(),
                    _ => format!("A description for page {index} that is long enough for the meta length heuristic in this tool."),
                };
                page.h1s = match index % 6 {
                    0 => Vec::new(),
                    1 => vec!["One".into(), "Two".into()],
                    _ => vec![format!("Heading {index}")],
                };
                page.status = match index % 41 {
                    0 => 404,
                    1 => 301,
                    _ => 200,
                };
                page.redirect_to = if page.status == 301 {
                    format!("https://example.com/page-{}", index + 1)
                } else {
                    String::new()
                };
                page.canonical = if page.status == 200 {
                    format!("https://example.com/page-{index}")
                } else {
                    String::new()
                };
                page.words = index % 900;
                page.images = index % 8;
                page.images_missing_alt = index % 5;
                page.links = (1..=3)
                    .map(|step| format!("https://example.com/page-{}", index + step * 7))
                    .collect();
                page
            })
            .collect()
    }

    #[test]
    #[ignore = "synthetic measurement; run in release with --ignored --nocapture"]
    fn measure_large_result_workflows() {
        for count in [10_000_usize, 50_000] {
            let pages = synthetic_pages(count);
            let audit = Audit::from_pages(&pages);
            let index: Vec<String> = pages.iter().map(page_search_text).collect();

            let mut state = init();
            state.pages = pages.clone();
            state.audit = audit.clone();
            state.search_index = index;
            state.phase = RunPhase::Completed;
            state.counts_dirty = true;
            refresh_counts(&mut state, true);
            switch_tab(&mut state, Tab::Titles);
            state.view_mut().query.replace_text("representative");
            state.view_mut().filter = CheckFilter::Findings;
            state.view_mut().sort = ("url".into(), SortDir::Ascending);

            let started = Instant::now();
            refresh_visible_forced(&mut state);
            let cached_query = started.elapsed();
            let matched = state.visible.len();

            let started = Instant::now();
            let cached_matches = state
                .search_index
                .iter()
                .filter(|text| text.contains("representative"))
                .count();
            let cached_match = started.elapsed();
            let started = Instant::now();
            let rebuilt = pages
                .iter()
                .filter(|page| page_search_text(page).contains("representative"))
                .count();
            let rebuilt_query = started.elapsed();

            let started = Instant::now();
            let _ = counts_with(&pages, &state.audit);
            let count_refresh = started.elapsed();

            let mut physical = pages.clone();
            let started = Instant::now();
            physical.sort_by(|a, b| compare(a, b, Tab::Titles, "url", &audit));
            let physical_sort = started.elapsed();
            let started = Instant::now();
            let mut indices: Vec<usize> = (0..pages.len()).collect();
            indices.sort_by(|a, b| compare(&pages[*a], &pages[*b], Tab::Titles, "url", &audit));
            let index_sort = started.elapsed();
            assert_eq!(physical.len(), indices.len());

            // A burst of 10,000 events: old unbounded drain versus one budgeted poll.
            let (tx, rx) = mpsc::channel();
            for _ in 0..10_000 {
                let _ = tx.send(CrawlEvent::Page(Box::new(pages[0].clone())));
            }
            let mut burst = init();
            burst.phase = RunPhase::Crawling;
            burst.job = Some(CrawlJob {
                rx,
                cancel: Arc::new(AtomicBool::new(false)),
                stopping: false,
            });
            let started = Instant::now();
            poll_crawl(&mut burst);
            let budgeted_poll = started.elapsed();
            let budgeted_pages = burst.pages.len();

            // The previous poll loop drained every available event in one frame.
            let mut unbounded = init();
            let started = Instant::now();
            for _ in 0..10_000 {
                unbounded.sitemap.observe_page(&pages[0]);
                unbounded.search_index.push(page_search_text(&pages[0]));
                unbounded.pages.push(pages[0].clone());
                unbounded.audit.push(&pages[0]);
            }
            refresh_visible(&mut unbounded);
            let unbounded_burst = started.elapsed();

            let started = Instant::now();
            sync_selection(&mut state);
            let selection_sync = started.elapsed();

            let mut sstate = init();
            sstate.tab = Tab::Sitemaps;
            sstate.sitemap_view = SitemapView::Urls;
            sstate.phase = RunPhase::Completed;
            sstate.sitemap.discovery_done = true;
            sstate.sitemap.urls = pages
                .iter()
                .map(|page| SitemapUrl {
                    url: page.url.clone(),
                    sources: vec!["https://example.com/sitemap.xml".into()],
                    state: UrlState::Checked,
                    status: page.status,
                    issue: if page.status >= 400 {
                        format!("HTTP {}", page.status)
                    } else {
                        String::new()
                    },
                    content_type: page.content_type.clone(),
                    assessment: Box::new(page.indexability.clone()),
                    ..SitemapUrl::default()
                })
                .collect();
            sstate.view_mut().query.replace_text("representative");
            sstate.view_mut().filter = CheckFilter::Findings;
            let started = Instant::now();
            refresh_sitemap_visible_inner(&mut sstate, true);
            let sitemap_query = started.elapsed();

            let mut rt = test_runtime(state);
            let started = Instant::now();
            rt.frame(0.016);
            let first_frame = started.elapsed();
            // Actual inspector construction for a duplicate-heavy selection and for a page with
            // many incoming links (both read-model paths normalize/scan retained data).
            let duplicate_url = pages[8].url.clone();
            rt.update(|state: &mut State, _tx: &mut Tx| {
                switch_tab(state, Tab::Titles);
                select_scene_url(state, &duplicate_url);
                state.details_open = true;
            });
            let started = Instant::now();
            rt.frame(0.016);
            let duplicate_inspector_frame = started.elapsed();
            rt.update(|state: &mut State, _| state.relation_pages.peers = 1);
            let started = Instant::now();
            rt.frame(0.032);
            let duplicate_paging_frame = started.elapsed();
            rt.update(|state: &mut State, _| state.action_notice = None);
            let started = Instant::now();
            rt.frame(0.048);
            let duplicate_repeat_frame = started.elapsed();
            let duplicate_peers = {
                let page = rt
                    .state
                    .pages
                    .iter()
                    .find(|page| page.url == duplicate_url)
                    .unwrap();
                rt.state
                    .audit
                    .title_duplicate_peers(&rt.state.pages, page)
                    .len()
            };

            let hub_url = pages[0].url.clone();
            rt.update(|state: &mut State, _tx: &mut Tx| {
                for page in &mut state.pages {
                    if page.url != hub_url && !page.links.contains(&hub_url) {
                        page.links.push(hub_url.clone());
                    }
                }
                state.audit = Audit::from_pages(&state.pages);
                invalidate_inspector(state);
                switch_tab(state, Tab::Links);
                select_scene_url(state, &hub_url);
                state.details_open = true;
            });
            let started = Instant::now();
            rt.frame(0.016);
            let inbound_inspector_frame = started.elapsed();
            rt.update(|state: &mut State, _| state.relation_pages.inbound = 1);
            let started = Instant::now();
            rt.frame(0.032);
            let inbound_paging_frame = started.elapsed();
            rt.update(|state: &mut State, _| state.action_notice = None);
            let started = Instant::now();
            rt.frame(0.048);
            let inbound_repeat_frame = started.elapsed();
            let started = Instant::now();
            let inbound_sources = {
                let page = rt
                    .state
                    .pages
                    .iter()
                    .find(|page| page.url == hub_url)
                    .unwrap();
                rt.state.audit.inbound_sources(&rt.state.pages, page).len()
            };
            let inbound_audit_scan = started.elapsed();
            invalidate_inspector(&mut rt.state);
            let started = Instant::now();
            drop(page_positions(&rt.state));
            let positions_rebuild = started.elapsed();
            let started = Instant::now();
            let page = page_by_url(&rt.state, &hub_url).unwrap();
            drop(inspector_relations(&rt.state, page, Tab::Links));
            let inbound_relations_rebuild = started.elapsed();

            // A heavy outbound selection exercises stable broken-first lookup without an
            // O(pages × targets) sort key. It has no pre-change outbound baseline.
            rt.update(|state: &mut State, _| {
                let links = state.pages.iter().map(|page| page.url.clone()).collect();
                state.pages[0].links = links;
                state.audit = Audit::from_pages(&state.pages);
                invalidate_inspector(state);
                state.relation_pages.outbound = 0;
            });
            let started = Instant::now();
            rt.frame(0.064);
            let outbound_inspector_frame = started.elapsed();
            rt.update(|state: &mut State, _| state.relation_pages.outbound = 1);
            let started = Instant::now();
            rt.frame(0.080);
            let outbound_paging_frame = started.elapsed();

            println!(
                "results={count} matched={matched} cached_matches={cached_matches} rebuilt_matches={rebuilt} cached_query={cached_query:?} cached_match={cached_match:?} rebuilt_query={rebuilt_query:?} count_refresh={count_refresh:?} physical_sort={physical_sort:?} index_sort={index_sort:?} budgeted_poll={budgeted_poll:?} budgeted_pages={budgeted_pages} unbounded_burst={unbounded_burst:?} first_frame={first_frame:?} duplicate_inspector_frame={duplicate_inspector_frame:?} duplicate_peers={duplicate_peers} duplicate_paging_frame={duplicate_paging_frame:?} duplicate_repeat_frame={duplicate_repeat_frame:?} inbound_inspector_frame={inbound_inspector_frame:?} inbound_sources={inbound_sources} inbound_paging_frame={inbound_paging_frame:?} inbound_repeat_frame={inbound_repeat_frame:?} selection_sync={selection_sync:?} sitemap_query={sitemap_query:?} positions_rebuild={positions_rebuild:?} inbound_audit_scan={inbound_audit_scan:?} inbound_relations_rebuild={inbound_relations_rebuild:?} outbound_inspector_frame={outbound_inspector_frame:?} outbound_paging_frame={outbound_paging_frame:?}"
            );
        }
    }
    #[test]
    fn the_use_sitemaps_checkbox_drives_the_next_crawl_option() {
        let state = init();
        assert!(state.use_sitemaps);
        let mut rt = Runtime::new(
            state,
            view,
            (1180.0, 740.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.016);
        let (x, y) = rt.find("use-sitemaps").expect("use sitemaps checkbox");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(!rt.state.use_sitemaps);
    }
}
