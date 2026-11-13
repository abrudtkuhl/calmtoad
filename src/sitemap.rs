//! Sitemap discovery, parsing and the audit view over the URLs they list.
//!
//! A sitemap is metadata, not a page: reading one never creates a [`Page`]. The crawl
//! engine schedules the page `loc` values through its ordinary fetch pipeline and keeps
//! this module's report beside the page results.
//!
//! Namespace policy: the standard sitemap namespace
//! (`http://www.sitemaps.org/schemas/sitemap/0.9`) and documents with no namespace at all
//! are accepted. A prefixed standard namespace (`sm:urlset`) is handled by resolving the
//! prefix. Elements in another namespace are ignored, which keeps image, video and
//! hreflang `loc` elements from becoming page seeds. DTDs are rejected rather than
//! expanded, so no external entity is ever fetched.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::GzDecoder;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use url::Url;

use crate::crawl::{Page, url_key};
use crate::indexability::{
    self, Assessment, EvidenceSource, PageEvidence, ReadState, RobotsDirective,
};

/// Maximum uncompressed XML accepted for one sitemap document (50 MiB).
pub const MAX_DOC_BYTES: u64 = 52_428_800;
/// Maximum compressed download accepted for one sitemap document (16 MiB).
pub const MAX_COMPRESSED_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum `url` or `sitemap` entries accepted in one document.
pub const MAX_ENTRIES_PER_DOC: usize = 50_000;
/// Maximum unique sitemap documents a crawl will read or report.
pub const MAX_DOCUMENTS: usize = 100;
/// Maximum index nesting. A root index is level 0; level 5 may still be read.
pub const MAX_INDEX_DEPTH: u8 = 5;
/// Maximum page URL entries processed across all sitemaps in one crawl.
pub const MAX_TOTAL_URLS: usize = 100_000;
/// Maximum redirect hops followed for one sitemap document.
pub const MAX_REDIRECTS: u8 = 5;

const SITEMAP_NS: &[u8] = b"http://www.sitemaps.org/schemas/sitemap/0.9";

/// A byte response from the sitemap fetch seam.
#[derive(Clone, Debug, Default)]
pub struct FetchedBytes {
    pub status: u16,
    pub content_type: String,
    pub location: String,
    pub body: Vec<u8>,
    pub error: String,
    /// `Retry-After` in milliseconds, when a rate-limited server sent one.
    pub retry_after_ms: u64,
}

/// What a parsed sitemap document turned out to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SitemapKind {
    #[default]
    Unknown,
    Urlset,
    Index,
}

/// How a sitemap file was handled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileState {
    #[default]
    Pending,
    /// Read and parsed. The document may still contain zero URLs.
    Ok,
    /// A guessed root that the site does not serve. This is absence, not an error.
    Absent,
    /// Declared, fetched, or parsed with a problem worth reporting.
    Error,
    /// Out of scope, blocked by robots.txt, nested too deeply, or otherwise not requested.
    Skipped,
}

/// How a URL listed by a sitemap was handled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UrlState {
    /// Waiting for a scheduled page fetch.
    #[default]
    Pending,
    /// A page record exists; `issue` holds the observed findings.
    Checked,
    /// robots.txt disallowed the URL, so it was never requested.
    Blocked,
    /// Invalid, out of scope, or otherwise never requested.
    Skipped,
    /// The crawl ended before this URL reached a worker. This is not a failure.
    NotChecked,
}

/// Where a sitemap file came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SitemapOrigin {
    /// Referenced by another sitemap index.
    #[default]
    Index,
    /// Declared in robots.txt.
    Robots,
    /// One of `/sitemap.xml` and `/sitemap_index.xml`.
    Guessed,
}

impl SitemapOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Robots => "robots.txt",
            Self::Guessed => "guessed",
        }
    }
}

impl UrlState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Checked => "Checked",
            Self::Blocked => "Blocked",
            Self::Skipped => "Skipped",
            Self::NotChecked => "Not checked",
        }
    }
}

impl FileState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Ok => "Read",
            Self::Absent => "Not found",
            Self::Error => "Error",
            Self::Skipped => "Skipped",
        }
    }
}

/// One sitemap document the crawl tried to read.
#[derive(Clone, Debug, Default)]
pub struct SitemapFile {
    pub url: String,
    pub origin: SitemapOrigin,
    pub depth: u8,
    pub state: FileState,
    pub status: u16,
    pub kind: SitemapKind,
    /// `url` or `sitemap` entries in the document, including entries without a usable loc.
    pub entries: usize,
    /// Valid absolute HTTP(S) page URLs in the document.
    pub urls: usize,
    pub issue: String,
}

impl SitemapFile {
    pub fn needs_attention(&self) -> bool {
        self.state == FileState::Error
            || (self.state != FileState::Ok
                && self.state != FileState::Pending
                && self.state != FileState::Absent)
            || (!self.issue.is_empty() && self.state != FileState::Absent)
    }
}

/// One URL listed by at least one sitemap.
#[derive(Clone, Debug, Default)]
pub struct SitemapUrl {
    pub url: String,
    /// Every sitemap file that listed this URL, in discovery order.
    pub sources: Vec<String>,
    pub state: UrlState,
    pub status: u16,
    pub issue: String,
    pub redirect_to: String,
    pub canonical: String,
    pub content_type: String,
    pub error: String,
    /// The same assessment the Internal tab and its drawer read, for checked URLs.
    ///
    /// Boxed so streaming this enum does not make every event carry the full assessment.
    pub assessment: Box<Assessment>,
}

impl SitemapUrl {
    pub fn needs_attention(&self) -> bool {
        match self.state {
            UrlState::Checked => self.assessment.needs_attention(),
            UrlState::Blocked | UrlState::Skipped | UrlState::NotChecked => true,
            UrlState::Pending => false,
        }
    }
}

/// The sitemap half of one crawl.
#[derive(Clone, Debug, Default)]
pub struct SitemapReport {
    pub files: Vec<SitemapFile>,
    pub urls: Vec<SitemapUrl>,
    /// The first guardrail or failure that made discovery incomplete.
    pub partial: Option<String>,
    /// Discovery finished, whether or not it found anything.
    pub discovery_done: bool,
    url_index: HashMap<String, usize>,
    file_index: HashMap<String, usize>,
}

impl SitemapReport {
    pub fn insert_file(&mut self, file: SitemapFile) {
        if let Some(&index) = self.file_index.get(&file.url) {
            let existing = &mut self.files[index];
            // A later terminal result replaces the placeholder; a duplicate Pending result
            // does not erase work already done.
            if existing.state == FileState::Pending || file.state != FileState::Pending {
                *existing = file;
            }
            return;
        }
        self.file_index.insert(file.url.clone(), self.files.len());
        self.files.push(file);
    }

    /// Record a listed URL. Returns true when this is the first membership.
    pub fn insert_url(&mut self, entry: SitemapUrl) -> bool {
        if let Some(&index) = self.url_index.get(&entry.url) {
            let existing = &mut self.urls[index];
            for source in entry.sources {
                if !existing.sources.iter().any(|seen| seen == &source) {
                    existing.sources.push(source);
                }
            }
            // Never replace checked evidence with a pending duplicate. A terminal state
            // from a later file is recorded only when nothing has been checked yet.
            if existing.state == UrlState::Pending && entry.state != UrlState::Pending {
                existing.state = entry.state;
                if existing.issue.is_empty() {
                    existing.issue = entry.issue;
                }
            }
            return false;
        }
        self.url_index.insert(entry.url.clone(), self.urls.len());
        self.urls.push(entry);
        true
    }

    pub fn url(&self, url: &str) -> Option<&SitemapUrl> {
        self.url_index.get(url).map(|index| &self.urls[*index])
    }

    pub fn url_mut(&mut self, url: &str) -> Option<&mut SitemapUrl> {
        self.url_index
            .get(url)
            .copied()
            .map(|index| &mut self.urls[index])
    }

    /// Attach the crawl's actual page evidence to a listed URL.
    pub fn observe_page(&mut self, page: &Page) {
        let Some(index) = self.url_index.get(&page.url).copied() else {
            return;
        };
        let entry = &mut self.urls[index];
        entry.state = UrlState::Checked;
        entry.status = page.status;
        entry.redirect_to = page.redirect_to.clone();
        entry.canonical = page.canonical.clone();
        entry.content_type = page.content_type.clone();
        entry.error = page.error.clone();
        *entry.assessment = page.indexability.clone();
        entry.issue = entry.assessment.reason_text();
    }

    /// Mark every URL the crawl never reached. `reason` is a fact, not a failure claim.
    pub fn mark_not_checked(&mut self, reason: &str) {
        for entry in &mut self.urls {
            if entry.state == UrlState::Pending {
                entry.state = UrlState::NotChecked;
                if entry.issue.is_empty() {
                    entry.issue = reason.to_string();
                }
            }
        }
    }

    pub fn note_partial(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if reason.is_empty() {
            return;
        }
        match &mut self.partial {
            Some(existing) if existing.contains(&reason) => {}
            Some(existing) => {
                existing.push_str(" · ");
                existing.push_str(&reason);
            }
            None => self.partial = Some(reason),
        }
    }

    pub fn summary(&self) -> SitemapSummary {
        let mut summary = SitemapSummary {
            files: self.files.len(),
            ..SitemapSummary::default()
        };
        for file in &self.files {
            summary.file_errors += (file.state == FileState::Error) as usize;
            summary.file_issues += file.needs_attention() as usize;
        }
        summary.listed = self.urls.len();
        for entry in &self.urls {
            match entry.state {
                UrlState::Pending => summary.pending += 1,
                UrlState::Checked => summary.checked += 1,
                UrlState::Blocked => summary.blocked += 1,
                UrlState::Skipped => summary.skipped += 1,
                UrlState::NotChecked => summary.not_checked += 1,
            }
            summary.issue_urls += entry.needs_attention() as usize;
        }
        summary
    }
}

/// Counts that reconcile with the per-URL states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SitemapSummary {
    pub listed: usize,
    pub checked: usize,
    pub blocked: usize,
    pub skipped: usize,
    pub not_checked: usize,
    pub pending: usize,
    pub files: usize,
    pub file_errors: usize,
    pub file_issues: usize,
    /// Listed URLs with a finding, blocked/skipped state, or an unfinished visit.
    pub issue_urls: usize,
}

/// The seed's scheme, host and effective port. Sitemap documents and listed page URLs
/// outside it are skipped without a request.
pub(crate) fn same_origin(seed: &Url, candidate: &Url) -> bool {
    if !matches!(candidate.scheme(), "http" | "https") {
        return false;
    }
    let host = |url: &Url| {
        url.host_str()
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
    };
    seed.scheme() == candidate.scheme()
        && host(seed).is_some_and(|seed_host| host(candidate).is_some_and(|host| host == seed_host))
        && seed.port_or_known_default() == candidate.port_or_known_default()
}

fn origin_of(seed: &Url) -> String {
    seed.origin().ascii_serialization()
}

/// The useful result of reading one XML document.
#[derive(Clone, Debug, Default)]
pub struct ParsedSitemap {
    pub kind: SitemapKind,
    /// `loc` text values, in document order. Validation happens during discovery.
    pub locs: Vec<String>,
    /// `url` or `sitemap` entries, including entries without a usable loc.
    pub entries: usize,
    /// A visible parse diagnostic. Some valid locations may still have been collected.
    pub error: Option<String>,
}

/// Parse one sitemap XML document with a streaming reader.
///
/// Only `loc` directly under a `url` (in a `urlset`) or under a `sitemap` (in a
/// `sitemapindex`) is collected. Foreign namespaces are ignored as whole subtrees, so an
/// image, video or hreflang extension cannot contribute page seeds.
pub fn parse_document(bytes: &[u8], cancel: &AtomicBool) -> ParsedSitemap {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut depth: usize = 0;
    let mut ignored_depth: Option<usize> = None;
    let mut root: Option<SitemapKind> = None;
    let mut entry_depth: Option<usize> = None;
    let mut loc_depth: Option<usize> = None;
    let mut text = String::new();
    let mut locs: Vec<String> = Vec::new();
    let mut entries: usize = 0;
    let mut error: Option<String> = None;
    let mut events: usize = 0;

    loop {
        events += 1;
        if events.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
            error = Some("Stopped".into());
            break;
        }
        let (namespace, event) = match reader.read_resolved_event_into(&mut buf) {
            Ok(event) => event,
            Err(cause) => {
                error = Some(format!("XML error: {cause}"));
                break;
            }
        };
        let accepted = matches!(namespace, ResolveResult::Unbound)
            || matches!(&namespace, ResolveResult::Bound(namespace) if namespace.as_ref() == SITEMAP_NS);
        match event {
            Event::Start(start) => {
                let name = start.local_name();
                let name = name.as_ref();
                if ignored_depth.is_some() {
                    depth += 1;
                    continue;
                }
                if !accepted {
                    ignored_depth = Some(depth);
                    depth += 1;
                    continue;
                }
                if root.is_none() {
                    if depth != 0 {
                        error = Some("Not a sitemap document".into());
                        break;
                    }
                    match name {
                        b"urlset" => root = Some(SitemapKind::Urlset),
                        b"sitemapindex" => root = Some(SitemapKind::Index),
                        _ => {
                            error = Some("Not a sitemap document".into());
                            break;
                        }
                    }
                }
                match root {
                    Some(SitemapKind::Urlset) if depth == 1 && name == b"url" => {
                        entries += 1;
                        if entries > MAX_ENTRIES_PER_DOC {
                            error = Some(format!(
                                "More than {MAX_ENTRIES_PER_DOC} entries in one sitemap"
                            ));
                            break;
                        }
                        entry_depth = Some(depth);
                    }
                    Some(SitemapKind::Index) if depth == 1 && name == b"sitemap" => {
                        entries += 1;
                        if entries > MAX_ENTRIES_PER_DOC {
                            error = Some(format!(
                                "More than {MAX_ENTRIES_PER_DOC} entries in one sitemap"
                            ));
                            break;
                        }
                        entry_depth = Some(depth);
                    }
                    Some(_)
                        if name == b"loc"
                            && entry_depth.is_some_and(|entry| depth == entry + 1) =>
                    {
                        loc_depth = Some(depth);
                        text.clear();
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::Empty(start) => {
                let name = start.local_name();
                let name = name.as_ref();
                if ignored_depth.is_some() || !accepted {
                    continue;
                }
                if root.is_none() {
                    if depth != 0 {
                        error = Some("Not a sitemap document".into());
                        break;
                    }
                    match name {
                        b"urlset" => root = Some(SitemapKind::Urlset),
                        b"sitemapindex" => root = Some(SitemapKind::Index),
                        _ => {
                            error = Some("Not a sitemap document".into());
                            break;
                        }
                    }
                }
                match root {
                    Some(SitemapKind::Urlset) if depth == 1 && name == b"url" => {
                        entries += 1;
                        if entries > MAX_ENTRIES_PER_DOC {
                            error = Some(format!(
                                "More than {MAX_ENTRIES_PER_DOC} entries in one sitemap"
                            ));
                            break;
                        }
                    }
                    Some(SitemapKind::Index) if depth == 1 && name == b"sitemap" => {
                        entries += 1;
                        if entries > MAX_ENTRIES_PER_DOC {
                            error = Some(format!(
                                "More than {MAX_ENTRIES_PER_DOC} entries in one sitemap"
                            ));
                            break;
                        }
                    }
                    Some(_)
                        if name == b"loc"
                            && entry_depth.is_some_and(|entry| depth == entry + 1) =>
                    {
                        // An empty loc is not a URL. The entry still counts.
                        text.clear();
                    }
                    _ => {}
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if ignored_depth == Some(depth) {
                    ignored_depth = None;
                }
                if loc_depth == Some(depth) {
                    let loc = text.trim().to_string();
                    if !loc.is_empty() {
                        locs.push(loc);
                    }
                    text.clear();
                    loc_depth = None;
                }
                if entry_depth == Some(depth) {
                    entry_depth = None;
                }
            }
            Event::Text(value) => {
                if loc_depth.is_some_and(|loc| depth == loc + 1) {
                    match value.xml10_content() {
                        Ok(content) => text.push_str(&content),
                        Err(cause) => {
                            error = Some(format!("XML text error: {cause}"));
                            break;
                        }
                    }
                }
            }
            Event::CData(value) => {
                if loc_depth.is_some_and(|loc| depth == loc + 1) {
                    match value.xml10_content() {
                        Ok(content) => text.push_str(&content),
                        Err(cause) => {
                            error = Some(format!("XML text error: {cause}"));
                            break;
                        }
                    }
                }
            }
            Event::GeneralRef(reference) => {
                if loc_depth.is_some_and(|loc| depth == loc + 1) {
                    let raw = reference
                        .decode()
                        .map(|value| value.into_owned())
                        .unwrap_or_default();
                    match reference.resolve_char_ref() {
                        Ok(Some(character)) => text.push(character),
                        Ok(None) => match resolve_predefined_entity(&raw) {
                            Some(value) => text.push_str(value),
                            None => {
                                error = Some(format!("Unknown entity &{raw};"));
                                break;
                            }
                        },
                        Err(cause) => {
                            error = Some(format!("Bad character reference: {cause}"));
                            break;
                        }
                    }
                }
            }
            Event::DocType(_) => {
                error = Some("Sitemap DTDs are not supported".into());
                break;
            }
            Event::Eof => break,
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
        }
        buf.clear();
    }

    if error.is_none() && depth > 0 {
        error = Some("XML error: unexpected end of document".into());
    }
    if error.is_none() && root.is_none() {
        error = Some("Not a sitemap document".into());
    }
    ParsedSitemap {
        kind: root.unwrap_or(SitemapKind::Unknown),
        locs,
        entries,
        error,
    }
}

/// Inflate or accept one sitemap response body. Gzip is detected by its magic bytes so a
/// body already decoded by the HTTP transport is never decompressed twice.
pub fn decode_body(fetched: &FetchedBytes, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    if fetched.status == 0 {
        return Err(if fetched.error.is_empty() {
            "Could not fetch sitemap".to_string()
        } else {
            fetched.error.clone()
        });
    }
    if !(200..300).contains(&fetched.status) {
        return Err(format!("HTTP {}", fetched.status));
    }
    if !fetched.error.is_empty() {
        return Err(fetched.error.clone());
    }
    if fetched.body.len() as u64 > MAX_COMPRESSED_BYTES {
        return Err("Sitemap download is larger than 16 MiB".into());
    }
    let gzip = fetched.body.starts_with(&[0x1f, 0x8b]);
    let mut output = Vec::with_capacity(if gzip {
        0
    } else {
        fetched.body.len().min(MAX_DOC_BYTES as usize)
    });
    if gzip {
        let mut decoder = GzDecoder::new(fetched.body.as_slice());
        let mut chunk = [0_u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Stopped".into());
            }
            let read = decoder
                .read(&mut chunk)
                .map_err(|error| format!("Could not decompress sitemap: {error}"))?;
            if read == 0 {
                break;
            }
            output.extend_from_slice(&chunk[..read]);
            if output.len() as u64 > MAX_DOC_BYTES {
                return Err("Sitemap is larger than 50 MiB uncompressed".into());
            }
        }
    } else {
        if fetched.body.len() as u64 > MAX_DOC_BYTES {
            return Err("Sitemap is larger than 50 MiB uncompressed".into());
        }
        output.extend_from_slice(&fetched.body);
    }
    Ok(output)
}

/// Discovery updates streamed to the crawl loop and the window.
#[derive(Clone, Debug)]
pub enum DiscoveryEvent {
    File(SitemapFile),
    Url(SitemapUrl),
    Partial(String),
    Done,
}

struct Sink<'a> {
    report: SitemapReport,
    on_event: &'a mut dyn FnMut(DiscoveryEvent),
}

impl Sink<'_> {
    fn file(&mut self, file: SitemapFile) {
        self.report.insert_file(file.clone());
        (self.on_event)(DiscoveryEvent::File(file));
    }

    fn url(&mut self, entry: SitemapUrl) {
        self.report.insert_url(entry.clone());
        (self.on_event)(DiscoveryEvent::Url(entry));
    }

    fn partial(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.report.note_partial(reason.clone());
        (self.on_event)(DiscoveryEvent::Partial(reason));
    }
}

/// Discover and read the seed origin's sitemaps.
///
/// `declared` is the deduplicated list of `Sitemap:` lines from robots.txt. `allows`
/// checks robots restrictions for every document hop and listed URL. Nothing outside the
/// seed origin is requested. The callback observes the same events that build the return
/// value, so callers can stream discovery into a UI.
pub fn discover(
    seed: &Url,
    declared: Vec<String>,
    allows: &(dyn Fn(&Url) -> bool + Sync),
    fetch: &(dyn Fn(&str) -> FetchedBytes + Sync),
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(DiscoveryEvent),
) -> SitemapReport {
    let mut sink = Sink {
        report: SitemapReport::default(),
        on_event,
    };
    let mut queue: VecDeque<(String, u8, SitemapOrigin)> = VecDeque::new();
    let mut known: HashSet<String> = HashSet::new();
    let mut declared_in_scope = false;

    for raw in declared {
        let raw = raw.trim();
        match Url::parse(raw) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                let key = url_key(url);
                if Url::parse(&key).is_ok_and(|url| same_origin(seed, &url)) {
                    declared_in_scope = true;
                }
                if known.insert(key.clone()) {
                    queue.push_back((key, 0, SitemapOrigin::Robots));
                }
            }
            _ => sink.file(SitemapFile {
                url: raw.to_string(),
                origin: SitemapOrigin::Robots,
                state: FileState::Skipped,
                issue: "Invalid sitemap URL in robots.txt".into(),
                ..SitemapFile::default()
            }),
        }
    }

    // Guessed roots are tried only when robots.txt gave us no usable in-scope
    // declaration. A 404 here is absence, not an error.
    if !declared_in_scope {
        let origin = origin_of(seed);
        for path in ["/sitemap.xml", "/sitemap_index.xml"] {
            let url = format!("{origin}{path}");
            if known.insert(url.clone()) {
                queue.push_back((url, 0, SitemapOrigin::Guessed));
            }
        }
    }

    let mut seen_docs: HashSet<String> = HashSet::new();
    let mut queued_docs: HashSet<String> = queue.iter().map(|(url, _, _)| url.clone()).collect();
    let mut documents = 0_usize;
    let mut total_entries = 0_usize;
    let mut guessed_settled = false;

    while let Some((document_url, depth, origin)) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            sink.partial("Stopped");
            break;
        }
        if origin == SitemapOrigin::Guessed && guessed_settled {
            continue;
        }
        let Ok(document) = Url::parse(&document_url) else {
            sink.file(SitemapFile {
                url: document_url,
                origin,
                depth,
                state: FileState::Skipped,
                issue: "Invalid sitemap URL".into(),
                ..SitemapFile::default()
            });
            continue;
        };
        if !same_origin(seed, &document) {
            sink.file(SitemapFile {
                url: document_url,
                origin,
                depth,
                state: FileState::Skipped,
                issue: "Outside this site".into(),
                ..SitemapFile::default()
            });
            if origin == SitemapOrigin::Guessed {
                guessed_settled = true;
            }
            continue;
        }
        if !allows(&document) {
            sink.file(SitemapFile {
                url: document_url,
                origin,
                depth,
                state: FileState::Skipped,
                issue: "Blocked by robots.txt".into(),
                ..SitemapFile::default()
            });
            if origin == SitemapOrigin::Guessed {
                guessed_settled = true;
            }
            continue;
        }
        if !seen_docs.insert(document_url.clone()) {
            continue;
        }
        if documents >= MAX_DOCUMENTS {
            sink.partial(format!("More than {MAX_DOCUMENTS} sitemap files"));
            break;
        }
        documents += 1;

        let fetched = match fetch_document(&document_url, seed, allows, fetch, cancel) {
            Ok(fetched) => fetched,
            Err((state, status, issue)) => {
                if origin == SitemapOrigin::Guessed {
                    if state == FileState::Absent {
                        continue;
                    }
                    guessed_settled = true;
                }
                let state = if state == FileState::Absent {
                    FileState::Error
                } else {
                    state
                };
                let issue = if state == FileState::Error && status == 404 {
                    "Not found (404)".to_string()
                } else {
                    issue
                };
                if state != FileState::Skipped {
                    sink.partial(format!("{}: {issue}", short_url(&document_url)));
                }
                sink.file(SitemapFile {
                    url: document_url,
                    origin,
                    depth,
                    state,
                    status,
                    issue,
                    ..SitemapFile::default()
                });
                continue;
            }
        };

        if origin == SitemapOrigin::Guessed {
            guessed_settled = true;
        }

        let bytes = match decode_body(&fetched, cancel) {
            Ok(bytes) => bytes,
            Err(issue) => {
                if issue == "Stopped" {
                    sink.partial("Stopped");
                } else {
                    sink.partial(format!("{}: {issue}", short_url(&document_url)));
                }
                sink.file(SitemapFile {
                    url: document_url,
                    origin,
                    depth,
                    state: if issue == "Stopped" {
                        FileState::Skipped
                    } else {
                        FileState::Error
                    },
                    status: fetched.status,
                    issue,
                    ..SitemapFile::default()
                });
                continue;
            }
        };
        let parsed = parse_document(&bytes, cancel);
        if parsed.error.is_none() && parsed.entries == 0 && parsed.locs.is_empty() {
            // An empty but well-formed document is still a read file.
        }
        let mut file = SitemapFile {
            url: document_url.clone(),
            origin,
            depth,
            state: if parsed.error.is_some() {
                FileState::Error
            } else {
                FileState::Ok
            },
            status: fetched.status,
            kind: parsed.kind,
            entries: parsed.entries,
            urls: 0,
            issue: parsed.error.clone().unwrap_or_default(),
        };
        if let Some(error) = &parsed.error {
            sink.partial(format!("{}: {error}", short_url(&document_url)));
        }
        // Count every structural entry, including duplicates and entries with no usable
        // `<loc>`, against the crawl-wide parser budget.
        let remaining_entries = MAX_TOTAL_URLS.saturating_sub(total_entries);
        let entries_to_count = parsed.entries.min(remaining_entries);
        let locations_to_process = parsed.locs.len().min(remaining_entries);
        total_entries += entries_to_count;
        if parsed.entries > remaining_entries || parsed.locs.len() > remaining_entries {
            sink.partial(format!("More than {MAX_TOTAL_URLS} sitemap URL entries"));
        }
        // Insert the document before its children so the file list follows discovery order.
        // The final event below replaces this placeholder with the counted totals.
        sink.file(file.clone());

        match parsed.kind {
            SitemapKind::Urlset => {
                for loc in parsed.locs.into_iter().take(locations_to_process) {
                    match Url::parse(loc.trim()) {
                        Ok(url) if matches!(url.scheme(), "http" | "https") => {
                            file.urls += 1;
                            let key = url_key(url);
                            let (state, issue) = if let Ok(url) = Url::parse(&key) {
                                if !same_origin(seed, &url) {
                                    (UrlState::Skipped, "Outside this site".to_string())
                                } else if !allows(&url) {
                                    (UrlState::Blocked, "Blocked by robots.txt".to_string())
                                } else {
                                    (UrlState::Pending, String::new())
                                }
                            } else {
                                (UrlState::Skipped, "Invalid URL".to_string())
                            };
                            sink.url(SitemapUrl {
                                url: key,
                                sources: vec![document_url.clone()],
                                state,
                                issue,
                                ..SitemapUrl::default()
                            });
                        }
                        _ => sink.url(SitemapUrl {
                            url: loc.trim().to_string(),
                            sources: vec![document_url.clone()],
                            state: UrlState::Skipped,
                            issue: "Invalid URL".to_string(),
                            ..SitemapUrl::default()
                        }),
                    }
                }
            }
            SitemapKind::Index => {
                for loc in parsed.locs.into_iter().take(locations_to_process) {
                    let Ok(url) = Url::parse(loc.trim()) else {
                        sink.file(SitemapFile {
                            url: loc.trim().to_string(),
                            origin: SitemapOrigin::Index,
                            depth: depth.saturating_add(1),
                            state: FileState::Skipped,
                            issue: "Invalid sitemap URL".into(),
                            ..SitemapFile::default()
                        });
                        continue;
                    };
                    if !matches!(url.scheme(), "http" | "https") {
                        sink.file(SitemapFile {
                            url: loc.trim().to_string(),
                            origin: SitemapOrigin::Index,
                            depth: depth.saturating_add(1),
                            state: FileState::Skipped,
                            issue: "Invalid sitemap URL".into(),
                            ..SitemapFile::default()
                        });
                        continue;
                    }
                    let key = url_key(url);
                    let child = match Url::parse(&key) {
                        Ok(url) => url,
                        Err(_) => continue,
                    };
                    file.urls += 1;
                    if !same_origin(seed, &child) {
                        sink.file(SitemapFile {
                            url: key,
                            origin: SitemapOrigin::Index,
                            depth: depth.saturating_add(1),
                            state: FileState::Skipped,
                            issue: "Outside this site".into(),
                            ..SitemapFile::default()
                        });
                    } else if !allows(&child) {
                        sink.file(SitemapFile {
                            url: key,
                            origin: SitemapOrigin::Index,
                            depth: depth.saturating_add(1),
                            state: FileState::Skipped,
                            issue: "Blocked by robots.txt".into(),
                            ..SitemapFile::default()
                        });
                    } else if depth >= MAX_INDEX_DEPTH {
                        sink.file(SitemapFile {
                            url: key,
                            origin: SitemapOrigin::Index,
                            depth: depth.saturating_add(1),
                            state: FileState::Skipped,
                            issue: "Index nesting limit".into(),
                            ..SitemapFile::default()
                        });
                        sink.partial(format!("More than {MAX_INDEX_DEPTH} sitemap index levels"));
                    } else if !queued_docs.contains(&key) && !seen_docs.contains(&key) {
                        queued_docs.insert(key.clone());
                        queue.push_back((key, depth.saturating_add(1), SitemapOrigin::Index));
                    }
                }
            }
            SitemapKind::Unknown => {}
        }

        sink.file(file);
    }

    sink.report.discovery_done = true;
    (sink.on_event)(DiscoveryEvent::Done);
    sink.report
}

fn short_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(url) => {
            let path = url.path();
            if path.is_empty() || path == "/" {
                url.host_str().unwrap_or(url.as_str()).to_string()
            } else {
                path.to_string()
            }
        }
        Err(_) => url.to_string(),
    }
}

fn resolve(base: &Url, location: &str) -> Option<Url> {
    let location = location.trim();
    if location.is_empty() {
        return None;
    }
    let mut url = base.join(location).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

/// Fetch one sitemap document, following at most five redirects and applying scope and
/// robots checks again on every hop. A 404/410 is returned as `Absent` so the caller can
/// distinguish a guessed path from a declared one.
#[allow(clippy::type_complexity)]
fn fetch_document(
    url: &str,
    seed: &Url,
    allows: &(dyn Fn(&Url) -> bool + Sync),
    fetch: &(dyn Fn(&str) -> FetchedBytes + Sync),
    cancel: &AtomicBool,
) -> Result<FetchedBytes, (FileState, u16, String)> {
    let mut current =
        Url::parse(url).map_err(|_| (FileState::Skipped, 0, "Invalid sitemap URL".to_string()))?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut hops = 0_u8;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err((FileState::Skipped, 0, "Stopped".into()));
        }
        if !same_origin(seed, &current) {
            return Err((FileState::Skipped, 0, "Outside this site".into()));
        }
        if !allows(&current) {
            return Err((FileState::Skipped, 0, "Blocked by robots.txt".into()));
        }
        if !seen.insert(current.as_str().to_string()) {
            return Err((FileState::Error, 0, "Sitemap redirect loop".into()));
        }
        let fetched = fetch(current.as_str());
        if fetched.status == 0 {
            return Err((
                FileState::Error,
                0,
                if fetched.error.is_empty() {
                    "Could not fetch sitemap".to_string()
                } else {
                    fetched.error
                },
            ));
        }
        if (300..400).contains(&fetched.status) {
            if hops >= MAX_REDIRECTS {
                return Err((
                    FileState::Error,
                    fetched.status,
                    "Too many sitemap redirects".into(),
                ));
            }
            let Some(next) = resolve(&current, &fetched.location) else {
                return Err((
                    FileState::Error,
                    fetched.status,
                    "Sitemap redirect has no target".into(),
                ));
            };
            current = next;
            hops += 1;
            continue;
        }
        if fetched.status == 404 || fetched.status == 410 {
            return Err((FileState::Absent, fetched.status, "Not found".to_string()));
        }
        if !(200..300).contains(&fetched.status) {
            return Err((
                FileState::Error,
                fetched.status,
                format!("HTTP {}", fetched.status),
            ));
        }
        return Ok(fetched);
    }
}

/// A deterministic mixed outcome set for the UI sample scene. It covers a read index and
/// urlset, file and URL errors, blocked and out-of-scope entries, and a visit the page limit
/// stopped, so the Sitemaps tab can be reviewed without a network crawl.
pub fn sample_report() -> SitemapReport {
    let mut report = SitemapReport {
        discovery_done: true,
        partial: Some("More than 100,000 sitemap URL entries".into()),
        ..SitemapReport::default()
    };
    for file in [
        SitemapFile {
            url: "https://example.com/sitemap.xml".into(),
            origin: SitemapOrigin::Robots,
            depth: 0,
            state: FileState::Ok,
            status: 200,
            kind: SitemapKind::Index,
            entries: 2,
            urls: 2,
            issue: String::new(),
        },
        SitemapFile {
            url: "https://example.com/sitemap-pages.xml".into(),
            origin: SitemapOrigin::Index,
            depth: 1,
            state: FileState::Ok,
            status: 200,
            kind: SitemapKind::Urlset,
            entries: 5,
            urls: 5,
            issue: String::new(),
        },
        SitemapFile {
            url: "https://example.com/blog/sitemap.xml".into(),
            origin: SitemapOrigin::Index,
            depth: 1,
            state: FileState::Error,
            status: 404,
            kind: SitemapKind::Unknown,
            entries: 0,
            urls: 0,
            issue: "Not found (404)".into(),
        },
        SitemapFile {
            url: "https://example.com/sitemap-old.xml".into(),
            origin: SitemapOrigin::Robots,
            depth: 0,
            state: FileState::Error,
            status: 200,
            kind: SitemapKind::Unknown,
            entries: 0,
            urls: 0,
            issue: "Not a sitemap document".into(),
        },
        SitemapFile {
            url: "https://example.com/sitemap-images.xml".into(),
            origin: SitemapOrigin::Robots,
            depth: 0,
            state: FileState::Skipped,
            status: 0,
            kind: SitemapKind::Unknown,
            entries: 0,
            urls: 0,
            issue: "Blocked by robots.txt".into(),
        },
        SitemapFile {
            url: "https://example.com/sitemap-news.xml".into(),
            origin: SitemapOrigin::Robots,
            depth: 0,
            state: FileState::Error,
            status: 200,
            kind: SitemapKind::Urlset,
            entries: 0,
            urls: 0,
            issue: "Sitemap is larger than 50 MiB uncompressed".into(),
        },
        SitemapFile {
            url: "https://example.com/sitemap_index.xml".into(),
            origin: SitemapOrigin::Guessed,
            depth: 0,
            state: FileState::Absent,
            status: 404,
            kind: SitemapKind::Unknown,
            entries: 0,
            urls: 0,
            issue: "Not found".into(),
        },
    ] {
        report.insert_file(file);
    }
    let assessed = |url: &str,
                    status: u16,
                    content_type: &str,
                    redirect_to: &str,
                    noindex: bool,
                    canonical: Option<&str>| {
        let mut evidence = PageEvidence {
            // Only a successful HTML response has a body this crawl reads.
            read: if status == 200 && content_type.to_ascii_lowercase().contains("html") {
                ReadState::Complete
            } else {
                ReadState::NotRead
            },
            ..PageEvidence::default()
        };
        if noindex {
            evidence.directives.push(RobotsDirective {
                source: EvidenceSource::Meta,
                agent: None,
                raw: "noindex".into(),
                noindex: true,
                nofollow: false,
                truncated: false,
            });
        }
        if let Some(canonical) = canonical
            && let Ok(base) = Url::parse(url)
        {
            evidence
                .canonicals
                .push(indexability::canonical_declaration(Some(canonical), &base));
        }
        indexability::assess(url, status, content_type, redirect_to, "", evidence)
    };
    for entry in [
        SitemapUrl {
            url: "https://example.com/".into(),
            sources: vec![
                "https://example.com/sitemap.xml".into(),
                "https://example.com/sitemap-pages.xml".into(),
            ],
            state: UrlState::Checked,
            status: 200,
            content_type: "text/html; charset=utf-8".into(),
            assessment: Box::new(assessed(
                "https://example.com/",
                200,
                "text/html",
                "",
                false,
                None,
            )),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "https://example.com/guide/".into(),
            sources: vec!["https://example.com/sitemap-pages.xml".into()],
            state: UrlState::Checked,
            status: 200,
            canonical: "https://example.com/guide".into(),
            content_type: "text/html; charset=utf-8".into(),
            issue: "Canonical points to https://example.com/guide".into(),
            assessment: Box::new(assessed(
                "https://example.com/guide/",
                200,
                "text/html",
                "",
                false,
                Some("https://example.com/guide"),
            )),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "https://example.com/pricing".into(),
            sources: vec!["https://example.com/sitemap-pages.xml".into()],
            state: UrlState::Checked,
            status: 404,
            content_type: "text/html; charset=utf-8".into(),
            issue: "404 Client error".into(),
            assessment: Box::new(assessed(
                "https://example.com/pricing",
                404,
                "text/html",
                "",
                false,
                None,
            )),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "https://example.com/old-page".into(),
            sources: vec!["https://example.com/sitemap-pages.xml".into()],
            state: UrlState::Checked,
            status: 301,
            redirect_to: "https://example.com/new-page".into(),
            content_type: "text/html".into(),
            issue: "Redirect to https://example.com/new-page".into(),
            assessment: Box::new(assessed(
                "https://example.com/old-page",
                301,
                "text/html",
                "https://example.com/new-page",
                false,
                None,
            )),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "https://example.com/draft".into(),
            sources: vec!["https://example.com/sitemap-pages.xml".into()],
            state: UrlState::Checked,
            status: 200,
            content_type: "text/html; charset=utf-8".into(),
            issue: "Noindex".into(),
            assessment: Box::new(assessed(
                "https://example.com/draft",
                200,
                "text/html",
                "",
                true,
                None,
            )),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "https://example.com/report.pdf".into(),
            sources: vec!["https://example.com/sitemap-pages.xml".into()],
            state: UrlState::Checked,
            status: 200,
            content_type: "application/pdf".into(),
            issue: "Not HTML".into(),
            assessment: Box::new(assessed(
                "https://example.com/report.pdf",
                200,
                "application/pdf",
                "",
                false,
                None,
            )),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "https://example.com/private".into(),
            sources: vec!["https://example.com/sitemap.xml".into()],
            state: UrlState::Blocked,
            issue: "Blocked by robots.txt".into(),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "http://example.com/legacy".into(),
            sources: vec!["https://example.com/sitemap.xml".into()],
            state: UrlState::Skipped,
            issue: "Outside this site".into(),
            ..SitemapUrl::default()
        },
        SitemapUrl {
            url: "https://example.com/late-page".into(),
            sources: vec!["https://example.com/sitemap.xml".into()],
            state: UrlState::NotChecked,
            issue: "Page limit".into(),
            ..SitemapUrl::default()
        },
    ] {
        report.insert_url(entry);
    }
    for entry in &mut report.urls {
        if entry.state == UrlState::Checked {
            entry.issue = entry.assessment.reason_text();
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;

    const URLSET_HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#;

    fn doc(body: &str) -> FetchedBytes {
        FetchedBytes {
            status: 200,
            content_type: "application/xml".into(),
            body: body.as_bytes().to_vec(),
            ..FetchedBytes::default()
        }
    }

    fn missing(status: u16) -> FetchedBytes {
        FetchedBytes {
            status,
            ..FetchedBytes::default()
        }
    }

    fn gzip(body: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(body).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn parses_a_urlset_in_the_standard_namespace_and_ignores_foreign_locs() {
        let xml = format!(
            "{URLSET_HEAD}
  <url><loc>https://example.com/a</loc>
    <image:image xmlns:image=\"http://www.google.com/schemas/sitemap-image/1.1\"><image:loc>https://example.com/a.png</image:loc></image:image>
  </url>
  <url><loc> https://example.com/b?x=1&amp;y=2 </loc></url>
</urlset>"
        );
        let parsed = parse_document(xml.as_bytes(), &AtomicBool::new(false));
        assert_eq!(parsed.kind, SitemapKind::Urlset);
        assert_eq!(parsed.entries, 2);
        assert_eq!(
            parsed.locs,
            vec![
                "https://example.com/a".to_string(),
                "https://example.com/b?x=1&y=2".to_string()
            ]
        );
        assert!(parsed.error.is_none());
    }

    #[test]
    fn accepts_a_prefixed_standard_namespace_and_a_namespace_free_document() {
        let prefixed = r#"<?xml version="1.0"?>
<sm:urlset xmlns:sm="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sm:url><sm:loc>https://example.com/one</sm:loc></sm:url>
</sm:urlset>"#;
        let parsed = parse_document(prefixed.as_bytes(), &AtomicBool::new(false));
        assert_eq!(parsed.kind, SitemapKind::Urlset);
        assert_eq!(parsed.locs, vec!["https://example.com/one"]);

        let plain = "<urlset><url><loc>https://example.com/two</loc></url></urlset>";
        let parsed = parse_document(plain.as_bytes(), &AtomicBool::new(false));
        assert_eq!(parsed.kind, SitemapKind::Urlset);
        assert_eq!(parsed.locs, vec!["https://example.com/two"]);
    }

    #[test]
    fn parses_an_index_and_counts_entries_without_a_usable_loc() {
        let xml = r#"<?xml version="1.0"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/one.xml</loc></sitemap>
  <sitemap><loc> </loc></sitemap>
</sitemapindex>"#;
        let parsed = parse_document(xml.as_bytes(), &AtomicBool::new(false));
        assert_eq!(parsed.kind, SitemapKind::Index);
        assert_eq!(parsed.entries, 2);
        assert_eq!(parsed.locs, vec!["https://example.com/one.xml"]);
        assert!(parsed.error.is_none());
    }

    #[test]
    fn rejects_malformed_documents_dtds_and_unrelated_xml() {
        for (xml, expected) in [
            ("<urlset><url>", "XML error"),
            (
                "<!DOCTYPE urlset [<!ENTITY x \"y\">]><urlset></urlset>",
                "DTD",
            ),
            ("<rss><channel></channel></rss>", "Not a sitemap"),
            ("", "Not a sitemap"),
        ] {
            let parsed = parse_document(xml.as_bytes(), &AtomicBool::new(false));
            let error = parsed.error.unwrap_or_default();
            assert!(error.contains(expected), "{xml:?} => {error}");
        }
    }

    #[test]
    fn only_root_level_entries_are_read_from_a_sitemap() {
        let xml = format!(
            "{URLSET_HEAD}<wrapper><url><loc>https://example.com/nested</loc></url></wrapper></urlset>"
        );
        let parsed = parse_document(xml.as_bytes(), &AtomicBool::new(false));
        assert_eq!(parsed.kind, SitemapKind::Urlset);
        assert_eq!(parsed.entries, 0);
        assert!(parsed.locs.is_empty());
        assert!(parsed.error.is_none());
    }

    #[test]
    fn rejects_an_unknown_entity_instead_of_expanding_it() {
        let xml =
            format!("{URLSET_HEAD}<url><loc>https://example.com/&bogus;</loc></url></urlset>");
        let parsed = parse_document(xml.as_bytes(), &AtomicBool::new(false));
        assert!(parsed.error.unwrap_or_default().contains("Unknown entity"));
    }

    #[test]
    fn gzip_bodies_are_inflated_and_the_uncompressed_cap_is_enforced() {
        let xml = format!("{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>");
        let mut fetched = doc("");
        fetched.body = gzip(xml.as_bytes());
        let bytes = decode_body(&fetched, &AtomicBool::new(false)).unwrap();
        assert_eq!(bytes, xml.as_bytes());

        // ureq decodes Content-Encoding: gzip before the body reaches this seam. Plain XML
        // must pass through untouched rather than being inflated twice.
        let plain = doc(&xml);
        assert_eq!(
            decode_body(&plain, &AtomicBool::new(false)).unwrap(),
            xml.as_bytes()
        );

        let mut huge = doc("");
        huge.body = gzip(&vec![b' '; MAX_DOC_BYTES as usize + 1]);
        let error = decode_body(&huge, &AtomicBool::new(false)).unwrap_err();
        assert!(error.contains("50 MiB"), "{error}");
    }

    #[test]
    fn transport_download_errors_are_preserved_instead_of_becoming_xml_errors() {
        let fetched = FetchedBytes {
            status: 200,
            error: "Sitemap download is larger than 16 MiB".into(),
            ..FetchedBytes::default()
        };
        assert_eq!(
            decode_body(&fetched, &AtomicBool::new(false)).unwrap_err(),
            "Sitemap download is larger than 16 MiB"
        );
    }

    #[test]
    fn a_utf8_bom_is_accepted() {
        let xml =
            format!("\u{feff}{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>");
        let parsed = parse_document(xml.as_bytes(), &AtomicBool::new(false));
        assert_eq!(parsed.kind, SitemapKind::Urlset);
        assert_eq!(parsed.locs, vec!["https://example.com/a"]);
        assert!(parsed.error.is_none(), "{:?}", parsed.error);
    }

    fn discover_with(declared: Vec<String>, map: HashMap<String, FetchedBytes>) -> SitemapReport {
        discover_with_allows(declared, map, |_| true)
    }

    fn discover_with_allows(
        declared: Vec<String>,
        map: HashMap<String, FetchedBytes>,
        allow: impl Fn(&Url) -> bool + Sync,
    ) -> SitemapReport {
        let seed = Url::parse("https://example.com/").unwrap();
        let fetch = |url: &str| map.get(url).cloned().unwrap_or_else(|| missing(404));
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let report = discover(&seed, declared, &allow, &fetch, &cancel, &mut |event| {
            events.push(event)
        });
        assert!(report.discovery_done);
        assert!(matches!(events.last(), Some(DiscoveryEvent::Done)));
        report
    }

    #[test]
    fn robots_declarations_are_used_without_guessing_roots() {
        let mut map = HashMap::new();
        map.insert(
            "https://example.com/map.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url><url><loc>https://example.com/b</loc></url></urlset>"
            )),
        );
        let report = discover_with(vec!["https://example.com/map.xml".into()], map);
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].origin, SitemapOrigin::Robots);
        assert_eq!(report.files[0].state, FileState::Ok);
        assert_eq!(report.urls.len(), 2);
        assert!(
            report
                .urls
                .iter()
                .all(|entry| entry.state == UrlState::Pending)
        );
        assert!(
            !report
                .files
                .iter()
                .any(|file| file.url.ends_with("/sitemap.xml"))
        );
    }

    #[test]
    fn guessed_roots_are_absent_when_missing_and_stop_after_the_first_hit() {
        let mut both_missing = HashMap::new();
        both_missing.insert("https://example.com/sitemap.xml".to_string(), missing(404));
        both_missing.insert(
            "https://example.com/sitemap_index.xml".to_string(),
            missing(404),
        );
        let report = discover_with(Vec::new(), both_missing);
        assert!(report.files.is_empty(), "{:?}", report.files);

        let second_hit = HashMap::from([
            ("https://example.com/sitemap.xml".to_string(), missing(404)),
            (
                "https://example.com/sitemap_index.xml".to_string(),
                doc(&format!(
                    "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
                )),
            ),
        ]);
        let report = discover_with(Vec::new(), second_hit);
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].origin, SitemapOrigin::Guessed);
        assert_eq!(report.urls.len(), 1);
    }

    #[test]
    fn a_declared_missing_sitemap_is_an_error_while_an_index_child_keeps_its_source() {
        let mut map = HashMap::new();
        map.insert("https://example.com/sitemap.xml".to_string(), missing(404));
        let report = discover_with(vec!["https://example.com/sitemap.xml".into()], map.clone());
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].state, FileState::Error);
        assert_eq!(report.files[0].issue, "Not found (404)");

        map.insert(
            "https://example.com/root.xml".to_string(),
            doc(
                r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                <sitemap><loc>https://example.com/child.xml</loc></sitemap>
                <sitemap><loc>https://example.com/gone.xml</loc></sitemap>
            </sitemapindex>"#,
            ),
        );
        map.insert(
            "https://example.com/child.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
            )),
        );
        let report = discover_with(vec!["https://example.com/root.xml".into()], map);
        let child = report
            .urls
            .iter()
            .find(|entry| entry.url == "https://example.com/a")
            .unwrap();
        assert_eq!(child.sources, vec!["https://example.com/child.xml"]);
        let gone = report
            .files
            .iter()
            .find(|file| file.url == "https://example.com/gone.xml")
            .unwrap();
        assert_eq!(gone.origin, SitemapOrigin::Index);
        assert_eq!(gone.depth, 1);
        assert_eq!(gone.state, FileState::Error);
        assert_eq!(gone.issue, "Not found (404)");
        assert!(report.partial.is_some());
    }

    #[test]
    fn redirects_are_followed_and_cycles_are_reported() {
        let mut map = HashMap::new();
        map.insert(
            "https://example.com/old.xml".to_string(),
            FetchedBytes {
                status: 301,
                location: "/new.xml".into(),
                ..FetchedBytes::default()
            },
        );
        map.insert(
            "https://example.com/new.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
            )),
        );
        let report = discover_with(vec!["https://example.com/old.xml".into()], map);
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].state, FileState::Ok);
        assert_eq!(report.files[0].status, 200);

        let mut loop_map = HashMap::new();
        loop_map.insert(
            "https://example.com/one.xml".to_string(),
            FetchedBytes {
                status: 302,
                location: "/two.xml".into(),
                ..FetchedBytes::default()
            },
        );
        loop_map.insert(
            "https://example.com/two.xml".to_string(),
            FetchedBytes {
                status: 302,
                location: "/one.xml".into(),
                ..FetchedBytes::default()
            },
        );
        let report = discover_with(vec!["https://example.com/one.xml".into()], loop_map);
        assert_eq!(report.files[0].state, FileState::Error);
        assert!(report.files[0].issue.contains("redirect loop"));
    }

    #[test]
    fn scope_and_robots_are_checked_for_documents_and_listed_urls() {
        let mut map = HashMap::new();
        map.insert(
            "https://example.com/sitemap.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}
                    <url><loc>https://example.com/a</loc></url>
                    <url><loc>https://example.com/private/b</loc></url>
                    <url><loc>https://other.test/c</loc></url>
                    <url><loc>not a url</loc></url>
                </urlset>"
            )),
        );
        let report =
            discover_with_allows(Vec::new(), map, |url| !url.path().starts_with("/private"));
        let state = |url: &str| {
            report
                .urls
                .iter()
                .find(|entry| entry.url == url)
                .map(|entry| (entry.state, entry.issue.as_str()))
                .unwrap()
        };
        assert_eq!(state("https://example.com/a"), (UrlState::Pending, ""));
        assert_eq!(
            state("https://example.com/private/b"),
            (UrlState::Blocked, "Blocked by robots.txt")
        );
        assert_eq!(
            state("https://other.test/c"),
            (UrlState::Skipped, "Outside this site")
        );
        assert_eq!(state("not a url"), (UrlState::Skipped, "Invalid URL"));

        let mut blocked_doc = HashMap::new();
        blocked_doc.insert(
            "https://example.com/private/map.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
            )),
        );
        let report = discover_with_allows(
            vec!["https://example.com/private/map.xml".into()],
            blocked_doc,
            |url| !url.path().starts_with("/private"),
        );
        assert_eq!(report.files[0].state, FileState::Skipped);
        assert_eq!(report.files[0].issue, "Blocked by robots.txt");
    }

    #[test]
    fn multiple_declarations_are_deduplicated_and_all_are_read() {
        let requested = std::sync::Mutex::new(Vec::new());
        let mut map = HashMap::new();
        map.insert(
            "https://example.com/one.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
            )),
        );
        map.insert(
            "https://example.com/two.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/b</loc></url></urlset>"
            )),
        );
        let seed = Url::parse("https://example.com/").unwrap();
        let fetch = |url: &str| {
            requested.lock().unwrap().push(url.to_string());
            map.get(url).cloned().unwrap_or_else(|| missing(404))
        };
        let cancel = AtomicBool::new(false);
        let report = discover(
            &seed,
            vec![
                "https://example.com/one.xml".into(),
                "https://example.com/two.xml".into(),
                "https://example.com/one.xml".into(),
            ],
            &|_| true,
            &fetch,
            &cancel,
            &mut |_| {},
        );
        assert_eq!(report.files.len(), 2);
        assert_eq!(report.urls.len(), 2);
        let requested = requested.into_inner().unwrap();
        assert_eq!(
            requested
                .iter()
                .filter(|url| *url == "https://example.com/one.xml")
                .count(),
            1,
            "{requested:?}"
        );
        assert!(
            !requested.iter().any(|url| url.ends_with("/sitemap.xml")),
            "{requested:?}"
        );
    }

    #[test]
    fn an_index_cycle_reads_each_document_once() {
        let mut map = HashMap::new();
        map.insert(
            "https://example.com/root.xml".to_string(),
            doc(
                r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                <sitemap><loc>https://example.com/root.xml</loc></sitemap>
                <sitemap><loc>https://example.com/child.xml</loc></sitemap>
            </sitemapindex>"#,
            ),
        );
        map.insert(
            "https://example.com/child.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
            )),
        );
        let report = discover_with(vec!["https://example.com/root.xml".into()], map);
        assert_eq!(report.files.len(), 2, "{:?}", report.files);
        assert_eq!(report.urls.len(), 1);
        assert!(report.partial.is_none(), "{:?}", report.partial);
    }

    #[test]
    fn sitemap_redirects_are_checked_against_scope_and_robots() {
        let requested = std::sync::Mutex::new(Vec::new());
        let mut map = HashMap::new();
        map.insert(
            "https://example.com/old.xml".to_string(),
            FetchedBytes {
                status: 301,
                location: "https://other.test/map.xml".into(),
                ..FetchedBytes::default()
            },
        );
        map.insert(
            "https://example.com/safe.xml".to_string(),
            FetchedBytes {
                status: 302,
                location: "/private/map.xml".into(),
                ..FetchedBytes::default()
            },
        );
        let seed = Url::parse("https://example.com/").unwrap();
        let fetch = |url: &str| {
            requested.lock().unwrap().push(url.to_string());
            map.get(url).cloned().unwrap_or_else(|| missing(404))
        };
        let cancel = AtomicBool::new(false);
        let report = discover(
            &seed,
            vec!["https://example.com/old.xml".into()],
            &|_| true,
            &fetch,
            &cancel,
            &mut |_| {},
        );
        assert_eq!(report.files[0].state, FileState::Skipped);
        assert_eq!(report.files[0].issue, "Outside this site");

        let report = discover(
            &seed,
            vec!["https://example.com/safe.xml".into()],
            &|url| !url.path().starts_with("/private"),
            &fetch,
            &cancel,
            &mut |_| {},
        );
        assert_eq!(report.files[0].state, FileState::Skipped);
        assert_eq!(report.files[0].issue, "Blocked by robots.txt");
        let requested = requested.into_inner().unwrap();
        assert!(
            !requested
                .iter()
                .any(|url| url.contains("other.test") || url.contains("/private/map.xml")),
            "{requested:?}"
        );
    }

    #[test]
    fn duplicate_listings_keep_every_source_and_the_document_limit_is_visible() {
        let mut map = HashMap::new();
        map.insert(
            "https://example.com/one.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
            )),
        );
        map.insert(
            "https://example.com/two.xml".to_string(),
            doc(&format!(
                "{URLSET_HEAD}<url><loc>https://example.com/a</loc></url></urlset>"
            )),
        );
        let report = discover_with(
            vec![
                "https://example.com/one.xml".into(),
                "https://example.com/two.xml".into(),
            ],
            map,
        );
        assert_eq!(report.urls.len(), 1);
        assert_eq!(
            report.urls[0].sources,
            vec![
                "https://example.com/one.xml".to_string(),
                "https://example.com/two.xml".to_string()
            ]
        );

        let mut many = HashMap::new();
        let mut index =
            String::from(r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for n in 0..=MAX_DOCUMENTS {
            index.push_str(&format!(
                "<sitemap><loc>https://example.com/part-{n}.xml</loc></sitemap>"
            ));
            many.insert(
                format!("https://example.com/part-{n}.xml"),
                doc(&format!(
                    "{URLSET_HEAD}<url><loc>https://example.com/page-{n}</loc></url></urlset>"
                )),
            );
        }
        index.push_str("</sitemapindex>");
        many.insert("https://example.com/root.xml".to_string(), doc(&index));
        let report = discover_with(vec!["https://example.com/root.xml".into()], many);
        assert!(
            report
                .partial
                .as_deref()
                .is_some_and(|reason| reason.contains("More than 100 sitemap files")),
            "{:?}",
            report.partial
        );
        assert_eq!(report.files.len(), MAX_DOCUMENTS);
    }

    #[test]
    fn entries_without_locations_still_consume_the_crawl_wide_budget() {
        let empty_entries = format!(
            "{URLSET_HEAD}{}{}</urlset>",
            "<url/>".repeat(MAX_ENTRIES_PER_DOC),
            ""
        );
        let mut map = HashMap::new();
        for name in ["one", "two"] {
            map.insert(
                format!("https://example.com/{name}.xml"),
                doc(&empty_entries),
            );
        }
        map.insert(
            "https://example.com/three.xml".into(),
            doc(&format!("{URLSET_HEAD}<url/></urlset>")),
        );
        let report = discover_with(
            ["one", "two", "three"]
                .into_iter()
                .map(|name| format!("https://example.com/{name}.xml"))
                .collect(),
            map,
        );
        assert!(report.urls.is_empty());
        assert!(
            report
                .partial
                .as_deref()
                .is_some_and(|reason| reason.contains("More than 100000")),
            "{:?}",
            report.partial
        );
    }

    #[test]
    fn summary_reconciles_with_the_reported_entries() {
        let report = sample_report();
        let summary = report.summary();
        assert_eq!(summary.files, report.files.len());
        assert_eq!(summary.listed, report.urls.len());
        assert_eq!(
            summary.checked
                + summary.blocked
                + summary.skipped
                + summary.not_checked
                + summary.pending,
            summary.listed
        );
        assert!(summary.file_errors > 0);
        assert!(summary.issue_urls > 0);
        assert!(
            report
                .urls
                .iter()
                .any(|entry| entry.state == UrlState::NotChecked)
        );
    }
}
