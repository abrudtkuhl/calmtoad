//! A desktop SEO spider: crawl one host, then open the views people use first.
//!
//! Internal URLs, titles, meta descriptions, H1s, response codes, canonicals, image alt text,
//! internal links, and the URLs a site's sitemaps list. `cargo run` opens the window. `cargo run -- --snap shots/sample.png --scene sample --size 1180x740`
//! renders it headless.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use calmtoad::crawl;
use calmtoad::crawl::{
    Audit, Counts, DEFAULT_MAX_PAGES, Limits, MAX_DEPTH, Page, auditable, canonical_issue,
    counts_with, h1_issue, image_issue, index_issue, links_issue, meta_issue, redirect_issue,
    response_issue, title_issue,
};
use calmtoad::sitemap::{FileState, SitemapFile, SitemapKind, SitemapReport, SitemapUrl, UrlState};
use calmtoad::update;
use rune::tooling::AppSpec;
use rune::{
    Align, Color, Cx, El, FileDialogOptions, Icon, MenuItem, TextCase, TextEdit, TextStyle, Tx,
    Waker, col, el, icon, row, text,
};
use rune_kit::{
    Button, ButtonVariant, Cell, Checkbox, ControlSize, IconButton, IconButtonVariant, Kit,
    SortDir, Table, TableColumn, TableState, TableStyle,
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
    Done(String, Box<SitemapReport>),
}

struct CrawlJob {
    rx: mpsc::Receiver<CrawlEvent>,
    cancel: Arc<AtomicBool>,
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

struct State {
    url: TextEdit,
    pages_limit: TextEdit,
    depth_limit: TextEdit,
    use_sitemaps: bool,
    tab: Tab,
    issues_only: bool,
    details_open: bool,
    table: TableState,
    pages: Vec<Page>,
    /// Indices into `pages` after the current tab's "issues only" filter, in display order.
    visible: Vec<usize>,
    audit: Audit,
    sitemap: SitemapReport,
    sitemap_view: SitemapView,
    sitemap_visible: Vec<usize>,
    sitemap_file_visible: Vec<usize>,
    sitemap_discovering: bool,
    message: String,
    crawling: bool,
    crawl_started: Option<Instant>,
    crawl_elapsed: Option<Duration>,
    export_notice: Option<Result<String, String>>,
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
    table.set_sort("url", SortDir::Ascending);
    State {
        url: TextEdit::new("https://example.com/"),
        pages_limit: TextEdit::new(DEFAULT_MAX_PAGES.to_string()),
        depth_limit: TextEdit::new(MAX_DEPTH.to_string()),
        use_sitemaps: true,
        tab: Tab::Internal,
        issues_only: false,
        details_open: false,
        table,
        pages: Vec::new(),
        visible: Vec::new(),
        audit: Audit::default(),
        sitemap: SitemapReport::default(),
        sitemap_view: SitemapView::Urls,
        sitemap_visible: Vec::new(),
        sitemap_file_visible: Vec::new(),
        sitemap_discovering: false,
        message: "Ready · enter a site URL to begin. Up to 40 pages and 3 levels deep.".into(),
        crawling: false,
        crawl_started: None,
        crawl_elapsed: None,
        export_notice: None,
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

fn poll(state: &mut State, _t: f64, tx: &mut Tx) -> bool {
    if !state.update_confirmed {
        // The window has laid out a frame. Now the new copy is known to start, so the
        // previous version can be removed.
        state.update_confirmed = true;
        update::install::confirm_pending();
    }
    let update_changed = poll_updates(state, tx);
    let crawl_changed = poll_crawl(state);
    update_changed || crawl_changed
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

fn poll_crawl(state: &mut State) -> bool {
    let Some(job) = state.job.take() else {
        return false;
    };
    let start_len = state.pages.len();
    let mut changed = false;
    let mut sitemap_changed = false;
    let mut finished = None;
    loop {
        match job.rx.try_recv() {
            Ok(CrawlEvent::Page(page)) => {
                state.sitemap.observe_page(&page);
                state.pages.push(*page);
                changed = true;
                sitemap_changed = true;
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
            Ok(CrawlEvent::Done(message, report)) => {
                finished = Some((message, report));
                break;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = Some((
                    format!("{} pages on the site", state.pages.len()),
                    Box::new(std::mem::take(&mut state.sitemap)),
                ));
                break;
            }
        }
    }
    if changed {
        // Add only the pages that arrived since the last poll; rebuilding the whole audit on
        // every frame turned a long crawl into quadratic work.
        let mut broken_added = false;
        for page in &state.pages[start_len..] {
            state.audit.push(page);
            broken_added |= page.status == 0 || page.status >= 400;
        }
        if broken_added {
            state.audit.rebuild_broken_links(&state.pages);
        }
        state.message = format!("{} pages so far…", state.pages.len());
        refresh_visible(state);
    } else if sitemap_changed && state.tab == Tab::Sitemaps {
        refresh_sitemap_visible(state);
    }
    let did_finish = finished.is_some();
    match finished {
        Some((message, report)) => {
            state.crawling = false;
            state.crawl_elapsed = state.crawl_started.take().map(|started| started.elapsed());
            state.message = message;
            state.audit = Audit::from_pages(&state.pages);
            state.sitemap = *report;
            state.sitemap_discovering = false;
            sort_pages(state);
            if state.tab == Tab::Sitemaps {
                sort_sitemap(state);
            }
            if let Some(url) = state.pages.first().map(|page| page.url.clone()) {
                state.table.select(url);
            } else {
                state.table.clear_selection();
            }
        }
        None => state.job = Some(job),
    }
    changed || sitemap_changed || did_finish
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
    !state.crawling && matches!(state.update, UpdateUi::Ready(_))
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

fn start_crawl(state: &mut State) {
    if state.crawling {
        return;
    }
    let raw = state.url.text.clone();
    let seed = match crawl::normalize_seed(&raw) {
        Ok(seed) => seed,
        Err(message) => {
            state.message = message;
            return;
        }
    };
    let limits = Limits {
        max_pages: state
            .pages_limit
            .text
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|pages| *pages > 0)
            .unwrap_or(DEFAULT_MAX_PAGES),
        max_depth: state
            .depth_limit
            .text
            .trim()
            .parse::<u8>()
            .ok()
            .unwrap_or(MAX_DEPTH)
            .min(10),
    };
    state.max_pages = limits.max_pages;
    state.crawling = true;
    state.crawl_started = Some(Instant::now());
    state.crawl_elapsed = None;
    state.export_notice = None;
    state.pages.clear();
    state.visible.clear();
    state.audit = Audit::default();
    state.sitemap = SitemapReport::default();
    state.sitemap.discovery_done = !state.use_sitemaps;
    state.sitemap_discovering = state.use_sitemaps;
    state.sitemap_visible.clear();
    state.sitemap_file_visible.clear();
    state.table.clear_selection();
    state.message = format!("Crawling {seed}…");

    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let waker = state.waker.clone();
    let options = crawl::CrawlOptions {
        sitemaps: state.use_sitemaps,
    };
    state.job = Some(CrawlJob {
        rx,
        cancel: cancel.clone(),
    });
    std::thread::spawn(move || {
        let event_tx = tx.clone();
        let wake = waker.clone();
        let outcome = crawl::crawl_site_with(&seed, limits, options, &cancel, |event| {
            let event = match event {
                crawl::CrawlEvent::Page(page) => CrawlEvent::Page(Box::new(page)),
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
        let _ = tx.send(CrawlEvent::Done(outcome.message, Box::new(outcome.sitemap)));
        if let Some(wake) = &waker {
            wake();
        }
    });
}

fn stop_crawl(state: &mut State) {
    if let Some(job) = &state.job {
        job.cancel.store(true, Ordering::Relaxed);
        state.message = "Stopping…".into();
    }
}

fn refresh_visible(state: &mut State) {
    if state.tab == Tab::Sitemaps {
        refresh_sitemap_visible(state);
        return;
    }
    let tab = state.tab;
    let issues_only = state.issues_only;
    let pages = &state.pages;
    let audit = &state.audit;
    let visible = pages
        .iter()
        .enumerate()
        .filter(|(_, page)| !issues_only || !issue_for(tab, page, audit).is_empty())
        .map(|(index, _)| index)
        .collect();
    state.visible = visible;
}

fn refresh_sitemap_visible(state: &mut State) {
    let issues_only = state.issues_only;
    match state.sitemap_view {
        SitemapView::Urls => {
            state.sitemap_visible = state
                .sitemap
                .urls
                .iter()
                .enumerate()
                .filter(|(_, entry)| !issues_only || entry.needs_attention())
                .map(|(index, _)| index)
                .collect();
        }
        SitemapView::Files => {
            state.sitemap_file_visible = state
                .sitemap
                .files
                .iter()
                .enumerate()
                .filter(|(_, file)| !issues_only || file.needs_attention())
                .map(|(index, _)| index)
                .collect();
        }
    }
}

fn sort_sitemap(state: &mut State) {
    refresh_sitemap_visible(state);
    let Some((column, dir)) = state
        .table
        .sort()
        .map(|(column, dir)| (column.to_string(), dir))
    else {
        return;
    };
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

fn compare_sitemap_url(a: &SitemapUrl, b: &SitemapUrl, column: &str) -> std::cmp::Ordering {
    match column {
        "state" => a.state.label().cmp(b.state.label()),
        "status" => a.status.cmp(&b.status),
        "issue" => a.issue.cmp(&b.issue),
        "source" => a.sources.first().cmp(&b.sources.first()),
        _ => a.url.cmp(&b.url),
    }
}

fn compare_sitemap_file(a: &SitemapFile, b: &SitemapFile, column: &str) -> std::cmp::Ordering {
    match column {
        "state" => a.state.label().cmp(b.state.label()),
        "status" => a.status.cmp(&b.status),
        "urls" => a.urls.cmp(&b.urls),
        "issue" => a.issue.cmp(&b.issue),
        _ => a.url.cmp(&b.url),
    }
}

fn set_pages(state: &mut State, pages: Vec<Page>, message: String) {
    state.pages = pages;
    state.audit = Audit::from_pages(&state.pages);
    state.message = message;
    sort_pages(state);
    if let Some(url) = state.pages.first().map(|page| page.url.clone()) {
        state.table.select(url);
    } else {
        state.table.clear_selection();
    }
}

fn load_sample(state: &mut State) {
    state.url.replace_text("https://example.com/");
    state.crawling = false;
    state.issues_only = false;
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

fn sort_pages(state: &mut State) {
    if state.tab == Tab::Sitemaps {
        sort_sitemap(state);
        return;
    }
    let tab = state.tab;
    let Some((column, dir)) = state
        .table
        .sort()
        .map(|(column, dir)| (column.to_string(), dir))
    else {
        refresh_visible(state);
        return;
    };
    let State { pages, audit, .. } = state;
    pages.sort_by(|a, b| dir.apply(compare(a, b, tab, &column, audit)));
    refresh_visible(state);
}

fn compare(a: &Page, b: &Page, tab: Tab, column: &str, audit: &Audit) -> std::cmp::Ordering {
    match (tab, column) {
        (_, "url") => a.url.cmp(&b.url),
        (Tab::Internal | Tab::Response, "status") => a.status.cmp(&b.status),
        (Tab::Internal, "indexable") => index_issue(a).cmp(index_issue(b)),
        (Tab::Internal, "words") => a.words.cmp(&b.words),
        (Tab::Titles, "title") => a.title.cmp(&b.title),
        (Tab::Titles, "len") => a.title.chars().count().cmp(&b.title.chars().count()),
        (Tab::Titles, "issue") => title_issue(a, audit).cmp(&title_issue(b, audit)),
        (Tab::Meta, "meta") => a.meta_description.cmp(&b.meta_description),
        (Tab::Meta, "len") => a
            .meta_description
            .chars()
            .count()
            .cmp(&b.meta_description.chars().count()),
        (Tab::Meta, "issue") => meta_issue(a, audit).cmp(&meta_issue(b, audit)),
        (Tab::H1, "h1") => a.h1().cmp(&b.h1()),
        (Tab::H1, "count") => a.h1s.len().cmp(&b.h1s.len()),
        (Tab::H1, "issue") => h1_issue(a, audit).cmp(&h1_issue(b, audit)),
        (Tab::Response, "type") => a.content_type.cmp(&b.content_type),
        (Tab::Response, "issue") => response_issue(a).cmp(&response_issue(b)),
        (Tab::Canonical, "canonical") => a.canonical.cmp(&b.canonical),
        (Tab::Canonical, "issue") => canonical_issue(a).cmp(&canonical_issue(b)),
        (Tab::Images, "images") => a.images.cmp(&b.images),
        (Tab::Images, "missing_alt") => a.images_missing_alt.cmp(&b.images_missing_alt),
        (Tab::Images, "issue") => image_issue(a).cmp(&image_issue(b)),
        (Tab::Links, "inbound") => audit.inbound(a).cmp(&audit.inbound(b)),
        (Tab::Links, "outbound") => a.links.len().cmp(&b.links.len()),
        (Tab::Links, "issue") => links_issue(a, audit).cmp(&links_issue(b, audit)),
        _ => a.url.cmp(&b.url),
    }
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
        Tab::Internal => index_issue(page).to_string(),
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
    let counts = counts_with(&state.pages, &state.audit);
    let mut app = col()
        .key("app")
        .w_full()
        .h_full()
        .fill(k.c.page)
        .child(topbar(&k, state, &counts, inset));
    if state.crawling {
        app = app.child(progress_line(&k, state));
    }
    app = app.child(command_bar(&k, state));
    if let Some(banner) = update_banner(&k, state) {
        app = app.child(banner);
    }
    app.child(section_header(&k, state))
        .child(results(&k, state))
        .child(status_line(&k, state))
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
    let mut bar = row()
        .w_full()
        .h(52.0)
        .pl(GUTTER + inset)
        .pr(GUTTER)
        .center_y()
        .gap(16.0)
        .child(icon(&TOAD_MARK, 20.0).key("toad-mark").label("Calm Toad"))
        .child(tabs)
        .child(el().grow(1.0));
    if let Some(chip) = update_chip(k, state) {
        bar = bar.child(chip);
    }
    bar.child(crawl_state(k, state))
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
}

fn update_chip(k: &Kit, state: &State) -> Option<El> {
    match &state.update {
        UpdateUi::Available(offered) => Some(
            Button::new(format!("Update {}", offered.version))
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Regular)
                .help(format!("Download Calm Toad {}", offered.version))
                .on_press(start_download)
                .build(k),
        ),
        UpdateUi::Downloading { .. } => Some(
            text("Updating…")
                .font(k.ty.small)
                .color(k.c.warning.fg)
                .text_case(TextCase::Uppercase),
        ),
        UpdateUi::Checking => Some(
            text("Checking…")
                .font(k.ty.small)
                .color(k.c.text_muted)
                .text_case(TextCase::Uppercase),
        ),
        _ => None,
    }
}

fn update_banner(k: &Kit, state: &State) -> Option<El> {
    match &state.update {
        UpdateUi::Available(offered) => {
            let mut actions = vec![
                Button::new("Download update")
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
            if state.crawling {
                "Stop the crawl before restarting to install.".into()
            } else {
                "Restarting will clear the current crawl results.".into()
            },
            vec![
                Button::new("Restart and install")
                    .variant(ButtonVariant::Neutral)
                    .control_size(ControlSize::Regular)
                    .disabled(state.crawling)
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
    col().w_full().px(GUTTER).pb(12.0).child(
        body.px(14.0)
            .py(10.0)
            .fill(k.c.layer_01)
            .border(1.0, k.c.border_weak)
            .radius(k.radius.lg),
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

fn crawl_state(k: &Kit, state: &State) -> El {
    let (label, color) = if state.crawling {
        ("Crawling".to_string(), k.c.warning.fg)
    } else if failed(state) {
        ("Error".to_string(), k.c.danger.fg)
    } else if !state.pages.is_empty() {
        (format!("{} pages", state.pages.len()), k.c.success.fg)
    } else {
        ("Ready".to_string(), k.c.text_muted)
    };
    row()
        .gap(7.0)
        .center_y()
        .child(dot(color))
        .child(
            text(label)
                .font(k.ty.small)
                .color(color)
                .text_case(TextCase::Uppercase),
        )
        .help("Crawl status")
}

fn failed(state: &State) -> bool {
    state.pages.is_empty() && !state.crawling && !state.message.starts_with("Ready")
}

fn command_bar(k: &Kit, state: &State) -> El {
    col().w_full().px(GUTTER).py(14.0).child(
        row()
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
            .child(sitemaps_toggle(k, state))
            .child(crawl_button(k, state)),
    )
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
        .on_submit(|state: &mut State, _| start_crawl(state))
        .disabled(state.crawling)
        .build(k)
        .grow(1.0)
        .min_w(0.0)
        .key("url")
}

fn pages_field(k: &Kit, state: &State) -> El {
    row()
        .h_full()
        .px(12.0)
        .gap(8.0)
        .center_y()
        .child(micro(k, "Pages"))
        .child(
            rune_kit::TextInput::new(&state.pages_limit)
                .plain()
                .height(38.0)
                .width(Some(76.0))
                .text_style(k.ty.mono)
                .label("Page limit")
                .on_edit(|state: &mut State| &mut state.pages_limit)
                .on_submit(|state: &mut State, _| start_crawl(state))
                .disabled(state.crawling)
                .build(k)
                .key("pages-limit"),
        )
}

fn depth_field(k: &Kit, state: &State) -> El {
    row()
        .h_full()
        .px(12.0)
        .gap(8.0)
        .center_y()
        .child(micro(k, "Depth"))
        .child(
            rune_kit::TextInput::new(&state.depth_limit)
                .plain()
                .height(38.0)
                .width(Some(42.0))
                .text_style(k.ty.mono)
                .label("Depth limit")
                .on_edit(|state: &mut State| &mut state.depth_limit)
                .on_submit(|state: &mut State, _| start_crawl(state))
                .disabled(state.crawling)
                .build(k)
                .key("depth-limit"),
        )
}

fn sitemaps_toggle(k: &Kit, state: &State) -> El {
    row().h_full().px(12.0).center_y().child(
        Checkbox::new(state.use_sitemaps)
            .label("Use sitemaps")
            .description("Read robots.txt and sitemap files")
            .disabled(state.crawling)
            .on_toggle(|state: &mut State| state.use_sitemaps ^= true)
            .build(k)
            .key("use-sitemaps"),
    )
}

fn crawl_button(k: &Kit, state: &State) -> El {
    let font = TextStyle::new(10.5, 500.0).mono().lh(14.0).tracking(1.2);
    if state.crawling {
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
        Button::new("Crawl")
            .variant(ButtonVariant::Submit)
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
        .control_size(ControlSize::Regular)
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
            state.table.set_sort("url", SortDir::Ascending);
            sort_pages(state);
        })
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
    let total = state.pages.len();
    let shown = visible_len(state);
    let summary = if total == 0 {
        if state.crawling {
            "Fetching pages".to_string()
        } else if failed(state) {
            "Nothing crawled".to_string()
        } else {
            "No pages yet".to_string()
        }
    } else if state.issues_only {
        format!("{shown} of {total} pages")
    } else {
        format!("{total} pages crawled")
    };
    row()
        .w_full()
        .px(GUTTER)
        .pt(12.0)
        .pb(14.0)
        .center_y()
        .gap(16.0)
        .child(
            col()
                .gap(3.0)
                .child(micro(k, active_title(state)))
                .child(text(summary).font(k.ty.text_small).color(k.c.text_muted)),
        )
        .child(el().grow(1.0))
        .child(export_button(k, state, shown))
        .child(details_toggle(k, state))
        .child(issues_checkbox(k, state))
}

fn sitemap_section_header(k: &Kit, state: &State) -> El {
    let total = match state.sitemap_view {
        SitemapView::Urls => state.sitemap.urls.len(),
        SitemapView::Files => state.sitemap.files.len(),
    };
    let shown = visible_len(state);
    let summary = if state.issues_only {
        format!("{shown} of {total} shown")
    } else {
        sitemap_summary(state)
    };
    let view = |label: &'static str, view: SitemapView, key: &'static str| {
        Button::new(label)
            .variant(if state.sitemap_view == view {
                ButtonVariant::Neutral
            } else {
                ButtonVariant::Ghost
            })
            .control_size(ControlSize::Regular)
            .help(match view {
                SitemapView::Urls => "URLs listed by the sitemaps",
                SitemapView::Files => "Sitemap files that were read or tried",
            })
            .on_press(move |state: &mut State| {
                state.sitemap_view = view;
                state.table.set_sort("url", SortDir::Ascending);
                sort_sitemap(state);
            })
            .build(k)
            .key(key)
    };
    row()
        .w_full()
        .px(GUTTER)
        .pt(12.0)
        .pb(14.0)
        .center_y()
        .gap(12.0)
        .child(
            col()
                .gap(3.0)
                .grow(1.0)
                .min_w(0.0)
                .child(micro(k, active_title(state)))
                .child(
                    text(summary)
                        .font(k.ty.text_small)
                        .color(k.c.text_muted)
                        .w_full()
                        .wrap(),
                ),
        )
        .child(view("URLs", SitemapView::Urls, "sitemap-urls"))
        .child(view("Files", SitemapView::Files, "sitemap-files"))
        .child(export_button(k, state, shown))
        .child(details_toggle(k, state))
        .child(issues_checkbox(k, state))
}

fn sitemap_summary(state: &State) -> String {
    let summary = state.sitemap.summary();
    if !state.sitemap.discovery_done {
        if !state.crawling && !state.sitemap_discovering {
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
    if !state.use_sitemaps {
        return "Sitemaps are off".to_string();
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

fn export_button(k: &Kit, state: &State, shown: usize) -> El {
    Button::new("Export CSV")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .help(format!(
            "Save the {shown} visible rows from {} in the current sort order",
            active_title(state)
        ))
        .disabled(state.crawling || shown == 0)
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

fn issues_checkbox(k: &Kit, state: &State) -> El {
    Checkbox::new(state.issues_only)
        .label("Issues only")
        .on_toggle(|state: &mut State| {
            state.issues_only ^= true;
            refresh_visible(state);
        })
        .build(k)
        .key("issues")
}

fn export_csv(state: &mut State, tx: &mut Tx) {
    if state.crawling {
        return;
    }
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
        save_csv(state, tx, headers, rows, title, slug);
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
    save_csv(state, tx, headers, rows, active_title(state), tab_slug(tab));
}

fn save_csv(
    state: &mut State,
    tx: &mut Tx,
    headers: &'static [&'static str],
    rows: Vec<Vec<String>>,
    title: &'static str,
    slug: &'static str,
) {
    let csv = calmtoad::export::to_csv(headers, &rows);
    let row_count = rows.len();
    let options = FileDialogOptions {
        title: format!("Export {title} as CSV"),
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
                            Some(Ok(format!("Exported {row_count} rows to {name}")));
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
        Tab::Internal => &["ADDRESS", "STATUS", "INDEXABLE", "WORDS"],
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
        SitemapView::Urls => &["ADDRESS", "STATE", "STATUS", "SOURCE", "ISSUE"],
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

fn export_row(tab: Tab, page: &Page, audit: &Audit) -> Vec<String> {
    let issue = || {
        if matches!(
            tab,
            Tab::Titles | Tab::Meta | Tab::H1 | Tab::Canonical | Tab::Images
        ) && !auditable(page)
        {
            "—".to_string()
        } else {
            let issue = issue_for(tab, page, audit);
            if issue.is_empty() { "OK".into() } else { issue }
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
            let index = index_issue(page);
            row.push(if index.is_empty() {
                "Yes".into()
            } else {
                index.into()
            });
            row.push(page.words.to_string());
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
    let has_rows = if state.tab == Tab::Sitemaps {
        match state.sitemap_view {
            SitemapView::Urls => !state.sitemap.urls.is_empty(),
            SitemapView::Files => !state.sitemap.files.is_empty(),
        }
    } else {
        !state.pages.is_empty()
    };
    if !has_rows {
        return frame.child(empty(k, state));
    }
    let frame = frame.child(table(k, state).grow(1.0).min_w(0.0).h_full());
    if state.details_open {
        frame.child(detail(k, state).w(330.0).h_full())
    } else {
        frame
    }
}

fn empty(k: &Kit, state: &State) -> El {
    let (title, copy) = if state.tab == Tab::Sitemaps {
        sitemap_empty(state)
    } else if state.crawling {
        (
            "Fetching pages".to_string(),
            "Following links on this site, up to 40 pages and 3 levels deep.".to_string(),
        )
    } else if state.message.starts_with("Ready") {
        (
            "Crawl a site".to_string(),
            "Check titles, descriptions, headings, responses, images and links.".to_string(),
        )
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
                .key("empty-copy"),
        )
}

fn sitemap_empty(state: &State) -> (String, String) {
    if !state.use_sitemaps {
        return (
            "Sitemaps are off".into(),
            "Turn on Use sitemaps and crawl again to read this site’s sitemaps.".into(),
        );
    }
    if state.crawling || state.sitemap_discovering {
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
        |state: &State, row| {
            Cow::Owned(
                state
                    .pages
                    .get(visible_index(state, row))
                    .map(|page| page.url.clone())
                    .unwrap_or_default(),
            )
        },
    )
    .name(tab_title(tab))
    .style(table_style(k))
    .zebra(false)
    .on_sort(|state: &mut State, column, dir, _| {
        state.table.set_sort(column, dir);
        sort_pages(state);
    });
    table = table.on_select(|state: &mut State, _id, _tx| {
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
                TableColumn::new("status", "STATUS")
                    .width(96.0)
                    .align(Align::End)
                    .sortable(),
            )
            .column(
                TableColumn::new("indexable", "INDEXABLE")
                    .width(150.0)
                    .sortable(),
            )
            .column(
                TableColumn::new("words", "WORDS")
                    .width(96.0)
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
                TableColumn::new("status", "STATUS")
                    .width(96.0)
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
            |state: &State, row| {
                Cow::Owned(
                    state
                        .sitemap_visible
                        .get(row)
                        .and_then(|index| state.sitemap.urls.get(*index))
                        .map(|entry| entry.url.clone())
                        .unwrap_or_default(),
                )
            },
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
        .on_sort(|state: &mut State, column, dir, _| {
            state.table.set_sort(column, dir);
            sort_sitemap(state);
        })
        .on_select(|state: &mut State, _id, _tx| {
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
            |state: &State, row| {
                Cow::Owned(
                    state
                        .sitemap_file_visible
                        .get(row)
                        .and_then(|index| state.sitemap.files.get(*index))
                        .map(|file| file.url.clone())
                        .unwrap_or_default(),
                )
            },
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
        .on_sort(|state: &mut State, column, dir, _| {
            state.table.set_sort(column, dir);
            sort_sitemap(state);
        })
        .on_select(|state: &mut State, _id, _tx| {
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
        "issue" => issue_cell(k, &entry.issue),
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
        (Tab::Internal, "indexable") => {
            let issue = index_issue(page);
            if issue.is_empty() {
                Cell::text("Yes").muted()
            } else {
                Cell::text(issue).color(k.c.danger.fg)
            }
        }
        (Tab::Internal, "words") => Cell::text(page.words.to_string()).mono().muted(),
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
        (_, "issue") => {
            // A 404 has no title to judge. Say so, instead of calling it OK.
            if matches!(
                tab,
                Tab::Titles | Tab::Meta | Tab::H1 | Tab::Canonical | Tab::Images
            ) && !auditable(page)
            {
                Cell::text("—").muted()
            } else {
                issue_cell(k, &issue_for(tab, page, audit))
            }
        }
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

fn detail(k: &Kit, state: &State) -> El {
    if state.tab == Tab::Sitemaps {
        return sitemap_detail(k, state);
    }
    let page = state
        .table
        .cursor()
        .and_then(|id| state.pages.iter().find(|page| page.url == id));
    let panel = col()
        .key("detail")
        .w(330.0)
        .h_full()
        .gap(12.0)
        .px(16.0)
        .py(16.0)
        .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_weak);

    let Some(page) = page else {
        return panel.child(micro(k, "Page details")).child(
            text("Select a result to inspect its page details.")
                .font(k.ty.body)
                .color(k.c.text_muted)
                .wrap()
                .w_full(),
        );
    };

    let page_url = page.url.clone();
    let actions = row()
        .gap(6.0)
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
    panel
        .child(
            row()
                .w_full()
                .center_y()
                .child(micro(k, "Selected page"))
                .child(el().grow(1.0))
                .child(actions),
        )
        .child(
            text(page.url.clone())
                .key("detail-url")
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .wrap()
                .w_full(),
        )
        .child(el().w_full().h(1.0).fill(k.c.border_weak))
        .child(detail_field(k, "Title", &dash(&page.title)))
        .child(detail_field(
            k,
            "Meta description",
            &dash(&page.meta_description),
        ))
        .child(detail_field(k, "H1", &dash(&page.h1())))
        .child(detail_field(k, "Canonical", &dash(&page.canonical)))
        .child(
            row()
                .w_full()
                .gap(20.0)
                .child(
                    col().w(70.0).gap(4.0).child(micro(k, "Status")).child(
                        text(page.status.to_string())
                            .font(k.ty.body)
                            .color(k.c.text_base),
                    ),
                )
                .child(
                    col().w(90.0).gap(4.0).child(micro(k, "Words")).child(
                        text(page.words.to_string())
                            .font(k.ty.body)
                            .color(k.c.text_base),
                    ),
                ),
        )
        .child(detail_field(
            k,
            "Content type",
            &dash(&mime(&page.content_type)),
        ))
        .child(detail_field(
            k,
            "Images · missing alt",
            &format!("{} · {}", page.images, page.images_missing_alt),
        ))
}

fn sitemap_detail(k: &Kit, state: &State) -> El {
    let panel = col()
        .key("detail")
        .w(330.0)
        .h_full()
        .gap(12.0)
        .px(16.0)
        .py(16.0)
        .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_weak);
    let cursor = state.table.cursor();

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
            Some(entry) => ("Sitemap URL", entry.url.clone(), sitemap_url_fields(entry)),
            None => match file {
                Some(file) => ("Sitemap file", file.url.clone(), sitemap_file_fields(file)),
                None => return sitemap_detail_empty(k, panel, "URL"),
            },
        },
        SitemapView::Files => match file {
            Some(file) => ("Sitemap file", file.url.clone(), sitemap_file_fields(file)),
            None => match url_entry {
                Some(entry) => ("Sitemap URL", entry.url.clone(), sitemap_url_fields(entry)),
                None => return sitemap_detail_empty(k, panel, "file"),
            },
        },
    };

    let copy_url = selected.clone();
    let open_url = selected.clone();
    let actions = row()
        .gap(6.0)
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
        );
    let mut body = panel
        .child(
            row()
                .w_full()
                .center_y()
                .child(micro(k, heading))
                .child(el().grow(1.0))
                .child(actions),
        )
        .child(
            text(selected)
                .key("detail-url")
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .wrap()
                .w_full(),
        )
        .child(el().w_full().h(1.0).fill(k.c.border_weak));
    for (label, value) in fields {
        body = body.child(detail_field(k, label, &value));
    }
    body
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

fn sitemap_url_fields(entry: &SitemapUrl) -> Vec<(&'static str, String)> {
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
        .gap(4.0)
        .child(micro(k, label))
        .child(
            text(value.to_string())
                .font(k.ty.body)
                .color(k.c.text_base)
                .wrap()
                .w_full(),
        )
}

fn status_line(k: &Kit, state: &State) -> El {
    let shown = visible_len(state);
    let message = if state.crawling {
        let elapsed = state
            .crawl_started
            .map(|started| started.elapsed())
            .unwrap_or_default();
        format!("{} pages · {}", state.pages.len(), format_duration(elapsed))
    } else if let Some(notice) = &state.export_notice {
        match notice {
            Ok(message) | Err(message) => message.clone(),
        }
    } else if let Some(elapsed) = state.crawl_elapsed {
        if state.issues_only {
            format!(
                "{} pages crawled in {} · {shown} shown",
                state.pages.len(),
                format_duration(elapsed)
            )
        } else {
            format!(
                "{} pages crawled in {}",
                state.pages.len(),
                format_duration(elapsed)
            )
        }
    } else if state.pages.is_empty() || !state.issues_only {
        state.message.clone()
    } else {
        format!("{} · {shown} of {} shown", state.message, state.pages.len())
    };
    let color = if state.crawling {
        k.c.warning.fg
    } else if state.export_notice.as_ref().is_some_and(Result::is_err) || failed(state) {
        k.c.danger.fg
    } else if !state.pages.is_empty() {
        k.c.success.fg
    } else {
        k.c.border_strong
    };
    row()
        .key("status")
        .w_full()
        .h(34.0)
        .px(GUTTER)
        .center_y()
        .gap(9.0)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_weak)
        .child(
            if !state.crawling
                && state.pages.is_empty()
                && state.export_notice.is_none()
                && state.message.starts_with("Ready")
            {
                row()
            } else {
                row().center_y().gap(9.0).child(dot(color)).child(
                    text(message)
                        .font(TextStyle::new(10.5, 400.0).mono().lh(15.0).tracking(0.2))
                        .color(k.c.text_muted),
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

fn scene_empty(_state: &mut State) {}
fn scene_sample(state: &mut State) {
    load_sample(state);
}
fn scene_titles(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Titles;
}
fn scene_meta(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Meta;
}
fn scene_h1(state: &mut State) {
    load_sample(state);
    state.tab = Tab::H1;
}
fn scene_response(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Response;
}
fn scene_canonical(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Canonical;
}
fn scene_images(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Images;
}
fn scene_links(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Links;
}
fn scene_sitemaps(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Sitemaps;
    state.table.set_sort("url", SortDir::Ascending);
    sort_pages(state);
}
fn scene_sitemap_files(state: &mut State) {
    scene_sitemaps(state);
    state.sitemap_view = SitemapView::Files;
    sort_sitemap(state);
}
fn scene_title_issues(state: &mut State) {
    load_sample(state);
    state.tab = Tab::Titles;
    state.issues_only = true;
    sort_pages(state);
}
fn scene_error(state: &mut State) {
    state.crawling = false;
    state.pages.clear();
    state.message = "Couldn’t read “not a url” as a URL.".into();
}
fn scene_crawling(state: &mut State) {
    state.pages.clear();
    state.crawling = true;
    state.message = "Crawling https://example.com/…".into();
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
            ("title-issues", scene_title_issues),
            ("error", scene_error),
            ("crawling", scene_crawling),
            ("update-available", scene_update_available),
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
    .menu_item(MenuItem::new("Check for Updates…", check_for_updates_menu))
    .run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

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

        let (x, y) = rt.find("issues").expect("issues-only checkbox");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(rt.state.issues_only);
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
        assert!(!rt.state.crawling);
    }

    #[test]
    fn csv_export_uses_visible_rows_in_the_active_tab_order() {
        let mut state = init();
        load_sample(&mut state);
        state.tab = Tab::Titles;
        state.issues_only = true;
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

        state.crawling = true;
        assert!(!can_install(&state));

        state.crawling = false;
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

        let (x, y) = rt.find("issues").expect("issues-only checkbox");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert!(rt.state.issues_only);
        assert!(!rt.state.sitemap_visible.is_empty());
        assert!(
            rt.state
                .sitemap_visible
                .iter()
                .all(|index| rt.state.sitemap.urls[*index].needs_attention())
        );

        let (x, y) = rt.find("sitemap-files").expect("file view toggle");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.sitemap_view, SitemapView::Files);
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
        state.issues_only = true;
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

        state.crawling = true;
        assert_eq!(sitemap_summary(&state), "Reading sitemaps");

        state.crawling = false;
        state.use_sitemaps = false;
        state.sitemap.discovery_done = true;
        assert_eq!(sitemap_summary(&state), "Sitemaps are off");
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
