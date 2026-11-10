//! A desktop SEO spider: crawl one host, then open the views people use first.
//!
//! Internal URLs, titles, meta descriptions, H1s, response codes, canonicals, image alt text and
//! internal links. `cargo run` opens the window. `cargo run -- --snap shots/sample.png --scene sample --size 1180x740`
//! renders it headless.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use calmtoad::crawl;
use calmtoad::crawl::{
    Audit, Counts, Limits, MAX_DEPTH, MAX_PAGES, Page, auditable, canonical_issue, counts_with,
    crawl_site_stream, h1_issue, image_issue, index_issue, links_issue, meta_issue, redirect_issue,
    response_issue, title_issue,
};
use rune::tooling::AppSpec;
use rune::{Align, Color, Cx, El, TextCase, TextEdit, TextStyle, Tx, Waker, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, Cell, Checkbox, ControlSize, Kit, SortDir, Table, TableColumn,
    TableState, TableStyle,
};

mod theme;

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
}

enum CrawlEvent {
    Page(Box<Page>),
    Done(String),
}

struct CrawlJob {
    rx: mpsc::Receiver<CrawlEvent>,
    cancel: Arc<AtomicBool>,
}

struct State {
    url: TextEdit,
    pages_limit: TextEdit,
    depth_limit: TextEdit,
    tab: Tab,
    issues_only: bool,
    table: TableState,
    pages: Vec<Page>,
    /// Indices into `pages` after the current tab's "issues only" filter, in display order.
    visible: Vec<usize>,
    audit: Audit,
    message: String,
    crawling: bool,
    crawl_started: Option<Instant>,
    crawl_elapsed: Option<Duration>,
    max_pages: usize,
    job: Option<CrawlJob>,
    waker: Option<Waker>,
}

fn init() -> State {
    let mut table = TableState::new().key("grid");
    table.set_sort("url", SortDir::Ascending);
    State {
        url: TextEdit::new("https://example.com/"),
        pages_limit: TextEdit::new(MAX_PAGES.to_string()),
        depth_limit: TextEdit::new(MAX_DEPTH.to_string()),
        tab: Tab::Internal,
        issues_only: false,
        table,
        pages: Vec::new(),
        visible: Vec::new(),
        audit: Audit::default(),
        message: "Ready · enter a site URL to begin. Up to 40 pages and 3 levels deep.".into(),
        crawling: false,
        crawl_started: None,
        crawl_elapsed: None,
        max_pages: MAX_PAGES,
        job: None,
        waker: None,
    }
}

fn setup(state: &mut State, waker: Waker) {
    state.waker = Some(waker);
}

fn poll(state: &mut State, _t: f64, _tx: &mut Tx) -> bool {
    let Some(job) = state.job.take() else {
        return false;
    };
    let start_len = state.pages.len();
    let mut changed = false;
    let mut finished = None;
    loop {
        match job.rx.try_recv() {
            Ok(CrawlEvent::Page(page)) => {
                state.pages.push(*page);
                changed = true;
            }
            Ok(CrawlEvent::Done(message)) => {
                finished = Some(message);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = Some(format!("{} pages on the site", state.pages.len()));
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
    }
    let did_finish = finished.is_some();
    match finished {
        Some(message) => {
            state.crawling = false;
            state.crawl_elapsed = state.crawl_started.take().map(|started| started.elapsed());
            state.message = message;
            state.audit = Audit::from_pages(&state.pages);
            sort_pages(state);
            if let Some(url) = state.pages.first().map(|page| page.url.clone()) {
                state.table.select(url);
            } else {
                state.table.clear_selection();
            }
        }
        None => state.job = Some(job),
    }
    changed || did_finish
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
            .unwrap_or(MAX_PAGES)
            .min(2_000),
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
    state.pages.clear();
    state.visible.clear();
    state.audit = Audit::default();
    state.table.clear_selection();
    state.message = format!("Crawling {seed}…");

    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let waker = state.waker.clone();
    state.job = Some(CrawlJob {
        rx,
        cancel: cancel.clone(),
    });
    std::thread::spawn(move || {
        let page_tx = tx.clone();
        let wake = waker.clone();
        let outcome = crawl_site_stream(&seed, limits, &cancel, |page| {
            let _ = page_tx.send(CrawlEvent::Page(Box::new(page.clone())));
            if let Some(wake) = &wake {
                wake();
            }
        });
        let _ = tx.send(CrawlEvent::Done(outcome.message));
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
    set_pages(
        state,
        crawl::sample_pages(),
        "9 pages on example.com".into(),
    );
}

fn sort_pages(state: &mut State) {
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
    }
}

fn tab_label(tab: Tab, counts: &Counts) -> String {
    match tab {
        Tab::Internal => format!("Internal {}", counts.pages),
        Tab::Titles => format!("Titles {}", counts.title),
        Tab::Meta => format!("Meta {}", counts.meta),
        Tab::H1 => format!("H1 {}", counts.h1),
        Tab::Response => format!("Response {}", counts.response),
        Tab::Canonical => format!("Canonical {}", counts.canonical),
        Tab::Images => format!("Images {}", counts.images),
        Tab::Links => format!("Links {}", counts.links),
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
    }
}

fn tab_count(tab: Tab, counts: &Counts) -> usize {
    match tab {
        Tab::Internal => counts.pages,
        Tab::Titles => counts.title,
        Tab::Meta => counts.meta,
        Tab::H1 => counts.h1,
        Tab::Response => counts.response,
        Tab::Canonical => counts.canonical,
        Tab::Images => counts.images,
        Tab::Links => counts.links,
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
    app.child(command_bar(&k, state))
        .child(section_header(&k, state))
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
        ]
        .into_iter()
        .map(|tab| tab_button(k, state, tab, counts)),
    );
    row()
        .w_full()
        .h(52.0)
        .pl(GUTTER + inset)
        .pr(GUTTER)
        .center_y()
        .gap(16.0)
        .child(tabs)
        .child(el().grow(1.0))
        .child(crawl_state(k, state))
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
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
                .width(Some(42.0))
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
    let count = tab_count(tab, counts);
    let count_ink = k.c.text_muted;
    let label_ink = if selected {
        k.c.text_base
    } else {
        k.c.text_muted
    };
    let small = k.ty.small;
    Button::new(tab_label(tab, counts))
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
    let issues = Checkbox::new(state.issues_only)
        .label("Issues only")
        .on_toggle(|state: &mut State| {
            state.issues_only ^= true;
            refresh_visible(state);
        })
        .build(k)
        .key("issues");
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
                .child(micro(k, tab_title(state.tab)))
                .child(text(summary).font(k.ty.text_small).color(k.c.text_muted)),
        )
        .child(el().grow(1.0))
        .child(issues)
}

fn results(k: &Kit, state: &State) -> El {
    let frame = row().key("frame").w_full().grow(1.0).min_h(0.0);
    if state.pages.is_empty() {
        return frame.child(empty(k, state));
    }
    frame
        .child(table(k, state).grow(1.0).min_w(0.0).h_full())
        .child(detail(k, state).w(330.0).h_full())
}

fn empty(k: &Kit, state: &State) -> El {
    let (title, copy) = if state.crawling {
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

fn table(k: &Kit, state: &State) -> El {
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

    panel
        .child(micro(k, "Selected page"))
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
    } else if failed(state) {
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
        .child(dot(color))
        .child(
            text(message)
                .font(TextStyle::new(10.5, 400.0).mono().lh(15.0).tracking(0.2))
                .color(k.c.text_muted),
        )
        .child(el().grow(1.0))
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

fn scene_pmprompt(state: &mut State) {
    const SEED: &str = "https://pmprompt.com/";
    state.url.replace_text(SEED);
    state.pages_limit.replace_text(MAX_PAGES.to_string());
    state.depth_limit.replace_text(MAX_DEPTH.to_string());
    let started = Instant::now();
    let outcome = crawl::crawl_site(SEED);
    state.max_pages = MAX_PAGES;
    set_pages(state, outcome.pages, outcome.message);
    state.crawl_elapsed = Some(started.elapsed());
}

fn main() {
    rune::run(AppSpec {
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
            ("title-issues", scene_title_issues),
            ("error", scene_error),
            ("crawling", scene_crawling),
            ("pmprompt", scene_pmprompt),
        ],
        setup: Some(setup),
        poll: Some(poll),
        min_size: Some((880.0, 560.0)),
        ..AppSpec::new("Calm Toad", (1180.0, 740.0), init, view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    fn texts(rt: &Runtime<State>) -> String {
        rt.elements()
            .into_iter()
            .filter_map(|element| element.text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_five_tabs_switch_and_issues_only_hides_clean_titles() {
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
        let all = texts(&rt);
        assert!(rt.find("brand").is_none(), "the header has no brand mark");
        assert!(all.contains("Internal"), "{all}");
        assert!(all.contains("https://example.com/"), "{all}");
        assert!(all.contains("Welcome"), "{all}");

        let (x, y) = rt.find("tab-response").expect("response tab");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.016);
        assert_eq!(rt.state.tab, Tab::Response);
        let all = texts(&rt);
        assert!(all.contains("Client error") || all.contains("404"), "{all}");

        rt.state.tab = Tab::Titles;
        rt.state.issues_only = true;
        refresh_visible(&mut rt.state);
        rt.frame(0.016);
        let all = texts(&rt);
        assert!(
            all.contains("Too long") || all.contains("Too short") || all.contains("Duplicate"),
            "{all}"
        );
        assert!(
            !all.contains("Why a calm homepage converts better"),
            "{all}"
        );
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
        let all = texts(&rt);
        assert!(all.contains("Couldn’t crawl"), "{all}");
        assert!(all.contains("not a url"), "{all}");
        assert!(!all.contains("Crawl a site"), "{all}");
    }
}
