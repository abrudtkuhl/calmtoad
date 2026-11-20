//! One-host crawl and the four on-page audits around it.
//!
//! The spider stores every fetched URL. Titles, meta descriptions and H1s are judged only on
//! HTTP 200 HTML. Response codes cover every URL, including redirects, 404s and 500s.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasherDefault;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use scraper::{ElementRef, Html, Node};
use url::Url;

use crate::content::{self, ContentBudget, ContentEvidence};
use crate::extract::{self, ExtractionOutcome};
use crate::indexability::{
    self, Assessment, CanonicalDeclaration, CaptureInfo, CaptureMode, EvidenceStage,
    OmittedEvidence, PageEvidence, ReadState, RobotsDirective,
};
use crate::links::{self, LinkCheckResult, LinkCheckState, LinkOccurrence};
use crate::render::{self, BrowserRenderer, Capture, RenderMode, RenderState, SourceState};
use crate::sitemap;

/// A small, dependency-free hasher for the crawl's hot maps.
///
/// The default SipHash is a poor fit for short URL and title keys that never leave the
/// process. This is the FxHash mixing function (the one rustc uses): fast, deterministic and
/// good enough for lookups. It is not collision-resistant against an attacker, which is fine
/// for a local desktop spider reading a site it was pointed at.
#[derive(Clone, Copy, Debug, Default)]
struct FastHasher {
    hash: u64,
}

impl FastHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.add(u64::from_ne_bytes(*chunk));
        }
        if rest.len() >= 4 {
            self.add(u32::from_ne_bytes(rest[..4].try_into().unwrap()) as u64);
            let tail = &rest[4..];
            if !tail.is_empty() {
                self.add(
                    tail[0] as u64
                        | (tail.get(1).copied().unwrap_or(0) as u64) << 8
                        | (tail.get(2).copied().unwrap_or(0) as u64) << 16,
                );
            }
        } else if !rest.is_empty() {
            let mut word = 0_u64;
            for (index, byte) in rest.iter().enumerate() {
                word |= (*byte as u64) << (index * 8);
            }
            self.add(word);
        }
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;
type FastSet<T> = HashSet<T, BuildHasherDefault<FastHasher>>;
type SitemapFetcher<'a> =
    dyn Fn(&str, &dyn Fn() -> bool, &dyn Fn(u64)) -> sitemap::FetchedBytes + Sync + 'a;

fn fast_set<T: std::hash::Hash + Eq>(capacity: usize) -> FastSet<T> {
    let mut set = FastSet::default();
    set.reserve(capacity);
    set
}

fn fast_map<K: std::hash::Hash + Eq, V>(capacity: usize) -> FastMap<K, V> {
    let mut map = FastMap::default();
    map.reserve(capacity);
    map
}

pub const DEFAULT_MAX_PAGES: usize = 40;
pub const MAX_DEPTH: u8 = 3;
pub const TITLE_MIN: usize = 30;
pub const TITLE_MAX: usize = 60;
pub const META_MIN: usize = 70;
pub const META_MAX: usize = 160;

/// Sent on every crawl request (HTML fetches, robots.txt, sitemaps, and Browser mode), so site
/// owners see the release that visited them. The version comes from `Cargo.toml` at build time.
pub const USER_AGENT: &str = concat!(
    "CalmToad/",
    env!("CARGO_PKG_VERSION"),
    " (+https://calmtoad.com)"
);
/// Fetches run in parallel because almost all of a crawl is spent waiting on the network.
/// The cap keeps us fast without hammering a small site.
const PARALLEL_REQUESTS: usize = 16;
const MAX_BODY_BYTES: u64 = 1_000_000;
/// Error and redirect bodies we are willing to read just to keep the connection reusable.
const DRAIN_BYTES: u64 = 64 * 1024;
/// How long the pool rests after a server pushes back, when it sent no `Retry-After`.
const PAUSE_PUSHBACK_MS: u64 = 1_000;
/// Transient failures get a couple of quick retries before they become crawl results.
const FETCH_ATTEMPTS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub title: String,
    pub meta_description: String,
    pub h1s: Vec<String>,
    /// Compatibility derivative of the assessment: an applicable generic or Googlebot rule.
    /// The authoritative object is [`Page::indexability`].
    pub noindex: bool,
    pub words: usize,
    pub redirect_to: String,
    pub error: String,
    pub depth: u8,
    /// Compatibility derivative: the first valid HTML canonical target, normalized like `url`.
    /// The authoritative evidence is [`Page::indexability`].
    pub canonical: String,
    pub images: usize,
    pub images_missing_alt: usize,
    /// Same-host links found on the page, normalized like `url`.
    ///
    /// The vector is shared so an inventory snapshot can retain the page without copying a
    /// potentially large link list. Pages are never mutated after publication except through
    /// `Arc::make_mut` during the pre-publication check pass.
    pub links: Arc<Vec<String>>,
    /// Individual anchor occurrences, including skipped and external destinations.
    pub link_evidence: Arc<Vec<LinkOccurrence>>,
    /// Nonempty when this page's retained link evidence hit a page or run cap.
    pub link_evidence_limit: String,
    /// Custom CSS extraction outcomes for this page, in rule order.
    pub extraction: Arc<Vec<ExtractionOutcome>>,
    /// Nonempty when extraction stopped at a per-page or per-run cap.
    pub extraction_limit: String,
    /// Duplicate-content evidence for the selected capture, shared across clones.
    pub content: Arc<ContentEvidence>,
    /// The one evidence-backed indexability assessment every view reads.
    pub indexability: Assessment,
}

impl Page {
    pub fn h1(&self) -> String {
        self.h1s.join(" · ")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_pages: usize,
    pub max_depth: u8,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pages: DEFAULT_MAX_PAGES,
            max_depth: MAX_DEPTH,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub status: u16,
    pub content_type: String,
    pub location: String,
    pub body: String,
    pub error: String,
    /// `Retry-After` in milliseconds, when a rate-limited server sent one.
    pub retry_after_ms: u64,
    /// Every bounded `X-Robots-Tag` directive, including agent scopes.
    pub header_directives: Vec<RobotsDirective>,
    /// More header directives were present than the evidence cap retained.
    pub header_truncated: bool,
    /// Effective restrictions from header directives beyond the retained cap.
    pub header_omitted: OmittedEvidence,
    /// Whether the body was read completely, truncated at the crawl limit, or not read.
    pub read: ReadState,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// The streaming UI consumes pages as they land; the final outcome keeps them for callers
    /// that want the whole set at once (and for tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub pages: Vec<Page>,
    pub message: String,
}

/// Per-crawl choices. Links-only callers keep the old behavior with the default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrawlOptions {
    /// Read the seed origin's sitemaps and schedule the URLs they list.
    pub sitemaps: bool,
    /// Capture pages with the fast HTTP fetch or a local headless browser.
    pub rendering: RenderMode,
    /// An explicit Chrome/Chromium for Browser mode. `None` discovers one.
    pub browser_path: Option<PathBuf>,
    /// Check observed external anchors and same-host document downloads after the page crawl.
    pub check_links: bool,
    /// Draft rules are compiled once before any page work starts.
    pub extraction_rules: Vec<extract::ExtractionRule>,
}

/// Everything the window streams while a crawl runs.
#[derive(Clone, Debug)]
pub enum CrawlEvent {
    /// Boxed because a fully assessed page is much larger than the sitemap variants.
    Page(Box<Page>),
    SitemapFile(sitemap::SitemapFile),
    SitemapUrl(sitemap::SitemapUrl),
    SitemapPartial(String),
    /// Discovery finished; page fetches for the listed URLs may still be running.
    SitemapDone,
    /// Optional external/document checks started with this many unique destinations.
    LinkCheckStarted(usize),
    /// One optional destination check finished.
    LinkCheck(String, LinkCheckResult),
}

/// The whole result of one crawl, including the sitemap report.
#[derive(Clone, Debug, Default)]
pub struct CrawlResult {
    pub pages: Vec<Page>,
    pub message: String,
    pub sitemap: sitemap::SitemapReport,
    /// Why the crawl ended, as a typed fact rather than a presentation string.
    pub outcome: CrawlOutcome,
    /// A browser setup failure, separate from page/domain failures. The message stays short
    /// for the run summary; this detail belongs behind the browser setup disclosure.
    pub setup_failure: Option<String>,
    /// The retained link-evidence tail hit the run cap.
    pub link_evidence_limited: bool,
}

/// How a crawl ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CrawlOutcome {
    #[default]
    Completed,
    /// The user stopped it.
    Stopped,
    /// The crawl reached its page budget.
    Limited,
    /// No page could be checked, or startup failed.
    Failed,
    /// robots.txt refused the seed before any page work started.
    Blocked,
}

#[derive(Clone, Debug, Default)]
struct Robots {
    rules: Vec<RobotsRule>,
    crawl_delay_ms: u64,
    /// `Sitemap:` declarations, which are independent of user-agent groups.
    sitemaps: Vec<String>,
}

#[derive(Clone, Debug)]
struct RobotsRule {
    allow: bool,
    pattern: String,
    length: usize,
}

impl Robots {
    fn parse(body: &str, user_agent: &str) -> Self {
        #[derive(Default)]
        struct Group {
            agents: Vec<String>,
            rules: Vec<(bool, String)>,
            delay_ms: u64,
        }

        let mut groups: Vec<Group> = Vec::new();
        let mut current: Option<Group> = None;
        let mut after_agent = false;
        let mut sitemaps: Vec<String> = Vec::new();
        for raw in body.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            let field = field.trim();
            let value = value.trim();
            if field.eq_ignore_ascii_case("sitemap") {
                if !value.is_empty() && !sitemaps.iter().any(|seen| seen == value) {
                    sitemaps.push(value.to_string());
                }
                continue;
            }
            if field.eq_ignore_ascii_case("user-agent") {
                if !after_agent {
                    if let Some(group) = current.take() {
                        groups.push(group);
                    }
                    current = Some(Group::default());
                }
                if let Some(group) = current.as_mut()
                    && !value.is_empty()
                {
                    group.agents.push(value.to_ascii_lowercase());
                }
                after_agent = true;
                continue;
            }
            after_agent = false;
            let Some(group) = current.as_mut() else {
                continue;
            };
            if field.eq_ignore_ascii_case("allow") || field.eq_ignore_ascii_case("disallow") {
                if !value.is_empty() {
                    group
                        .rules
                        .push((field.eq_ignore_ascii_case("allow"), value.to_string()));
                }
            } else if field.eq_ignore_ascii_case("crawl-delay")
                && let Ok(seconds) = value.parse::<f64>()
                && seconds.is_finite()
                && seconds > 0.0
            {
                group.delay_ms = (seconds * 1_000.0).round() as u64;
            }
        }
        if let Some(group) = current {
            groups.push(group);
        }

        let agent = user_agent.to_ascii_lowercase();
        let mut best = 0;
        let mut matched: Vec<&Group> = Vec::new();
        let mut wildcard: Option<&Group> = None;
        for group in &groups {
            for token in &group.agents {
                if token == "*" {
                    wildcard = Some(group);
                } else if agent.contains(token.as_str()) {
                    if token.len() > best {
                        best = token.len();
                        matched.clear();
                        matched.push(group);
                    } else if token.len() == best {
                        matched.push(group);
                    }
                }
            }
        }
        if matched.is_empty()
            && let Some(group) = wildcard
        {
            matched.push(group);
        }

        let mut rules = Vec::new();
        let mut crawl_delay_ms = 0;
        for group in matched {
            crawl_delay_ms = crawl_delay_ms.max(group.delay_ms);
            for (allow, pattern) in &group.rules {
                rules.push(RobotsRule {
                    allow: *allow,
                    pattern: pattern.clone(),
                    length: pattern.len(),
                });
            }
        }
        Self {
            rules,
            crawl_delay_ms,
            sitemaps,
        }
    }

    fn allows(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for rule in &self.rules {
            if !pattern_matches(&rule.pattern, path) {
                continue;
            }
            let replace = match best {
                None => true,
                Some((length, allow)) => {
                    rule.length > length || (rule.length == length && rule.allow && !allow)
                }
            };
            if replace {
                best = Some((rule.length, rule.allow));
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }

    fn crawl_delay(&self) -> Option<Duration> {
        (self.crawl_delay_ms > 0).then(|| Duration::from_millis(self.crawl_delay_ms))
    }
}

fn pattern_matches(pattern: &str, path: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(pattern) => (pattern, true),
        None => (pattern, false),
    };
    let mut rest = path;
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            first = false;
            continue;
        }
        if first && !pattern.starts_with('*') {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
        } else {
            let Some(index) = rest.find(part) else {
                return false;
            };
            rest = &rest[index + part.len()..];
        }
        first = false;
    }
    !anchored || rest.is_empty()
}

fn request_path(url: &Url) -> Cow<'_, str> {
    match url.query() {
        Some(query) => Cow::Owned(format!("{}?{}", url.path(), query)),
        None => Cow::Borrowed(url.path()),
    }
}

static ROBOTS_CACHE: OnceLock<Mutex<HashMap<String, Option<Robots>>>> = OnceLock::new();

fn robots_for(
    seed: &Url,
    fetch: &(impl Fn(&str) -> Fetched + Sync),
    cache: bool,
) -> Option<Robots> {
    let origin = seed.origin().ascii_serialization();
    if cache {
        let cache = ROBOTS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(hit) = cache.lock().unwrap().get(&origin) {
            return hit.clone();
        }
    }
    let fetched = fetch(&format!("{origin}/robots.txt"));
    let robots = (fetched.status == 200 && !fetched.body.is_empty())
        .then(|| Robots::parse(&fetched.body, USER_AGENT));
    if cache {
        let cache = ROBOTS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        cache.lock().unwrap().insert(origin, robots.clone());
    }
    robots
}

#[derive(Clone, Debug, Default)]
pub struct Audit {
    /// Values seen so far, kept so a streaming caller can add one page at a time without
    /// rebuilding the sets. The duplicate sets only hold values seen more than once.
    title_seen: FastSet<String>,
    meta_seen: FastSet<String>,
    h1_seen: FastSet<String>,
    title_dups: FastSet<String>,
    meta_dups: FastSet<String>,
    h1_dups: FastSet<String>,
    /// Redirect source → target, for spotting chains.
    redirects: FastMap<String, String>,
    /// How many crawled pages link to each URL.
    inbound: FastMap<String, usize>,
    /// URLs that ended in an error, used to find broken outbound links.
    broken_targets: FastSet<String>,
    /// Source URL → (broken outbound count, first broken target).
    broken_links: FastMap<String, (usize, String)>,
    /// Reused by `push` so normalizing one page's fields does not allocate per page.
    scratch: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DuplicateField {
    Title,
    Meta,
    H1,
}

impl Audit {
    pub fn from_pages(pages: &[Page]) -> Self {
        let mut audit = Self {
            inbound: fast_map(pages.len()),
            redirects: fast_map(pages.len() / 16),
            title_seen: fast_set(pages.len()),
            meta_seen: fast_set(pages.len()),
            h1_seen: fast_set(pages.len()),
            ..Self::default()
        };
        // `inbound` is only ever read for URLs that became pages, so links to anything else
        // do not need an entry. Count in a borrowed-key map first: one hash per link, and no
        // string copies until the final map is built.
        let mut inbound: FastMap<&str, usize> = fast_map(pages.len());
        for page in pages {
            inbound.entry(page.url.as_str()).or_insert(0);
            if (300..400).contains(&page.status) && !page.redirect_to.is_empty() {
                audit
                    .redirects
                    .insert(page.url.clone(), page.redirect_to.clone());
            }
            if page.status == 0 || page.status >= 400 {
                audit.broken_targets.insert(page.url.clone());
            }
        }
        let has_broken = !audit.broken_targets.is_empty();
        for page in pages {
            for link in page.links.iter() {
                let Some(count) = inbound.get_mut(link.as_str()) else {
                    continue;
                };
                *count += 1;
                if has_broken && audit.broken_targets.contains(link) {
                    audit.record_broken_link(page, link);
                }
            }
        }
        for (url, count) in inbound {
            if count > 0 {
                audit.inbound.insert(url.to_string(), count);
            }
        }
        let mut buffer = String::new();
        for page in pages {
            if !auditable(page) {
                continue;
            }
            collect_duplicate(
                &mut audit.title_seen,
                &mut audit.title_dups,
                &mut buffer,
                &page.title,
            );
            collect_duplicate(
                &mut audit.meta_seen,
                &mut audit.meta_dups,
                &mut buffer,
                &page.meta_description,
            );
            if page.h1s.len() == 1 {
                collect_duplicate(
                    &mut audit.h1_seen,
                    &mut audit.h1_dups,
                    &mut buffer,
                    &page.h1s[0],
                );
            }
        }
        audit
    }

    /// Add one page to the audit without rebuilding anything already counted.
    ///
    /// A streaming crawl calls this for each page as it lands. Broken-link backfill is handled
    /// separately by `rebuild_broken_links`, because a page that is broken may have been linked
    /// from pages that arrived earlier.
    pub fn push(&mut self, page: &Page) {
        if (300..400).contains(&page.status) && !page.redirect_to.is_empty() {
            self.redirects
                .insert(page.url.clone(), page.redirect_to.clone());
        }
        let broken = page.status == 0 || page.status >= 400;
        if broken {
            self.broken_targets.insert(page.url.clone());
        }
        // Most crawls have no broken pages at all; skip the lookup for every link when so.
        let has_broken = !self.broken_targets.is_empty();
        for link in page.links.iter() {
            // Clone only when the link is new; most links repeat across pages.
            match self.inbound.get_mut(link.as_str()) {
                Some(count) => *count += 1,
                None => {
                    self.inbound.insert(link.clone(), 1);
                }
            }
            if has_broken && self.broken_targets.contains(link) {
                self.record_broken_link(page, link);
            }
        }
        if !auditable(page) {
            return;
        }
        let Self {
            title_seen,
            meta_seen,
            h1_seen,
            title_dups,
            meta_dups,
            h1_dups,
            scratch,
            ..
        } = self;
        collect_duplicate(title_seen, title_dups, scratch, &page.title);
        collect_duplicate(meta_seen, meta_dups, scratch, &page.meta_description);
        if page.h1s.len() == 1 {
            collect_duplicate(h1_seen, h1_dups, scratch, &page.h1s[0]);
        }
    }

    fn record_broken_link(&mut self, page: &Page, link: &str) {
        let entry = self
            .broken_links
            .entry(page.url.clone())
            .or_insert_with(|| (0, String::new()));
        entry.0 += 1;
        if entry.1.is_empty() {
            entry.1 = link.to_string();
        }
    }

    /// Recompute outbound broken-link counts from every page seen so far.
    ///
    /// Cheap to skip when nothing is broken, which is the common case.
    pub fn rebuild_broken_links(&mut self, pages: &[Page]) {
        self.broken_links.clear();
        if self.broken_targets.is_empty() {
            return;
        }
        for page in pages {
            for link in page.links.iter() {
                if self.broken_targets.contains(link) {
                    self.record_broken_link(page, link);
                }
            }
        }
    }

    pub fn inbound(&self, page: &Page) -> usize {
        self.inbound.get(&page.url).copied().unwrap_or(0)
    }

    /// Whether these rows would show a broken-link finding.
    pub fn has_broken_links(&self, page: &Page) -> bool {
        self.broken_links.contains_key(&page.url)
    }

    /// Other auditable pages whose title normalizes to this page's title.
    pub fn title_duplicate_peers<'a>(&self, pages: &'a [Page], page: &Page) -> Vec<&'a Page> {
        self.duplicate_peers(
            pages,
            page,
            &page.title,
            &self.title_dups,
            DuplicateField::Title,
        )
    }

    /// Other auditable pages whose meta description normalizes to this page's.
    pub fn meta_duplicate_peers<'a>(&self, pages: &'a [Page], page: &Page) -> Vec<&'a Page> {
        self.duplicate_peers(
            pages,
            page,
            &page.meta_description,
            &self.meta_dups,
            DuplicateField::Meta,
        )
    }

    /// Other auditable pages whose single H1 normalizes to this page's single H1.
    pub fn h1_duplicate_peers<'a>(&self, pages: &'a [Page], page: &Page) -> Vec<&'a Page> {
        if page.h1s.len() != 1 {
            return Vec::new();
        }
        self.duplicate_peers(pages, page, &page.h1s[0], &self.h1_dups, DuplicateField::H1)
    }

    fn duplicate_peers<'a>(
        &self,
        pages: &'a [Page],
        page: &Page,
        value: &str,
        dups: &FastSet<String>,
        field: DuplicateField,
    ) -> Vec<&'a Page> {
        let key = norm(value);
        if key.is_empty() || !dups.contains(&key) {
            return Vec::new();
        }
        pages
            .iter()
            .filter(|other| other.url != page.url && auditable(other))
            .filter(|other| {
                let other_value = match field {
                    DuplicateField::Title => &other.title,
                    DuplicateField::Meta => &other.meta_description,
                    DuplicateField::H1 => {
                        if other.h1s.len() != 1 {
                            return false;
                        }
                        &other.h1s[0]
                    }
                };
                norm(other_value) == key
            })
            .collect()
    }

    /// Pages that contain an observed link to this page.
    pub fn inbound_sources<'a>(&self, pages: &'a [Page], page: &Page) -> Vec<&'a Page> {
        let mut seen: FastSet<&str> = FastSet::default();
        pages
            .iter()
            .filter(|other| other.url != page.url)
            .filter(|other| other.links.iter().any(|link| link == &page.url))
            .filter(|other| seen.insert(other.url.as_str()))
            .collect()
    }

    /// Follow the redirect target chain for this page, stopping at a repeat or eight hops.
    pub fn redirect_chain(&self, page: &Page) -> Vec<String> {
        let mut chain = Vec::new();
        let mut seen: FastSet<&str> = FastSet::default();
        let mut target = page.redirect_to.as_str();
        while !target.is_empty() && chain.len() < 8 && seen.insert(target) {
            chain.push(target.to_string());
            let Some(next) = self.redirects.get(target) else {
                break;
            };
            target = next;
        }
        chain
    }
}

/// The content checks the result views filter on, without formatting their issue text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentCheck {
    Title,
    Meta,
    H1,
    Canonical,
    Images,
}

/// Whether a page has a finding for `check`, using the same rules as the row audit.
pub fn content_issue(page: &Page, audit: &Audit, check: ContentCheck) -> bool {
    let auditable = auditable(page);
    let mut buffer = String::new();
    match check {
        ContentCheck::Title => title_has_issue(page, auditable, audit, &mut buffer),
        ContentCheck::Meta => meta_has_issue(page, auditable, audit, &mut buffer),
        ContentCheck::H1 => h1_has_issue(page, auditable, audit, &mut buffer),
        ContentCheck::Canonical => canonical_has_issue(page, auditable),
        ContentCheck::Images => auditable && page.images_missing_alt > 0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub pages: usize,
    pub title: usize,
    pub meta: usize,
    pub h1: usize,
    pub response: usize,
    pub blocked: usize,
    pub canonical: usize,
    pub images: usize,
    pub links: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn counts(pages: &[Page]) -> Counts {
    counts_with(pages, &Audit::from_pages(pages))
}

/// Counts with an audit that is already built, for callers that repaint more often than the
/// page set changes.
///
/// Counting only needs to know whether a field has an issue, not what the issue says, so this
/// path avoids building issue strings and reuses one normalization buffer across the page set.
pub fn counts_with(pages: &[Page], audit: &Audit) -> Counts {
    let mut counts = Counts {
        pages: pages.len(),
        ..Counts::default()
    };
    let mut buffer = String::new();
    for page in pages {
        let auditable = auditable(page);
        counts.title += title_has_issue(page, auditable, audit, &mut buffer) as usize;
        counts.meta += meta_has_issue(page, auditable, audit, &mut buffer) as usize;
        counts.h1 += h1_has_issue(page, auditable, audit, &mut buffer) as usize;
        // `response_issue` is empty only for a 2xx response.
        counts.response += (!(200..=299).contains(&page.status)) as usize;
        counts.blocked += page.indexability.has_confirmed_issue() as usize;
        counts.canonical += canonical_has_issue(page, auditable) as usize;
        counts.images += (auditable && page.images_missing_alt > 0) as usize;
        counts.links +=
            (!audit.broken_links.is_empty() && audit.broken_links.contains_key(&page.url)) as usize;
    }
    counts
}

/// Character count, with a cheap fast path for the ASCII that dominates real pages.
fn text_len(value: &str) -> usize {
    if value.is_ascii() {
        value.len()
    } else {
        value.chars().count()
    }
}

fn title_has_issue(page: &Page, auditable: bool, audit: &Audit, buffer: &mut String) -> bool {
    if !auditable {
        return false;
    }
    let length = text_len(&page.title);
    length == 0
        || !(TITLE_MIN..=TITLE_MAX).contains(&length)
        || (!audit.title_dups.is_empty()
            && audit.title_dups.contains(norm_into(&page.title, buffer)))
}

fn meta_has_issue(page: &Page, auditable: bool, audit: &Audit, buffer: &mut String) -> bool {
    if !auditable {
        return false;
    }
    let length = text_len(&page.meta_description);
    length == 0
        || !(META_MIN..=META_MAX).contains(&length)
        || (!audit.meta_dups.is_empty()
            && audit
                .meta_dups
                .contains(norm_into(&page.meta_description, buffer)))
}

fn h1_has_issue(page: &Page, auditable: bool, audit: &Audit, buffer: &mut String) -> bool {
    if !auditable {
        return false;
    }
    if page.h1s.is_empty() || page.h1s.len() > 1 {
        return true;
    }
    !audit.h1_dups.is_empty() && audit.h1_dups.contains(norm_into(&page.h1s[0], buffer))
}

fn canonical_has_issue(page: &Page, auditable: bool) -> bool {
    auditable && page.indexability.canonical_needs_attention()
}

pub fn auditable(page: &Page) -> bool {
    page.status == 200
        && is_html(&page.content_type, "")
        && page.error.is_empty()
        && !matches!(
            page.indexability.evidence.read,
            ReadState::Failed | ReadState::Truncated | ReadState::NotRead
        )
        // An incomplete Browser capture can look whole while scripts, source evidence or the
        // document itself were cut short. Content audits wait until the capture is complete.
        && page.indexability.capture.is_complete()
}

pub fn title_issue(page: &Page, audit: &Audit) -> String {
    let duplicate = !audit.title_dups.is_empty() && audit.title_dups.contains(&norm(&page.title));
    field_issue(page, &page.title, TITLE_MIN, TITLE_MAX, duplicate)
}

pub fn meta_issue(page: &Page, audit: &Audit) -> String {
    let duplicate =
        !audit.meta_dups.is_empty() && audit.meta_dups.contains(&norm(&page.meta_description));
    field_issue(page, &page.meta_description, META_MIN, META_MAX, duplicate)
}

pub fn h1_issue(page: &Page, audit: &Audit) -> String {
    if !auditable(page) {
        return String::new();
    }
    if page.h1s.is_empty() {
        return "Missing".into();
    }
    if page.h1s.len() > 1 {
        return "Multiple".into();
    }
    if !audit.h1_dups.is_empty() && audit.h1_dups.contains(&norm(&page.h1s[0])) {
        return "Duplicate".into();
    }
    String::new()
}

pub fn canonical_issue(page: &Page) -> String {
    if !auditable(page) {
        return String::new();
    }
    page.indexability.canonical_issue_text()
}

pub fn image_issue(page: &Page) -> String {
    if !auditable(page) || page.images_missing_alt == 0 {
        return String::new();
    }
    if page.images_missing_alt == page.images {
        match page.images {
            1 => "1 image without alt".into(),
            n => format!("{n} images without alt"),
        }
    } else {
        format!(
            "{} of {} images without alt",
            page.images_missing_alt, page.images
        )
    }
}

/// A same-host link on a page that ended in an error, or never answered.
pub fn links_issue(page: &Page, audit: &Audit) -> String {
    match audit.broken_links.get(&page.url) {
        None => String::new(),
        Some((1, target)) => format!("Broken link to {}", short_target(target)),
        Some((n, _)) => format!("{n} broken links"),
    }
}

/// A redirect whose target redirects again; the final hop may be several pages away.
pub fn redirect_issue(page: &Page, audit: &Audit) -> String {
    if !(300..400).contains(&page.status) || page.redirect_to.is_empty() {
        return String::new();
    }
    let mut hops = 1_usize;
    let mut target = page.redirect_to.as_str();
    let mut seen: FastSet<&str> = FastSet::default();
    while let Some(next) = audit.redirects.get(target) {
        if hops > 8 || !seen.insert(target) {
            break;
        }
        hops += 1;
        target = next;
    }
    if hops > 1 {
        format!("Chain of {hops}")
    } else {
        String::new()
    }
}

pub fn response_issue(page: &Page) -> String {
    if page.status == 0 {
        return if page.error.is_empty() {
            "Error".into()
        } else {
            page.error.clone()
        };
    }
    match page.status {
        200..=299 => String::new(),
        300..=399 => {
            if page.redirect_to.is_empty() {
                "Redirect".into()
            } else {
                format!("Redirect to {}", short_target(&page.redirect_to))
            }
        }
        400..=499 => format!("{} Client error", page.status),
        500..=599 => format!("{} Server error", page.status),
        status => format!("{status}"),
    }
}

/// `https://` is assumed when the scheme is missing.
pub fn normalize_seed(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Enter a URL to crawl.".into());
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = Url::parse(&with_scheme).map_err(|_| format!("Couldn't read “{raw}” as a URL."))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Use an http or https URL.".into());
    }
    if url.host_str().is_none() {
        return Err("That URL has no host.".into());
    }
    Ok(url_key(url))
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn crawl_site(seed: &str) -> Outcome {
    let result = crawl_engine(
        seed,
        Limits::default(),
        |url| fetch(agent(), url),
        |url, before, backoff| fetch_sitemap(agent(), url, None, before, backoff),
        true,
        CrawlOptions::default(),
        None,
        None,
        None,
        &mut |_| {},
    );
    Outcome {
        pages: result.pages,
        message: result.message,
    }
}

/// Crawl while reporting every page as soon as it lands. `cancel` is polled between pages so
/// the UI can stop a crawl without waiting for the whole queue to drain.
pub fn crawl_site_stream(
    seed: &str,
    limits: Limits,
    cancel: &AtomicBool,
    on_page: impl FnMut(&Page),
) -> Outcome {
    let mut on_page = on_page;
    let result = crawl_engine(
        seed,
        limits,
        |url| fetch(agent(), url),
        |url, before, backoff| fetch_sitemap(agent(), url, Some(cancel), before, backoff),
        true,
        CrawlOptions::default(),
        Some(cancel),
        None,
        None,
        &mut |event| {
            if let CrawlEvent::Page(page) = event {
                on_page(&page);
            }
        },
    );
    Outcome {
        pages: result.pages,
        message: result.message,
    }
}

/// Crawl with sitemap discovery and stream every update as it happens.
///
/// Browser mode launches one local browser for the whole crawl. A missing or unusable browser
/// is reported before any page work starts, so HTML mode never pays for a browser check.
pub fn crawl_site_with(
    seed: &str,
    limits: Limits,
    options: CrawlOptions,
    cancel: Arc<AtomicBool>,
    mut on_event: impl FnMut(CrawlEvent),
) -> CrawlResult {
    let browser = match options.rendering {
        RenderMode::Html => None,
        RenderMode::Browser => {
            let config = render::RenderConfig {
                browser_path: options.browser_path.clone(),
                ..render::RenderConfig::default()
            };
            match BrowserRenderer::launch(config, cancel.clone()) {
                Ok(renderer) => Some(renderer),
                Err(reason) => {
                    // A stop during startup is not a broken installation, and must not open
                    // browser recovery controls.
                    if cancel.load(Ordering::Relaxed) {
                        return CrawlResult {
                            message: "Stopped before the browser finished starting.".into(),
                            pages: Vec::new(),
                            sitemap: sitemap::SitemapReport::default(),
                            outcome: CrawlOutcome::Stopped,
                            setup_failure: None,
                            link_evidence_limited: false,
                        };
                    }
                    return CrawlResult {
                        message: "Browser could not start.".into(),
                        pages: Vec::new(),
                        sitemap: sitemap::SitemapReport::default(),
                        outcome: CrawlOutcome::Failed,
                        setup_failure: Some(reason),
                        link_evidence_limited: false,
                    };
                }
            }
        }
    };
    let job = browser.as_ref().map(|renderer| BrowserJob {
        renderer,
        cancel: cancel.clone(),
    });
    let extraction = if options.extraction_rules.is_empty() {
        None
    } else {
        match extract::CompiledRules::compile(&options.extraction_rules) {
            Ok(compiled) => Some(compiled),
            Err(errors) => {
                return CrawlResult {
                    message: format!(
                        "Custom extraction rules are invalid: {}",
                        errors
                            .first()
                            .map(|error| error.message.as_str())
                            .unwrap_or("check the rule fields")
                    ),
                    outcome: CrawlOutcome::Failed,
                    ..CrawlResult::default()
                };
            }
        }
    };
    let result = crawl_engine(
        seed,
        limits,
        |url| fetch(agent(), url),
        |url, before, backoff| fetch_sitemap(agent(), url, Some(&cancel), before, backoff),
        true,
        options,
        Some(&cancel),
        job,
        extraction,
        &mut on_event,
    );
    drop(browser);
    result
}

/// A shared agent keeps its connection pool (and TLS sessions) warm between crawls, so
/// re-crawling the same site does not pay for a fresh handshake on every page.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(8))
            .redirects(0)
            .max_idle_connections_per_host(PARALLEL_REQUESTS)
            .no_delay(true)
            .user_agent(USER_AGENT)
            .build()
    })
}

/// A fetched page on its way from a worker to the crawl loop.
/// A launched browser and the stop flag shared by every page it renders.
#[derive(Clone)]
struct BrowserJob<'a> {
    renderer: &'a BrowserRenderer,
    cancel: Arc<AtomicBool>,
}

struct FetchedPage {
    url: String,
    depth: u8,
    fetched: Fetched,
    parsed: Option<Parsed>,
    /// The pre-script source document, kept only when the rendered DOM was also parsed.
    source: Option<Parsed>,
    capture: CaptureInfo,
    extraction: ExtractionInput,
}

struct ExtractionInput {
    html: Option<String>,
    stage: EvidenceStage,
    applicable: bool,
    partial: bool,
}

/// Page results and sitemap discovery share one channel so the loop can end only after
/// both kinds of work report completion.
enum EngineEvent {
    Page(Box<FetchedPage>),
    SitemapFile(sitemap::SitemapFile),
    SitemapUrl(sitemap::SitemapUrl),
    SitemapPartial(String),
    SitemapDone,
}

/// Shared work queue for the fetch workers.
#[derive(Default)]
struct Work {
    queue: VecDeque<(String, u8)>,
    done: bool,
    /// Set when a server pushes back; every worker pauses until then.
    pause_until: Option<Instant>,
    /// From `Crawl-delay`, shared so the whole pool paces itself as one client.
    crawl_delay: Option<Duration>,
    next_start: Option<Instant>,
}

/// Wakes the workers even if the crawl loop panics, so a bad page cannot hang the scope.
struct Finish<'a> {
    shared: &'a Mutex<Work>,
    wake: &'a Condvar,
}

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        let mut work = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        work.done = true;
        drop(work);
        self.wake.notify_all();
    }
}

/// Claim the next origin-wide request start, shared by page workers and sitemap reads.
fn reserve_request_slot(shared: &Mutex<Work>, wake: &Condvar, cancel: &AtomicBool) -> bool {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let mut work = shared.lock().unwrap_or_else(|error| error.into_inner());
        if work.done {
            return false;
        }
        let now = Instant::now();
        let mut available = now;
        if let Some(until) = work.pause_until {
            if until > available {
                available = until;
            } else {
                work.pause_until = None;
            }
        }
        if let Some(next_start) = work.next_start
            && next_start > available
        {
            available = next_start;
        }
        if available > now {
            let wait = (available - now).min(Duration::from_millis(50));
            let (guard, _) = wake
                .wait_timeout(work, wait)
                .unwrap_or_else(|error| error.into_inner());
            drop(guard);
            continue;
        }
        if let Some(delay) = work.crawl_delay {
            // Crawl-delay is untrusted robots.txt input. Keep a pathological value from
            // overflowing Instant while still applying a long, visible wait.
            work.next_start = Some(
                now.checked_add(delay)
                    .unwrap_or_else(|| now + Duration::from_secs(86_400)),
            );
        }
        return true;
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn crawl_with(seed: &str, limits: Limits, fetch: impl Fn(&str) -> Fetched + Sync) -> Outcome {
    let result = crawl_engine(
        seed,
        limits,
        fetch,
        |_, _before, _backoff| sitemap::FetchedBytes::default(),
        false,
        CrawlOptions::default(),
        None,
        None,
        None,
        &mut |_| {},
    );
    Outcome {
        pages: result.pages,
        message: result.message,
    }
}

/// Deterministic seam for sitemap integration tests. Production callers use
/// [`crawl_site_with`], which fetches both kinds of body over the shared agent.
#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
pub fn crawl_with_options(
    seed: &str,
    limits: Limits,
    options: CrawlOptions,
    fetch: impl Fn(&str) -> Fetched + Sync,
    fetch_sitemap: impl Fn(&str) -> sitemap::FetchedBytes + Sync,
    cancel: Option<&AtomicBool>,
    mut on_event: impl FnMut(CrawlEvent),
) -> CrawlResult {
    crawl_engine(
        seed,
        limits,
        fetch,
        |url, _before, _backoff| fetch_sitemap(url),
        false,
        options,
        cancel,
        None,
        None,
        &mut on_event,
    )
}

#[derive(Clone, Debug)]
enum RobotsPolicy {
    /// A known robots policy: `Some` when parsed, `None` for a confirmed absence.
    Known(Option<Robots>),
    /// The policy could not be read. This is unknown, not permission.
    Unavailable(String),
}

enum CheckStop {
    Cancelled,
    Deadline,
}

enum RobotsFailure {
    Stop(CheckStop),
    Unavailable(String),
}

impl CheckStop {
    fn limited(self) -> LinkCheckResult {
        match self {
            Self::Cancelled => LinkCheckResult::limited("Stopped during optional check"),
            Self::Deadline => {
                LinkCheckResult::limited("Optional check exceeded the 30 second deadline")
            }
        }
    }
}

fn check_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(5))
        .user_agent(USER_AGENT)
        .build()
}

fn check_remaining(cancel: &AtomicBool, deadline: Instant) -> Result<Duration, CheckStop> {
    if cancel.load(Ordering::Relaxed) {
        return Err(CheckStop::Cancelled);
    }
    let now = Instant::now();
    if now >= deadline {
        return Err(CheckStop::Deadline);
    }
    Ok(deadline - now)
}

fn sleep_cancellable_until(
    duration: Duration,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<(), CheckStop> {
    let wake = Instant::now() + duration;
    loop {
        let remaining = check_remaining(cancel, deadline)?;
        let now = Instant::now();
        if now >= wake {
            return Ok(());
        }
        let wait = wake.saturating_duration_since(now).min(remaining);
        if wait.is_zero() {
            return Ok(());
        }
        std::thread::sleep(wait.min(Duration::from_millis(50)));
    }
}

/// Read a robots body with an absolute deadline and cancellation. Reads are chunked so a
/// server that trickles bytes cannot hold the check open past the chain deadline.
fn read_robots_body(
    mut reader: impl Read,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<String, RobotsFailure> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        check_remaining(cancel, deadline).map_err(RobotsFailure::Stop)?;
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let room = (256usize * 1024).saturating_sub(bytes.len());
                if read > room {
                    bytes.extend_from_slice(&buffer[..room]);
                    return Err(RobotsFailure::Unavailable(
                        "robots.txt exceeds the 256 KiB policy limit; the policy is unknown".into(),
                    ));
                }
                bytes.extend_from_slice(&buffer[..read]);
                if read == 0 || bytes.len() == 256 * 1024 {
                    // The next read tells us whether a complete policy ended exactly at the cap.
                    continue;
                }
            }
            Err(error) => {
                if let Err(stop) = check_remaining(cancel, deadline) {
                    return Err(RobotsFailure::Stop(stop));
                }
                return Err(RobotsFailure::Unavailable(format!(
                    "robots.txt read failed: {error}"
                )));
            }
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn check_robots_origin(
    origin: &str,
    agent: &ureq::Agent,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Robots>, RobotsFailure> {
    let mut url = format!("{origin}/robots.txt");
    for _ in 0..=links::MAX_REDIRECT_HOPS {
        let remaining = check_remaining(cancel, deadline).map_err(RobotsFailure::Stop)?;
        let response = match agent
            .get(&url)
            .set("Accept-Encoding", "identity")
            .timeout(remaining.min(Duration::from_secs(10)))
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(status, _)) if (400..500).contains(&status) => {
                return Ok(None);
            }
            Err(ureq::Error::Status(status, _)) => {
                return Err(RobotsFailure::Unavailable(format!(
                    "robots.txt returned HTTP {status}"
                )));
            }
            Err(error) => {
                return Err(RobotsFailure::Unavailable(format!(
                    "robots.txt could not be read: {error}"
                )));
            }
        };
        let status = response.status();
        if (300..400).contains(&status) {
            let Some(location) = response.header("location") else {
                return Err(RobotsFailure::Unavailable(format!(
                    "robots.txt redirected without a location ({status})"
                )));
            };
            let base =
                Url::parse(&url).map_err(|error| RobotsFailure::Unavailable(error.to_string()))?;
            let next = base
                .join(location)
                .map_err(|error| RobotsFailure::Unavailable(error.to_string()))?;
            if next.scheme() != "http" && next.scheme() != "https" {
                return Err(RobotsFailure::Unavailable(
                    "robots.txt redirected outside HTTP(S)".into(),
                ));
            }
            url = next.to_string();
            continue;
        }
        if status == 200 {
            let body = read_robots_body(response.into_reader(), cancel, deadline)?;
            return Ok(Some(Robots::parse(&body, USER_AGENT)));
        }
        if (400..500).contains(&status) {
            return Ok(None);
        }
        return Err(RobotsFailure::Unavailable(format!(
            "robots.txt returned HTTP {status}"
        )));
    }
    Err(RobotsFailure::Unavailable(
        "robots.txt exceeded the redirect limit".into(),
    ))
}

fn checked_with_retry_after(
    status: u16,
    final_url: String,
    chain: Vec<String>,
    retry_after: Option<&str>,
) -> LinkCheckResult {
    let mut result = LinkCheckResult::checked(status, final_url, chain);
    if let Some(retry_after) = retry_after.filter(|value| !value.trim().is_empty()) {
        result.reason = format!(
            "HTTP {status}; Retry-After: {} (not retried)",
            retry_after.trim()
        );
    }
    result
}

fn retry_after_delay(value: Option<&str>, now: std::time::SystemTime) -> Option<Duration> {
    let value = value?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|date| date.duration_since(now).unwrap_or_default())
}

#[derive(Clone, Copy)]
enum RetryCooldown {
    Until(Instant),
    /// A server delay beyond the monotonic clock's range must never become permission.
    Unrepresentable,
}

fn record_retry_cooldown(
    cooldowns: &mut HashMap<String, RetryCooldown>,
    origin: &str,
    header: Option<&str>,
) {
    if let Some(delay) = retry_after_delay(header, std::time::SystemTime::now()) {
        let cooldown = Instant::now()
            .checked_add(delay)
            .map_or(RetryCooldown::Unrepresentable, RetryCooldown::Until);
        cooldowns.insert(origin.to_string(), cooldown);
    }
}

fn cache_actual_response(
    checked: &mut FastMap<String, LinkCheckResult>,
    url: &Url,
    result: &LinkCheckResult,
) {
    if result.status == 0 {
        return;
    }
    // Every requested redirect destination is reusable, including intermediate redirects.
    // Its cached chain starts after that destination, so later sources retain only their own
    // prefix followed by the shared suffix. A terminal response has an empty suffix.
    for (index, destination) in result.redirect_chain.iter().enumerate() {
        if let Ok(destination) = Url::parse(destination) {
            let mut cached = result.clone();
            cached.redirect_chain = result.redirect_chain[index + 1..].to_vec();
            checked.insert(url_key(destination), cached);
        }
    }
    let mut cached = result.clone();
    cached.redirect_chain.clear();
    checked.insert(url_key(url.clone()), cached);
}

#[allow(clippy::too_many_arguments)]
fn optional_check_result(
    key: &str,
    known_pages: &FastMap<String, LinkCheckResult>,
    checked: &mut FastMap<String, LinkCheckResult>,
    optional_checks: &FastSet<String>,
    internal_host: &str,
    agent: &ureq::Agent,
    robots_cache: &mut HashMap<String, RobotsPolicy>,
    last_start: &mut HashMap<String, Instant>,
    retry_after_until: &mut HashMap<String, RetryCooldown>,
    cancel: &AtomicBool,
    budget: &mut usize,
) -> LinkCheckResult {
    if let Some(known) = known_pages.get(key) {
        return known.clone();
    }
    if let Some(done) = checked.get(key) {
        return done.clone();
    }
    if *budget == 0 {
        return LinkCheckResult::limited("Optional check budget exhausted");
    }
    let Ok(mut current) = Url::parse(key) else {
        return LinkCheckResult {
            state: LinkCheckState::Invalid,
            reason: "Invalid destination URL".into(),
            ..LinkCheckResult::default()
        };
    };
    if !current.username().is_empty() || current.password().is_some() {
        return LinkCheckResult::limited("Embedded credentials are not checked");
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    let mut chain = Vec::new();
    let mut visited: FastSet<String> = fast_set(8);
    visited.insert(url_key(current.clone()));
    let mut final_url;
    for _ in 0..=links::MAX_REDIRECT_HOPS {
        if let Err(stop) = check_remaining(cancel, deadline) {
            return stop.limited();
        }
        let origin = current.origin().ascii_serialization();
        if !robots_cache.contains_key(&origin) {
            let policy = match check_robots_origin(&origin, agent, cancel, deadline) {
                Ok(policy) => RobotsPolicy::Known(policy),
                Err(RobotsFailure::Stop(stop)) => return stop.limited(),
                Err(RobotsFailure::Unavailable(reason)) => {
                    robots_cache.insert(origin.clone(), RobotsPolicy::Unavailable(reason.clone()));
                    return LinkCheckResult::blocked(format!(
                        "robots.txt could not be read: {reason}"
                    ));
                }
            };
            robots_cache.insert(origin.clone(), policy);
        }
        match robots_cache.get(&origin) {
            Some(RobotsPolicy::Unavailable(reason)) => {
                return LinkCheckResult::blocked(format!("robots.txt could not be read: {reason}"));
            }
            Some(RobotsPolicy::Known(Some(robots))) if !robots.allows(&request_path(&current)) => {
                return LinkCheckResult::blocked("robots.txt disallows this URL");
            }
            _ => {}
        }
        let delay = match robots_cache.get(&origin) {
            Some(RobotsPolicy::Known(Some(robots))) => robots.crawl_delay(),
            _ => None,
        }
        .unwrap_or_default()
        .max(Duration::from_secs(1));
        let now = Instant::now();
        let delay_until = last_start
            .get(&origin)
            .map(|last| last.checked_add(delay).unwrap_or(deadline))
            .unwrap_or(now);
        let cooldown_until = match retry_after_until.get(&origin) {
            Some(RetryCooldown::Until(until)) => *until,
            Some(RetryCooldown::Unrepresentable) => {
                return LinkCheckResult::limited(
                    "Retry-After exceeds the supported time range; destination not requested",
                );
            }
            None => now,
        };
        let wait_until = delay_until.max(cooldown_until);
        if wait_until >= deadline {
            return LinkCheckResult::limited(
                "Origin cooldown exceeds the 30 second check deadline; destination not requested",
            );
        }
        if wait_until > now
            && let Err(stop) = sleep_cancellable_until(wait_until - now, cancel, deadline)
        {
            return stop.limited();
        }
        last_start.insert(origin.clone(), Instant::now());
        if *budget == 0 {
            return LinkCheckResult::limited("Optional check budget exhausted");
        }
        let remaining = match check_remaining(cancel, deadline) {
            Ok(remaining) => remaining,
            Err(stop) => return stop.limited(),
        };
        *budget -= 1;
        let response = match agent
            .get(current.as_str())
            .set("Accept-Encoding", "identity")
            .timeout(remaining.min(Duration::from_secs(10)))
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) if (300..400).contains(&status) => response,
            Err(ureq::Error::Status(status, response)) => {
                let retry_header = response.header("retry-after");
                if matches!(status, 429 | 503) {
                    record_retry_cooldown(retry_after_until, &origin, retry_header);
                }
                let result =
                    checked_with_retry_after(status, current.to_string(), chain, retry_header);
                cache_actual_response(checked, &current, &result);
                return result;
            }
            Err(error) => {
                if let Err(stop) = check_remaining(cancel, deadline) {
                    return stop.limited();
                }
                return LinkCheckResult::unavailable(error.to_string());
            }
        };
        let status = response.status();
        let retry_header = response.header("retry-after");
        if matches!(status, 429 | 503) {
            record_retry_cooldown(retry_after_until, &origin, retry_header);
        }
        final_url = current.to_string();
        if (300..400).contains(&status) {
            let Some(location) = response.header("location") else {
                let result = LinkCheckResult {
                    state: LinkCheckState::Checked,
                    reason: "Redirect without a Location header".into(),
                    status,
                    final_url,
                    redirect_chain: chain,
                    limited: false,
                };
                cache_actual_response(checked, &current, &result);
                return result;
            };
            let Ok(next) = current.join(location) else {
                let result = LinkCheckResult::limited("Redirect target is not a valid URL");
                cache_actual_response(checked, &current, &result);
                return result;
            };
            if next.scheme() != "http" && next.scheme() != "https" {
                let result = LinkCheckResult::limited("Redirect target is not HTTP(S)");
                cache_actual_response(checked, &current, &result);
                return result;
            }
            if !next.username().is_empty() || next.password().is_some() {
                let result = LinkCheckResult::limited("Redirect target embeds credentials");
                cache_actual_response(checked, &current, &result);
                return result;
            }
            let next_key = url_key(next.clone());
            if !visited.insert(next_key.clone()) {
                return LinkCheckResult::limited("Redirect loop");
            }
            if same_host(&next, internal_host)
                && !known_pages.contains_key(&next_key)
                && !optional_checks.contains(&next_key)
            {
                return LinkCheckResult::limited(
                    "Redirect target is an internal page outside the page crawl",
                );
            }
            if let Some(done) = known_pages
                .get(&next_key)
                .or_else(|| checked.get(&next_key))
            {
                // Keep the chain that belongs to this source and append the cached
                // destination's own chain, including intermediate shared redirects.
                let mut reused_chain = chain.clone();
                reused_chain.push(next.to_string());
                reused_chain.extend(done.redirect_chain.iter().cloned());
                return LinkCheckResult {
                    state: done.state,
                    reason: done.reason.clone(),
                    status: done.status,
                    final_url: if done.final_url.is_empty() {
                        next_key
                    } else {
                        done.final_url.clone()
                    },
                    redirect_chain: reused_chain,
                    limited: done.limited,
                };
            }
            chain.push(next.to_string());
            current = next;
            continue;
        }
        let result = checked_with_retry_after(status, final_url, chain, retry_header);
        cache_actual_response(checked, &current, &result);
        return result;
    }
    LinkCheckResult::limited("Redirect hop limit reached")
}

fn apply_link_checks(
    pages: &mut [Page],
    known_pages: &FastMap<String, LinkCheckResult>,
    checked: &FastMap<String, LinkCheckResult>,
) {
    for page in pages {
        for occurrence in Arc::make_mut(&mut page.link_evidence).iter_mut() {
            if matches!(
                occurrence.check.as_ref().map(|check| check.state),
                Some(LinkCheckState::Blocked | LinkCheckState::Invalid | LinkCheckState::Fragment)
            ) {
                continue;
            }
            if let Some(result) = known_pages
                .get(&occurrence.key)
                .or_else(|| checked.get(&occurrence.key))
            {
                occurrence.check = Some(Box::new(result.clone()));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn crawl_engine(
    seed: &str,
    limits: Limits,
    fetch: impl Fn(&str) -> Fetched + Sync,
    fetch_sitemap: impl Fn(&str, &dyn Fn() -> bool, &dyn Fn(u64)) -> sitemap::FetchedBytes + Sync,
    cache_robots: bool,
    options: CrawlOptions,
    cancel: Option<&AtomicBool>,
    browser: Option<BrowserJob<'_>>,
    compiled_extraction: Option<extract::CompiledRules>,
    on_event: &mut dyn FnMut(CrawlEvent),
) -> CrawlResult {
    let no_cancel = AtomicBool::new(false);
    let cancel = cancel.unwrap_or(&no_cancel);
    let cancelled = || cancel.load(Ordering::Relaxed);
    let wants_extraction = !options.extraction_rules.is_empty();
    let seed = match normalize_seed(seed) {
        Ok(seed) => seed,
        Err(message) => {
            return CrawlResult {
                message,
                outcome: CrawlOutcome::Failed,
                ..CrawlResult::default()
            };
        }
    };
    let host = host_of(&seed).unwrap_or_default();

    if limits.max_pages == 0 {
        return CrawlResult {
            pages: Vec::new(),
            message: format!("0 pages on {host}"),
            ..CrawlResult::default()
        };
    }

    // robots.txt is fetched once per origin. A missing or unreadable file allows the crawl.
    let seed_url = Url::parse(&seed).ok();
    let robots = seed_url
        .as_ref()
        .and_then(|seed_url| robots_for(seed_url, &fetch, cache_robots));
    if let (Some(seed_url), Some(robots)) = (seed_url.as_ref(), robots.as_ref())
        && !robots.allows(&request_path(seed_url))
    {
        return CrawlResult {
            pages: Vec::new(),
            message: format!("Blocked by robots.txt · {host}"),
            outcome: CrawlOutcome::Blocked,
            ..CrawlResult::default()
        };
    }

    // Reserve a modest starting amount. The user can set a large page limit, so
    // allocating the full crawl budget before discovering any URLs is wasteful.
    let mut seen: FastSet<String> = fast_set(limits.max_pages.min(256).saturating_mul(4).max(16));
    seen.insert(seed.clone());
    let mut pages: Vec<Page> = Vec::with_capacity(limits.max_pages.min(256));
    let mut page_index: FastMap<String, usize> = fast_map(limits.max_pages.min(256));
    let mut committed = 1_usize;
    let mut over_cap = false;
    let mut stopped_early = false;
    let mut link_evidence_total = 0_usize;
    let mut link_evidence_limited = false;
    let mut extraction_budget = extract::MAX_RUN_VALUE_BYTES;
    let mut optional_checks: FastSet<String> = fast_set(64);

    let shared = Mutex::new(Work {
        queue: VecDeque::from([(seed, 0_u8)]),
        crawl_delay: robots.as_ref().and_then(Robots::crawl_delay),
        ..Work::default()
    });
    let wake = Condvar::new();
    // Browser pages are far heavier than HTTP fetches, so Browser mode runs at most two at a
    // time. HTML mode keeps the wide pool that makes it fast.
    let workers = if browser.is_some() {
        limits.max_pages.min(render::MAX_CONCURRENCY)
    } else {
        limits.max_pages.min(PARALLEL_REQUESTS)
    };
    // A browser may only request documents that the crawl would itself schedule. This closure
    // is consulted before a navigation leaves the browser, so scripts cannot widen the crawl.
    let browser_allowed: Option<render::UrlFilter> = browser.as_ref().map(|_| {
        let host = host.clone();
        let robots = robots.clone();
        let allowed = move |url: &str| {
            let Ok(url) = Url::parse(url) else {
                return false;
            };
            same_host(&url, &host)
                && robots
                    .as_ref()
                    .is_none_or(|robots| robots.allows(&request_path(&url)))
        };
        Arc::new(allowed) as render::UrlFilter
    });
    let mut discovery_done = !options.sitemaps;
    let mut sitemap_report = sitemap::SitemapReport::default();
    sitemap_report.discovery_done = discovery_done;

    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<EngineEvent>();
        let content_budget = Arc::new(ContentBudget::default());

        for _ in 0..workers {
            let tx = tx.clone();
            let shared = &shared;
            let wake = &wake;
            let fetch = &fetch;
            let browser_allowed = browser_allowed.clone();
            let browser = browser.as_ref();
            let content_budget = content_budget.clone();
            scope.spawn(move || {
                loop {
                    let (url, depth) = {
                        let mut work = shared.lock().unwrap();
                        loop {
                            if work.done {
                                return;
                            }
                            if let Some(job) = work.queue.pop_front() {
                                break job;
                            }
                            work = wake.wait(work).unwrap();
                        }
                    };
                    if !reserve_request_slot(shared, wake, cancel) {
                        return;
                    }
                    // A panicking fetcher must still produce a result, or the crawl loop would
                    // wait forever for this URL.
                    let (fetched, parsed, source, capture, extraction) =
                        catch_unwind(AssertUnwindSafe(|| {
                            if let (Some(job), Some(allowed)) = (browser, browser_allowed.as_ref())
                            {
                                let capture =
                                    job.renderer
                                        .render(&url, allowed.clone(), job.cancel.clone());
                                let (fetched, parsed, source, capture_info) =
                                    browser_page(&capture, &content_budget, Some(cancel));
                                let applicable =
                                    capture.status == 200 && is_html(&capture.content_type, "");
                                let partial = !capture_info.is_complete();
                                (
                                    fetched,
                                    parsed,
                                    source,
                                    capture_info,
                                    ExtractionInput {
                                        html: wants_extraction
                                            .then(|| capture.rendered_html.clone())
                                            .flatten(),
                                        stage: EvidenceStage::Rendered,
                                        applicable,
                                        partial,
                                    },
                                )
                            } else {
                                let fetched = fetch(&url);
                                let complete = fetched.status == 200
                                    && fetched.error.is_empty()
                                    && is_html(&fetched.content_type, &fetched.body)
                                    && fetched.read == ReadState::Complete;
                                let parsed = if fetched.status == 200
                                    && is_html(&fetched.content_type, &fetched.body)
                                {
                                    Some(parse_html_staged(
                                        &fetched.body,
                                        EvidenceStage::Document,
                                        &content_budget,
                                        complete,
                                        Some(cancel),
                                    ))
                                } else {
                                    None
                                };
                                let partial = fetched.read != ReadState::Complete;
                                let extraction_html = wants_extraction
                                    .then(|| fetched.body.clone())
                                    .filter(|body| !body.is_empty());
                                let mut fetched = fetched;
                                // The body is no longer needed once parsed; dropping it here keeps
                                // large pages from crossing the channel into the crawl loop.
                                fetched.body = String::new();
                                (
                                    fetched,
                                    parsed,
                                    None,
                                    CaptureInfo::html(),
                                    ExtractionInput {
                                        html: extraction_html,
                                        stage: EvidenceStage::Document,
                                        applicable: true,
                                        partial,
                                    },
                                )
                            }
                        }))
                        .unwrap_or_else(|_| {
                            (
                                Fetched {
                                    error: "crawler worker panicked".into(),
                                    ..Fetched::default()
                                },
                                None,
                                None,
                                CaptureInfo::default(),
                                ExtractionInput {
                                    html: None,
                                    stage: EvidenceStage::Document,
                                    applicable: false,
                                    partial: true,
                                },
                            )
                        });
                    if fetched.status == 429 || fetched.status == 503 {
                        let wait =
                            Duration::from_millis(fetched.retry_after_ms.max(PAUSE_PUSHBACK_MS));
                        let mut work = shared.lock().unwrap();
                        let until = Instant::now() + wait;
                        if work.pause_until.is_none_or(|current| until > current) {
                            work.pause_until = Some(until);
                        }
                        drop(work);
                        wake.notify_all();
                    }
                    if tx
                        .send(EngineEvent::Page(Box::new(FetchedPage {
                            url,
                            depth,
                            fetched,
                            parsed,
                            source,
                            capture,
                            extraction,
                        })))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }

        if let (true, Some(seed_url)) = (options.sitemaps, seed_url.clone()) {
            let tx_sitemap = tx.clone();
            let shared_work = &shared;
            let wake_condvar = &wake;
            let fetch_sitemap: &SitemapFetcher<'_> = &fetch_sitemap;
            let declared = robots
                .as_ref()
                .map(|robots| robots.sitemaps.clone())
                .unwrap_or_default();
            let robots_for_discovery = robots.clone();
            scope.spawn(move || {
                let allows = |url: &Url| {
                    robots_for_discovery
                        .as_ref()
                        .is_none_or(|robots| robots.allows(&request_path(url)))
                };
                let before_request = || reserve_request_slot(shared_work, wake_condvar, cancel);
                let note_backoff = |milliseconds: u64| {
                    let mut work = shared_work
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let until = Instant::now() + Duration::from_millis(milliseconds);
                    if work.pause_until.is_none_or(|current| until > current) {
                        work.pause_until = Some(until);
                    }
                    drop(work);
                    wake_condvar.notify_all();
                };
                let fetch_with_delay =
                    |url: &str| fetch_sitemap(url, &before_request, &note_backoff);
                let mut send = |event: sitemap::DiscoveryEvent| {
                    let event = match event {
                        sitemap::DiscoveryEvent::File(file) => EngineEvent::SitemapFile(file),
                        sitemap::DiscoveryEvent::Url(url) => EngineEvent::SitemapUrl(url),
                        sitemap::DiscoveryEvent::Partial(reason) => {
                            EngineEvent::SitemapPartial(reason)
                        }
                        sitemap::DiscoveryEvent::Done => EngineEvent::SitemapDone,
                    };
                    let _ = tx_sitemap.send(event);
                };
                let panic_tx = tx_sitemap.clone();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    sitemap::discover(
                        &seed_url,
                        declared,
                        &allows,
                        &fetch_with_delay,
                        cancel,
                        &mut send,
                    )
                }));
                if result.is_err() {
                    let _ = panic_tx.send(EngineEvent::SitemapPartial(
                        "Sitemap discovery failed".into(),
                    ));
                    let _ = panic_tx.send(EngineEvent::SitemapDone);
                }
            });
        } else if options.sitemaps {
            discovery_done = true;
        }
        drop(tx);
        let _finish = Finish {
            shared: &shared,
            wake: &wake,
        };

        let mut pending = 1_usize;
        while pending > 0 || !discovery_done {
            if cancelled() {
                stopped_early = true;
                // The discovery thread's final events are not drained after this break, so
                // record the interruption here instead of letting the report read as complete.
                if !discovery_done {
                    sitemap_report.note_partial("Stopped");
                }
                break;
            }
            let event = match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    discovery_done = true;
                    break;
                }
            };
            let result = match event {
                EngineEvent::Page(result) => {
                    pending -= 1;
                    result
                }
                EngineEvent::SitemapFile(file) => {
                    sitemap_report.insert_file(file.clone());
                    on_event(CrawlEvent::SitemapFile(file));
                    continue;
                }
                EngineEvent::SitemapUrl(entry) => {
                    let is_new = sitemap_report.insert_url(entry.clone());
                    if is_new && entry.state == sitemap::UrlState::Pending {
                        if seen.insert(entry.url.clone()) {
                            if committed >= limits.max_pages {
                                over_cap = true;
                                if let Some(url) = sitemap_report.url_mut(&entry.url) {
                                    url.state = sitemap::UrlState::NotChecked;
                                    url.issue = "Page limit".into();
                                }
                            } else {
                                committed += 1;
                                {
                                    let mut work = shared.lock().unwrap();
                                    work.queue.push_back((entry.url.clone(), 0));
                                }
                                pending += 1;
                                wake.notify_all();
                            }
                        } else if let Some(index) = page_index.get(&entry.url).copied()
                            && let Some(page) = pages.get(index)
                        {
                            sitemap_report.observe_page(page);
                        }
                    }
                    let current = sitemap_report.url(&entry.url).cloned().unwrap_or(entry);
                    on_event(CrawlEvent::SitemapUrl(current));
                    continue;
                }
                EngineEvent::SitemapPartial(reason) => {
                    sitemap_report.note_partial(reason.clone());
                    on_event(CrawlEvent::SitemapPartial(reason));
                    continue;
                }
                EngineEvent::SitemapDone => {
                    discovery_done = true;
                    sitemap_report.discovery_done = true;
                    on_event(CrawlEvent::SitemapDone);
                    continue;
                }
            };
            let FetchedPage {
                url,
                depth,
                fetched,
                parsed,
                source,
                capture,
                extraction,
            } = *result;
            let page_url =
                if parsed.is_some() || source.is_some() || (300..400).contains(&fetched.status) {
                    Url::parse(&url).ok()
                } else {
                    None
                };
            let mut new_jobs: Vec<(String, u8)> = Vec::new();
            let mut page_links: Vec<String> = Vec::new();
            let mut canonicals: Vec<CanonicalDeclaration> = Vec::new();
            let mut link_evidence: Vec<LinkOccurrence> = Vec::new();
            let mut link_evidence_limit = String::new();

            // One policy path for every discovered link, whether it came from the rendered DOM
            // or from the source document when no rendered DOM exists. Scheduling and evidence
            // are separate: blocked, external and asset links remain evidence even when they are
            // not page jobs.
            let mut discover_links = |anchors: &[Anchor],
                                      link_base: &Url,
                                      scheduling: bool,
                                      stage: EvidenceStage,
                                      tail: &[LinkRef],
                                      evidence: &mut Vec<LinkOccurrence>,
                                      limit: &mut String| {
                for anchor in anchors {
                    let mut occurrence = LinkOccurrence {
                        source: url.clone(),
                        order: anchor.order,
                        raw_href: anchor.href.clone(),
                        anchor: anchor.text.clone(),
                        stage,
                        ..LinkOccurrence::default()
                    };
                    if anchor.overlong {
                        occurrence.resolved = anchor.href.clone();
                        occurrence.truncated = true;
                        occurrence.check = Some(Box::new(LinkCheckResult::limited(
                            "URL exceeds 8 KiB; not requested",
                        )));
                        occurrence.skip_reason = "URL exceeds 8 KiB; not requested".into();
                        retain_occurrence(evidence, occurrence, limit);
                        continue;
                    }
                    match resolve_evidence(link_base, &anchor.href) {
                        EvidenceHref::Invalid(reason) => {
                            occurrence.check = Some(Box::new(LinkCheckResult {
                                state: LinkCheckState::Invalid,
                                reason: reason.clone(),
                                ..LinkCheckResult::default()
                            }));
                            occurrence.skip_reason = reason;
                            retain_occurrence(evidence, occurrence, limit);
                            continue;
                        }
                        EvidenceHref::Fragment { url, key, fragment } => {
                            occurrence.truncated = url.as_str().len() > links::MAX_URL_BYTES;
                            occurrence.resolved =
                                links::truncate_bytes(url.as_str(), links::MAX_URL_BYTES);
                            occurrence.key = key;
                            occurrence.fragment = fragment;
                            occurrence.check = Some(Box::new(LinkCheckResult {
                                state: LinkCheckState::Fragment,
                                reason: "Same-document fragment; not fetched".into(),
                                ..LinkCheckResult::default()
                            }));
                            occurrence.skip_reason = "Same-document fragment".into();
                            retain_occurrence(evidence, occurrence, limit);
                            continue;
                        }
                        EvidenceHref::Url {
                            url: next,
                            key,
                            fragment,
                        } => {
                            occurrence.truncated = next.as_str().len() > links::MAX_URL_BYTES;
                            occurrence.resolved =
                                links::truncate_bytes(next.as_str(), links::MAX_URL_BYTES);
                            occurrence.key = key.clone();
                            occurrence.fragment = fragment;
                            let same = same_host(&next, &host);
                            let asset = looks_like_asset(&next);
                            let document = is_document_download(&next);
                            let web_asset = is_web_asset(&next);
                            let allowed = robots
                                .as_ref()
                                .is_none_or(|robots| robots.allows(&request_path(&next)));
                            let optional_candidate = options.check_links
                                && ((same && asset && document) || (!same && !web_asset));

                            if scheduling && same && !asset {
                                if !allowed {
                                    occurrence.skip_reason = "Blocked by robots.txt".into();
                                    occurrence.check = Some(Box::new(LinkCheckResult::blocked(
                                        "robots.txt disallows this URL",
                                    )));
                                } else {
                                    page_links.push(key.clone());
                                    if depth >= limits.max_depth {
                                        occurrence.skip_reason = "Not fetched (depth limit)".into();
                                        occurrence.check =
                                            Some(Box::new(LinkCheckResult::limited(
                                                "Depth limit prevented a fetch",
                                            )));
                                    } else if !seen.insert(key.clone()) {
                                        occurrence.skip_reason =
                                            "Already discovered in this crawl".into();
                                    } else if committed >= limits.max_pages {
                                        over_cap = true;
                                        occurrence.skip_reason = "Not fetched (page limit)".into();
                                        occurrence.check =
                                            Some(Box::new(LinkCheckResult::limited(
                                                "Page limit prevented a fetch",
                                            )));
                                    } else {
                                        committed += 1;
                                        new_jobs.push((key, depth + 1));
                                    }
                                }
                            } else if optional_candidate {
                                occurrence.skip_reason = "Queued for optional check".into();
                                optional_checks.insert(key.clone());
                            } else if same && asset {
                                occurrence.skip_reason = if document {
                                    "Document download not checked".into()
                                } else {
                                    "Asset not checked".into()
                                };
                            } else if !same {
                                occurrence.skip_reason = if web_asset {
                                    "External asset not checked".into()
                                } else if options.check_links {
                                    "Queued for optional check".into()
                                } else {
                                    "External link; checking is off".into()
                                };
                            } else if !scheduling {
                                occurrence.skip_reason =
                                    "Source evidence; rendered document supplied discovery".into();
                            }
                            retain_occurrence(evidence, occurrence, limit);
                        }
                    }
                }
                // The retained evidence list is bounded; discovery after the cap still
                // follows the same page scope, robots and budget rules.
                for link in tail {
                    if link.overlong {
                        continue;
                    }
                    let Some(next) = resolve(link_base, &link.href) else {
                        continue;
                    };
                    if !next.username().is_empty() || next.password().is_some() {
                        continue;
                    }
                    if !same_host(&next, &host) || looks_like_asset(&next) {
                        continue;
                    }
                    if robots
                        .as_ref()
                        .is_some_and(|robots| !robots.allows(&request_path(&next)))
                    {
                        continue;
                    }
                    let key = url_key(next);
                    page_links.push(key.clone());
                    if depth >= limits.max_depth || !seen.insert(key.clone()) {
                        continue;
                    }
                    if committed >= limits.max_pages {
                        over_cap = true;
                        continue;
                    }
                    committed += 1;
                    new_jobs.push((key, depth + 1));
                }
            };

            if let (Some(parsed), Some(page_url)) = (parsed.as_ref(), page_url.as_ref()) {
                let link_base = parsed
                    .base
                    .as_deref()
                    .and_then(|href| resolve(page_url, href))
                    .unwrap_or_else(|| page_url.clone());
                let stage = if capture.mode == CaptureMode::Browser {
                    EvidenceStage::Rendered
                } else {
                    EvidenceStage::Document
                };
                for raw in &parsed.canonicals {
                    canonicals.push(indexability::canonical_declaration_staged(
                        raw.as_deref(),
                        &link_base,
                        stage,
                    ));
                }
                discover_links(
                    &parsed.anchors,
                    &link_base,
                    true,
                    stage,
                    &parsed.links[parsed.anchors.len()..],
                    &mut link_evidence,
                    &mut link_evidence_limit,
                );
                if parsed.anchors_truncated {
                    link_evidence_limit = "Page link evidence limited to 10,000 occurrences".into();
                }
            }

            if let (Some(source), Some(page_url)) = (source.as_ref(), page_url.as_ref()) {
                let source_base = source
                    .base
                    .as_deref()
                    .and_then(|href| resolve(page_url, href))
                    .unwrap_or_else(|| page_url.clone());
                let mut source_canonicals = Vec::new();
                for raw in &source.canonicals {
                    source_canonicals.push(indexability::canonical_declaration_staged(
                        raw.as_deref(),
                        &source_base,
                        EvidenceStage::Document,
                    ));
                }
                indexability::merge_canonicals(&mut canonicals, source_canonicals);
                // Without a rendered document the source is the only document that can supply
                // discovered links. Those links are still not a completed content audit.
                let source_schedules = parsed.is_none();
                let source_tail: &[LinkRef] = if source_schedules {
                    &source.links[source.anchors.len()..]
                } else {
                    &[]
                };
                discover_links(
                    &source.anchors,
                    &source_base,
                    source_schedules,
                    EvidenceStage::Document,
                    source_tail,
                    &mut link_evidence,
                    &mut link_evidence_limit,
                );
                if source.anchors_truncated && link_evidence_limit.is_empty() {
                    link_evidence_limit = "Page link evidence limited to 10,000 occurrences".into();
                }
            }
            drop(discover_links);

            let remaining = links::MAX_RUN_OCCURRENCES.saturating_sub(link_evidence_total);
            if link_evidence.len() > remaining {
                link_evidence.truncate(remaining);
                link_evidence_limit = "Run link evidence limited to 250,000 occurrences".into();
                link_evidence_limited = true;
            }
            link_evidence_total += link_evidence.len();

            let redirect = if (300..400).contains(&fetched.status) {
                page_url
                    .as_ref()
                    .and_then(|page| resolve(page, &fetched.location))
            } else {
                None
            };
            let redirect_to = redirect
                .as_ref()
                .map(|next| url_key(next.clone()))
                .unwrap_or_default();
            if let Some(next) = redirect
                && same_host(&next, &host)
                && robots
                    .as_ref()
                    .is_none_or(|robots| robots.allows(&request_path(&next)))
                && seen.insert(redirect_to.clone())
            {
                if committed < limits.max_pages {
                    committed += 1;
                    new_jobs.push((redirect_to.clone(), depth));
                } else {
                    over_cap = true;
                }
            }

            // A script that moved the document to a new path or query is a destination, not a
            // fact about this URL. Admit it through the same scope, robots and budget path as a
            // discovered link, at the same depth as a replacement navigation.
            let navigation = capture
                .render
                .as_ref()
                .map(|render| render.navigation_to.as_str())
                .filter(|to| !to.is_empty())
                .and_then(|to| Url::parse(to).ok());
            if let Some(next) = navigation
                && same_host(&next, &host)
                && !looks_like_asset(&next)
                && robots
                    .as_ref()
                    .is_none_or(|robots| robots.allows(&request_path(&next)))
            {
                let key = url_key(next);
                if seen.insert(key.clone()) {
                    if committed < limits.max_pages {
                        committed += 1;
                        new_jobs.push((key, depth));
                    } else {
                        over_cap = true;
                    }
                }
            }

            let content_stage = if capture.mode == crate::indexability::CaptureMode::Browser {
                EvidenceStage::Rendered
            } else {
                EvidenceStage::Document
            };
            let content = parsed
                .as_ref()
                .map(|parsed| Arc::new(parsed.content.clone()))
                .unwrap_or_else(|| {
                    Arc::new(ContentEvidence::ineligible(
                        if content_stage == EvidenceStage::Rendered {
                            "Rendered DOM unavailable; source fallback is not used for content comparison"
                        } else {
                            "No complete HTML document"
                        },
                        content_stage,
                    ))
                });
            let had_html = parsed.is_some();
            let (title, meta_description, h1s, words, images, images_missing_alt, mut directives) =
                match parsed {
                    Some(parsed) => (
                        parsed.title,
                        parsed.meta,
                        parsed.h1s,
                        parsed.words,
                        parsed.images,
                        parsed.images_missing_alt,
                        parsed.robots_directives,
                    ),
                    None => (
                        String::new(),
                        String::new(),
                        Vec::new(),
                        0,
                        0,
                        0,
                        Vec::new(),
                    ),
                };
            if let Some(source) = source {
                indexability::merge_directives(&mut directives, source.robots_directives);
            }
            directives.extend(fetched.header_directives);
            let content_type = if fetched.content_type.is_empty() && had_html {
                "text/html".into()
            } else {
                fetched.content_type
            };
            let evidence = PageEvidence {
                directives,
                canonicals,
                read: fetched.read,
                truncated: fetched.header_truncated,
                omitted: fetched.header_omitted,
            };
            let indexability = indexability::assess(
                &url,
                fetched.status,
                &content_type,
                &redirect_to,
                &fetched.error,
                evidence,
                capture,
            );
            let extraction_results =
                compiled_extraction
                    .as_ref()
                    .map_or_else(Vec::new, |compiled| {
                        extract::evaluate(
                            compiled,
                            extraction.html.as_deref(),
                            extraction.stage,
                            extraction.applicable,
                            extraction.partial,
                            &mut extraction_budget,
                        )
                    });
            let extraction_limit = if extraction_results.iter().any(|outcome| outcome.limited) {
                "Extraction stopped at a value cap".to_string()
            } else {
                String::new()
            };
            let page = Page {
                url,
                status: fetched.status,
                content_type,
                title,
                meta_description,
                h1s,
                noindex: indexability.has_noindex(),
                words,
                redirect_to,
                error: fetched.error,
                depth,
                canonical: indexability
                    .first_valid_canonical()
                    .unwrap_or_default()
                    .to_string(),
                images,
                images_missing_alt,
                links: Arc::new(page_links),
                link_evidence: Arc::new(link_evidence),
                link_evidence_limit,
                extraction: Arc::new(extraction_results),
                extraction_limit,
                content,
                indexability,
            };
            sitemap_report.observe_page(&page);
            on_event(CrawlEvent::Page(Box::new(page.clone())));
            page_index.insert(page.url.clone(), pages.len());
            pages.push(page);

            if !new_jobs.is_empty() {
                let added = new_jobs.len();
                {
                    let mut work = shared.lock().unwrap();
                    work.queue.extend(new_jobs);
                }
                pending += added;
                wake.notify_all();
            }
        }
    });

    let mut known_pages: FastMap<String, LinkCheckResult> = fast_map(pages.len());
    for page in &pages {
        let result = if page.status == 0 {
            LinkCheckResult::unavailable(if page.error.is_empty() {
                "Page crawl could not fetch this URL".to_string()
            } else {
                page.error.clone()
            })
        } else {
            LinkCheckResult::checked(page.status, page.url.clone(), Vec::new())
        };
        known_pages.insert(page.url.clone(), result);
    }
    let mut checked: FastMap<String, LinkCheckResult> = fast_map(optional_checks.len());
    let mut check_limited = false;
    let mut check_stopped = false;
    if options.check_links && !optional_checks.is_empty() && !cancelled() {
        let mut destinations: Vec<String> = optional_checks
            .iter()
            .filter(|key| !known_pages.contains_key(*key))
            .cloned()
            .collect();
        destinations.sort();
        if !destinations.is_empty() {
            on_event(CrawlEvent::LinkCheckStarted(destinations.len()));
            let agent = check_agent();
            let mut robots_cache: HashMap<String, RobotsPolicy> = HashMap::new();
            let mut last_start: HashMap<String, Instant> = HashMap::new();
            let mut retry_after_until: HashMap<String, RetryCooldown> = HashMap::new();
            let mut budget = links::MAX_CHECKED_DESTINATIONS;
            for key in destinations {
                if cancelled() {
                    check_stopped = true;
                    break;
                }
                let result = optional_check_result(
                    &key,
                    &known_pages,
                    &mut checked,
                    &optional_checks,
                    &host,
                    &agent,
                    &mut robots_cache,
                    &mut last_start,
                    &mut retry_after_until,
                    cancel,
                    &mut budget,
                );
                if result.limited {
                    check_limited = true;
                }
                checked.insert(key.clone(), result.clone());
                on_event(CrawlEvent::LinkCheck(key, result));
                if cancelled() {
                    check_stopped = true;
                    break;
                }
            }
        }
    }
    if cancelled() {
        check_stopped = true;
    }
    if check_stopped {
        stopped_early = true;
    }
    apply_link_checks(&mut pages, &known_pages, &checked);

    let mut message = if stopped_early {
        format!("Stopped · {} pages on {host}", pages.len())
    } else {
        format!("{} pages on {host}", pages.len())
    };
    let mut outcome = if stopped_early {
        CrawlOutcome::Stopped
    } else {
        CrawlOutcome::Completed
    };
    if over_cap && !stopped_early {
        message.push_str(&format!(
            " · stopped at the {} page limit",
            limits.max_pages
        ));
        outcome = CrawlOutcome::Limited;
    }
    if check_limited && !stopped_early {
        message.push_str(" · link checks incomplete");
    }
    if !stopped_early && pages.iter().all(|page| page.status == 0) {
        let why = pages
            .first()
            .map(|page| page.error.as_str())
            .filter(|e| !e.is_empty())
            .unwrap_or("no response");
        message = format!("Crawl failed · {why}");
        outcome = CrawlOutcome::Failed;
    }
    sitemap_report.discovery_done = true;
    if stopped_early {
        sitemap_report.mark_not_checked("Stopped");
    } else if over_cap {
        sitemap_report.mark_not_checked("Page limit");
    } else {
        sitemap_report.mark_not_checked("Not fetched");
    }
    CrawlResult {
        pages,
        message,
        sitemap: sitemap_report,
        outcome,
        setup_failure: None,
        link_evidence_limited,
    }
}

#[derive(Clone, Debug)]
struct LinkRef {
    /// The literal href when it fits the evidence bound; a bounded display form with
    /// `overlong` set otherwise.
    href: String,
    overlong: bool,
}

#[derive(Clone, Debug)]
struct Anchor {
    href: String,
    overlong: bool,
    text: String,
    order: u32,
}

struct Parsed {
    title: String,
    meta: String,
    h1s: Vec<String>,
    words: usize,
    /// Every anchor href in document order, independent of the evidence cap.
    links: Vec<LinkRef>,
    anchors: Vec<Anchor>,
    anchors_truncated: bool,
    /// `<base href>`, resolved by the crawler against the page URL before joining links.
    base: Option<String>,
    /// Every robots meta declaration, in document order, including agent scopes.
    robots_directives: Vec<RobotsDirective>,
    /// Every canonical href, in document order. `None` means the element had no href.
    canonicals: Vec<Option<String>>,
    images: usize,
    images_missing_alt: usize,
    content: ContentEvidence,
}

/// The counts the audits read out of a parsed document.
///
/// Exposed only so benchmarks and the profiling example can time parsing on its own.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParseStats {
    pub title_len: usize,
    pub meta_len: usize,
    pub h1s: usize,
    pub links: usize,
    pub words: usize,
    pub images: usize,
}

/// Parse one HTML document and return the counts the audits use. Not part of the stable API.
#[doc(hidden)]
pub fn parse_stats(body: &str) -> ParseStats {
    let parsed = parse_html(body);
    ParseStats {
        title_len: parsed.title.len(),
        meta_len: parsed.meta.len(),
        h1s: parsed.h1s.len(),
        links: parsed.links.len(),
        words: parsed.words,
        images: parsed.images,
    }
}

/// Elements whose text counts toward the page's word count.
fn is_word_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1" | "h2" | "h3" | "h4" | "li" | "td" | "th" | "blockquote" | "figcaption"
    )
}

/// Parse a document in one tree walk.
///
/// The previous version ran a separate selector match for the title, meta tags, H1s, links,
/// base, canonical, images and word blocks — eight full traversals of the DOM per page. The
/// tree is immutable, so everything can be collected while visiting each node once.
/// Turn one browser capture into the fetch result, parses and provenance the crawl uses.
///
/// The rendered DOM is the only document that supplies audited content. The pre-script source is
/// parsed for evidence that scripts may have removed, and is used for link discovery only when no
/// rendered DOM exists. An incomplete capture therefore keeps its source evidence without
/// pretending the source was a completed rendered audit.
fn browser_page(
    capture: &Capture,
    content_budget: &ContentBudget,
    cancel: Option<&AtomicBool>,
) -> (Fetched, Option<Parsed>, Option<Parsed>, CaptureInfo) {
    let mut fetched = Fetched {
        status: capture.status,
        content_type: capture.content_type.clone(),
        location: capture.location.clone(),
        ..Fetched::default()
    };
    let header_values: Vec<&str> = capture
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-robots-tag"))
        .map(|(_, value)| value.as_str())
        .collect();
    let (directives, truncated, omitted) = indexability::parse_x_robots_tag(&header_values);
    fetched.header_directives = directives;
    fetched.header_truncated = truncated;
    fetched.header_omitted = omitted;
    if capture.state == RenderState::Failed {
        fetched.error = capture.reason.clone();
    }

    let mut parsed = None;
    let mut source_parsed = None;
    if capture.status == 200 && is_html(&capture.content_type, "") {
        let capture_info = CaptureInfo::browser(capture);
        let complete = capture_info.is_complete();
        if let Some(body) = capture.rendered_html.as_deref() {
            parsed = Some(parse_html_staged(
                body,
                EvidenceStage::Rendered,
                content_budget,
                complete,
                cancel,
            ));
        }
        if let Some(body) = capture.source_html.as_deref() {
            source_parsed = Some(parse_html_staged(
                body,
                EvidenceStage::Document,
                content_budget,
                false,
                cancel,
            ));
        }
        // Content completeness follows the rendered DOM. Source-only captures remain available
        // for evidence inspection but are not completed content audits.
        fetched.read = if capture.rendered_html.is_some() {
            if capture.rendered_truncated {
                ReadState::Truncated
            } else {
                ReadState::Complete
            }
        } else {
            match capture.source_state {
                SourceState::Captured => ReadState::Complete,
                SourceState::Truncated => ReadState::Truncated,
                SourceState::TooLarge | SourceState::Unavailable | SourceState::NotApplicable => {
                    ReadState::NotRead
                }
            }
        };
    }
    (
        fetched,
        parsed,
        source_parsed,
        CaptureInfo::browser(capture),
    )
}

/// Normalized descendant text for an anchor, excluding non-content elements. An image-only
/// anchor intentionally has an empty label: alt text is not an accessibility name here.
fn anchor_text(element: ElementRef<'_>) -> String {
    let mut text = String::new();
    for node in element.descendants() {
        let Node::Text(value) = node.value() else {
            continue;
        };
        let excluded = node.ancestors().skip(1).any(|ancestor| {
            ancestor
                .value()
                .as_element()
                .is_some_and(|element| matches!(element.name(), "script" | "style" | "template"))
        });
        if excluded {
            continue;
        }
        for word in value.text.split_whitespace() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(word);
        }
    }
    text
}

fn parse_html(body: &str) -> Parsed {
    // Parsing-only callers (tests and the benchmark gate) do not need content evidence.
    // Real crawl workers use `parse_html_staged` with the capture-completeness decision.
    parse_html_staged(
        body,
        EvidenceStage::Document,
        &ContentBudget::default(),
        false,
        None,
    )
}

fn parse_html_staged(
    body: &str,
    stage: EvidenceStage,
    content_budget: &ContentBudget,
    content_complete: bool,
    content_cancel: Option<&AtomicBool>,
) -> Parsed {
    let doc = Html::parse_document(body);

    let mut title: Option<String> = None;
    let mut meta = String::new();
    let mut saw_description = false;
    let mut h1s = Vec::new();
    let mut links: Vec<LinkRef> = Vec::new();
    let mut anchors = Vec::new();
    let mut anchors_truncated = false;
    let mut base: Option<String> = None;
    let mut robots_directives: Vec<RobotsDirective> = Vec::new();
    let mut canonicals: Vec<Option<String>> = Vec::new();
    let mut images = 0_usize;
    let mut images_missing_alt = 0_usize;
    let mut words = 0_usize;

    for node in doc.tree.nodes() {
        match node.value() {
            Node::Element(element) => match element.name() {
                "title" => {
                    if title.is_none() {
                        title = ElementRef::wrap(node).map(|element| text_of(element.text()));
                    }
                }
                "meta" => {
                    let name = element.attr("name").unwrap_or("");
                    if !saw_description && name.eq_ignore_ascii_case("description") {
                        saw_description = true;
                        meta = element.attr("content").map(clean).unwrap_or_default();
                    }
                    if let Some(directive) = element.attr("content").and_then(|content| {
                        indexability::parse_meta_directive_staged(name, content, stage)
                    }) {
                        robots_directives.push(directive);
                    }
                }
                "h1" => {
                    if let Some(element) = ElementRef::wrap(node) {
                        let text = text_of(element.text());
                        if !text.is_empty() {
                            h1s.push(text);
                        }
                    }
                }
                "a" => {
                    if let Some(raw) = element.attr("href") {
                        // An overlong href is declined explicitly. It is never truncated into a
                        // different destination and then requested as if it were the real one.
                        let overlong = raw.len() > links::MAX_URL_BYTES;
                        let href = if overlong {
                            links::truncate_bytes(raw, links::MAX_URL_BYTES)
                        } else {
                            raw.to_string()
                        };
                        let text = ElementRef::wrap(node).map(anchor_text).unwrap_or_default();
                        let order = links.len() as u32;
                        links.push(LinkRef {
                            href: href.clone(),
                            overlong,
                        });
                        if anchors.len() < links::MAX_PAGE_OCCURRENCES {
                            anchors.push(Anchor {
                                href,
                                overlong,
                                text: links::truncate_chars(&text, links::MAX_ANCHOR_CHARS),
                                order,
                            });
                        } else {
                            anchors_truncated = true;
                        }
                    }
                }
                "base" => {
                    if base.is_none() {
                        base = element.attr("href").map(str::to_string);
                    }
                }
                "link" => {
                    if element.attr("rel").is_some_and(|rel| {
                        rel.split_whitespace()
                            .any(|token| token.eq_ignore_ascii_case("canonical"))
                    }) {
                        canonicals.push(element.attr("href").map(str::to_string));
                    }
                }
                "img" => {
                    images += 1;
                    if element.attr("alt").is_none_or(|alt| alt.trim().is_empty()) {
                        images_missing_alt += 1;
                    }
                }
                _ => {}
            },
            Node::Text(text) => {
                // Count words once per text node, and only when it sits inside a block that
                // the word count has always covered. Nested blocks are no longer counted twice.
                let count = text.text.split_whitespace().count();
                let in_block = count > 0
                    && node.ancestors().any(|ancestor| {
                        ancestor
                            .value()
                            .as_element()
                            .is_some_and(|element| is_word_block(element.name()))
                    });
                if in_block {
                    words += count;
                }
            }
            _ => {}
        }
    }

    let content = content::extract_with_cancel(
        &doc,
        stage,
        content_complete,
        content_budget,
        content_cancel,
    );
    Parsed {
        title: title.unwrap_or_default(),
        meta,
        h1s,
        words,
        links,
        anchors,
        anchors_truncated,
        base,
        robots_directives,
        canonicals,
        images,
        images_missing_alt,
        content,
    }
}

fn fetch(agent: &ureq::Agent, url: &str) -> Fetched {
    let mut attempt = 1;
    loop {
        let fetched = fetch_once(agent, url);
        if attempt >= FETCH_ATTEMPTS || !retryable(&fetched) {
            return fetched;
        }
        // 250ms, then 750ms. A server that sent `Retry-After` gets the longer of the two.
        let wait = fetched.retry_after_ms.max(retry_wait(attempt));
        std::thread::sleep(Duration::from_millis(wait));
        attempt += 1;
    }
}

fn fetch_once(agent: &ureq::Agent, url: &str) -> Fetched {
    match agent
        .get(url)
        .set("Accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1")
        .call()
    {
        Ok(response) => from_response(response.status(), response),
        Err(ureq::Error::Status(code, response)) => from_response(code, response),
        Err(error) => Fetched {
            error: error.to_string(),
            ..Fetched::default()
        },
    }
}

fn retryable(fetched: &Fetched) -> bool {
    matches!(fetched.status, 0 | 429 | 500 | 502 | 503 | 504)
}

fn retry_wait(attempt: usize) -> u64 {
    (250 * 3_u64.pow((attempt.saturating_sub(1)).min(3) as u32)).min(2_000)
}

fn from_response(status: u16, response: ureq::Response) -> Fetched {
    let content_type = response.header("content-type").unwrap_or("").to_string();
    let location = response.header("location").unwrap_or("").to_string();
    let retry_after_ms = response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000).min(10_000))
        .unwrap_or(0);
    let (header_directives, header_truncated, header_omitted) =
        indexability::parse_x_robots_tag(&response.all("x-robots-tag"));
    let mut body = String::new();
    let mut read = ReadState::NotRead;
    // Only a 200 is parsed, so downloading the body of a redirect or an error page is wasted
    // bandwidth and time.
    if status == 200 && wants_body(&content_type) {
        let capacity = response
            .header("content-length")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0)
            .min(MAX_BODY_BYTES as usize);
        let mut bytes = Vec::with_capacity(capacity);
        // Read one byte past the cap so a body that exactly fills it is still complete.
        match response
            .into_reader()
            .take(MAX_BODY_BYTES + 1)
            .read_to_end(&mut bytes)
        {
            Ok(_) => {
                if bytes.len() as u64 > MAX_BODY_BYTES {
                    bytes.truncate(MAX_BODY_BYTES as usize);
                    read = ReadState::Truncated;
                } else {
                    read = ReadState::Complete;
                }
            }
            Err(_) => read = ReadState::Failed,
        }
        body = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        };
    } else if status != 200 {
        // A redirect or an error page still has a body on many servers. Consume a little of it
        // so ureq can return the socket to the pool and the next request skips a handshake.
        let _ = std::io::copy(
            &mut response.into_reader().take(DRAIN_BYTES),
            &mut std::io::sink(),
        );
    }
    Fetched {
        status,
        content_type,
        location,
        body,
        error: String::new(),
        retry_after_ms,
        header_directives,
        header_truncated,
        header_omitted,
        read,
    }
}

fn wants_body(content_type: &str) -> bool {
    content_type.is_empty()
        || contains_ascii_ci(content_type, "html")
        || contains_ascii_ci(content_type, "text/plain")
}

/// Fetch a sitemap document as bounded bytes. Kept separate from the HTML path because
/// `Fetched` stores text and the crawler must not parse `.gz` bytes as a page.
fn fetch_sitemap(
    agent: &ureq::Agent,
    url: &str,
    cancel: Option<&AtomicBool>,
    before_attempt: &dyn Fn() -> bool,
    note_backoff: &dyn Fn(u64),
) -> sitemap::FetchedBytes {
    let mut attempt = 1;
    loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) || !before_attempt() {
            return sitemap::FetchedBytes {
                error: "Stopped".into(),
                ..sitemap::FetchedBytes::default()
            };
        }
        let fetched = fetch_sitemap_once(agent, url, cancel);
        if matches!(fetched.status, 429 | 503) {
            note_backoff(fetched.retry_after_ms.max(PAUSE_PUSHBACK_MS));
        }
        if attempt >= FETCH_ATTEMPTS || !matches!(fetched.status, 0 | 429 | 500 | 502 | 503 | 504) {
            return fetched;
        }
        let wait = fetched.retry_after_ms.max(retry_wait(attempt));
        if !sleep_cancellable(Duration::from_millis(wait), cancel) {
            return sitemap::FetchedBytes {
                error: "Stopped".into(),
                ..sitemap::FetchedBytes::default()
            };
        }
        attempt += 1;
    }
}

fn fetch_sitemap_once(
    agent: &ureq::Agent,
    url: &str,
    cancel: Option<&AtomicBool>,
) -> sitemap::FetchedBytes {
    match agent
        .get(url)
        .set(
            "Accept",
            "application/xml,text/xml,application/gzip,*/*;q=0.1",
        )
        .call()
    {
        Ok(response) => from_sitemap_response(response.status(), response, cancel),
        Err(ureq::Error::Status(code, response)) => from_sitemap_response(code, response, cancel),
        Err(error) => sitemap::FetchedBytes {
            error: error.to_string(),
            ..sitemap::FetchedBytes::default()
        },
    }
}

fn from_sitemap_response(
    status: u16,
    response: ureq::Response,
    cancel: Option<&AtomicBool>,
) -> sitemap::FetchedBytes {
    let content_type = response.header("content-type").unwrap_or("").to_string();
    let location = response.header("location").unwrap_or("").to_string();
    let retry_after_ms = response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000).min(10_000))
        .unwrap_or(0);
    let mut body = Vec::new();
    let mut error = String::new();
    if (200..300).contains(&status) {
        let capacity = response
            .header("content-length")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0)
            .min(sitemap::MAX_COMPRESSED_BYTES as usize);
        body.reserve(capacity);
        let mut reader = response.into_reader();
        let mut chunk = [0_u8; 64 * 1024];
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                body.clear();
                error = "Stopped".into();
                break;
            }
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    body.extend_from_slice(&chunk[..read]);
                    if body.len() as u64 > sitemap::MAX_COMPRESSED_BYTES {
                        body.clear();
                        error = "Sitemap download is larger than 16 MiB".into();
                        break;
                    }
                }
                Err(cause) => {
                    error = cause.to_string();
                    break;
                }
            }
        }
    } else {
        // Keep the connection reusable even when the document is a redirect or an error.
        let _ = std::io::copy(
            &mut response.into_reader().take(DRAIN_BYTES),
            &mut std::io::sink(),
        );
    }
    sitemap::FetchedBytes {
        status,
        content_type,
        location,
        body,
        error,
        retry_after_ms,
    }
}

fn sleep_cancellable(duration: Duration, cancel: Option<&AtomicBool>) -> bool {
    let started = Instant::now();
    loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return false;
        }
        let remaining = duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return true;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

fn retain_occurrence(
    evidence: &mut Vec<LinkOccurrence>,
    occurrence: LinkOccurrence,
    limit: &mut String,
) {
    if evidence.len() < links::MAX_PAGE_OCCURRENCES {
        evidence.push(occurrence);
    } else if limit.is_empty() {
        *limit = "Page link evidence limited to 10,000 occurrences".into();
    }
}

/// Resolve an anchor for evidence without silently dropping malformed or non-network links.
enum EvidenceHref {
    Url {
        url: Url,
        key: String,
        fragment: String,
    },
    Fragment {
        url: Url,
        key: String,
        fragment: String,
    },
    Invalid(String),
}

fn resolve_evidence(page: &Url, href: &str) -> EvidenceHref {
    let raw = href.trim();
    if raw.is_empty() {
        return EvidenceHref::Invalid("Empty href".into());
    }
    if let Some(fragment) = raw.strip_prefix('#') {
        let mut url = page.clone();
        url.set_fragment(None);
        return EvidenceHref::Fragment {
            key: url_key(url.clone()),
            url,
            fragment: links::truncate_chars(fragment, links::MAX_ANCHOR_CHARS),
        };
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("mailto:")
        || lower.starts_with("javascript:")
        || lower.starts_with("tel:")
        || lower.starts_with("data:")
    {
        return EvidenceHref::Invalid(format!(
            "Non-HTTP scheme ({})",
            lower.split(':').next().unwrap_or("unknown")
        ));
    }
    let mut url = match page.join(raw) {
        Ok(url) => url,
        Err(_) => return EvidenceHref::Invalid("Invalid URL".into()),
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return EvidenceHref::Invalid(format!("Non-HTTP scheme ({})", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return EvidenceHref::Invalid("Embedded credentials are not checked".into());
    }
    let fragment = url.fragment().unwrap_or("").to_string();
    url.set_fragment(None);
    let key = url_key(url.clone());
    EvidenceHref::Url {
        url,
        key,
        fragment: links::truncate_chars(&fragment, links::MAX_ANCHOR_CHARS),
    }
}

/// Document downloads that the optional checker may request without turning them into pages.
fn is_document_download(url: &Url) -> bool {
    const EXTENSIONS: &[&str] = &[
        "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "csv",
        "txt", "zip", "gz", "tar", "tgz", "7z", "rar", "epub",
    ];
    extension_is(url, EXTENSIONS)
}

/// Image, script, font, style, and media assets that are never optional link checks.
fn is_web_asset(url: &Url) -> bool {
    const EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "css", "js", "mjs", "map", "mp4", "mp3",
        "wav", "ogg", "woff", "woff2", "ttf", "eot", "xml", "json", "rss", "atom",
    ];
    extension_is(url, EXTENSIONS)
}

fn extension_is(url: &Url, extensions: &[&str]) -> bool {
    let last = url.path().rsplit('/').next().unwrap_or("");
    last.rsplit_once('.').is_some_and(|(_, ext)| {
        extensions
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

fn resolve(page: &Url, href: &str) -> Option<Url> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.starts_with("mailto:")
        || href.starts_with("javascript:")
        || href.starts_with("tel:")
        || href.starts_with("data:")
    {
        return None;
    }
    let mut url = page.join(href).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

pub(crate) fn url_key(mut url: Url) -> String {
    url.set_fragment(None);
    if url.path() != "/" && url.path().ends_with('/') {
        let path = url.path().trim_end_matches('/').to_string();
        url.set_path(&path);
    }
    // `Url` keeps its serialization as a string, so copying that is cheaper than going
    // through the `Display` machinery.
    url.as_str().to_owned()
}

fn host_of(url: &str) -> Option<String> {
    Url::parse(url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
    })
}

fn same_host(url: &Url, host: &str) -> bool {
    url.host_str()
        .is_some_and(|candidate| candidate.trim_end_matches('.').eq_ignore_ascii_case(host))
}

fn looks_like_asset(url: &Url) -> bool {
    const SKIP: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "css", "js", "mjs", "map", "pdf", "zip",
        "gz", "mp4", "mp3", "woff", "woff2", "ttf", "eot", "xml", "json", "rss", "atom",
    ];
    let last = url.path().rsplit('/').next().unwrap_or("");
    let Some((_, ext)) = last.rsplit_once('.') else {
        return false;
    };
    SKIP.iter().any(|skip| ext.eq_ignore_ascii_case(skip))
}

fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    haystack.len() >= needle.len()
        && haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

fn is_html(content_type: &str, body: &str) -> bool {
    if contains_ascii_ci(content_type, "html") {
        return true;
    }
    if !content_type.is_empty() && !contains_ascii_ci(content_type, "text/plain") {
        return false;
    }
    let head = body.trim_start();
    let head = head.get(..240).unwrap_or(head).to_ascii_lowercase();
    head.starts_with("<!doctype html")
        || head.starts_with("<html")
        || head.contains("<head")
        || head.contains("<body")
}

fn field_issue(page: &Page, value: &str, min: usize, max: usize, duplicate: bool) -> String {
    if !auditable(page) {
        return String::new();
    }
    let length = text_len(value);
    if length == 0 {
        return "Missing".into();
    }
    let length_issue = if length > max {
        "Too long"
    } else if length < min {
        "Too short"
    } else {
        ""
    };
    match (duplicate, length_issue) {
        (true, "") => "Duplicate".into(),
        (true, issue) => format!("Duplicate, {}", issue.to_lowercase()),
        (false, issue) => issue.into(),
    }
}

/// Record a normalized value in `seen`, adding it to `dups` when it has been seen before.
///
/// Checking with the borrowed key first means a repeated value costs no allocation at all
/// after the first time it is marked duplicate.
fn collect_duplicate(
    seen: &mut FastSet<String>,
    dups: &mut FastSet<String>,
    buffer: &mut String,
    value: &str,
) {
    let key = norm_into(value, buffer);
    if key.is_empty() {
        return;
    }
    if seen.contains(key) {
        if !dups.contains(key) {
            dups.insert(key.to_string());
        }
    } else {
        seen.insert(key.to_string());
    }
}

fn norm(value: &str) -> String {
    clean(value).to_lowercase()
}

/// Collapse whitespace and lowercase into a caller-owned buffer, so callers that normalize
/// many values can reuse the allocation. Matches `norm`.
fn norm_into<'a>(value: &str, buffer: &'a mut String) -> &'a str {
    buffer.clear();
    let mut first = true;
    for word in value.split_whitespace() {
        if !first {
            buffer.push(' ');
        }
        first = false;
        buffer.push_str(word);
    }
    if buffer.is_ascii() {
        buffer.make_ascii_lowercase();
    } else {
        let lower = buffer.to_lowercase();
        buffer.clear();
        buffer.push_str(&lower);
    }
    buffer.as_str()
}

fn short_target(url: &str) -> String {
    Url::parse(url)
        .map(|url| url.path().to_string())
        .unwrap_or_else(|_| url.to_string())
}

fn clean(value: &str) -> String {
    text_of(std::iter::once(value))
}

/// Collapse runs of whitespace across text chunks in one pass, without building a temporary
/// string or a `Vec` of words.
fn text_of<'a>(chunks: impl Iterator<Item = &'a str>) -> String {
    let mut text = String::new();
    for chunk in chunks {
        for word in chunk.split_whitespace() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(word);
        }
    }
    text
}

pub fn sample_pages() -> Vec<Page> {
    let html = |url: &str| Page {
        url: url.into(),
        status: 200,
        content_type: "text/html; charset=utf-8".into(),
        title: String::new(),
        meta_description: String::new(),
        h1s: Vec::new(),
        noindex: false,
        words: 0,
        redirect_to: String::new(),
        error: String::new(),
        depth: 0,
        canonical: String::new(),
        images: 0,
        images_missing_alt: 0,
        links: Arc::new(Vec::new()),
        link_evidence: Arc::new(Vec::new()),
        link_evidence_limit: String::new(),
        extraction: Arc::new(Vec::new()),
        extraction_limit: String::new(),
        content: Arc::new(ContentEvidence::default()),
        indexability: Assessment::default(),
    };
    let ok_meta =
        "A short field guide to the pages, titles and links that decide whether a page gets found.";
    let mut pages = vec![
        Page {
            title: "Example".into(),
            meta_description: String::new(),
            h1s: vec!["Welcome".into()],
            words: 86,
            ..html("https://example.com/")
        },
        Page {
            title: "Example".into(),
            meta_description: "Plans for teams.".into(),
            words: 40,
            depth: 1,
            ..html("https://example.com/pricing")
        },
        Page {
            title: "A practical guide to launching a small site without losing the whole week to it".into(),
            meta_description: "This description keeps going long after a search result would have cut it off, repeating the same promise about launches, checklists, reviews and the week you will not get back.".into(),
            h1s: vec!["Launch guide".into(), "Start here".into()],
            words: 240,
            depth: 1,
            ..html("https://example.com/guides/launch")
        },
        Page {
            title: "Why a calm homepage converts better".into(),
            meta_description: ok_meta.into(),
            h1s: vec!["Why a calm homepage converts better".into()],
            words: 180,
            depth: 1,
            ..html("https://example.com/blog/calm")
        },
        Page {
            title: "Field notes from a quiet redesign".into(),
            meta_description: "Notes from rebuilding a homepage so the first screen says one thing and the title matches it.".into(),
            h1s: vec!["Why a calm homepage converts better".into()],
            words: 150,
            depth: 2,
            ..html("https://example.com/blog/notes")
        },
        Page {
            title: "A private page for the team only".into(),
            meta_description: "Internal notes that stay out of the index and are not a public description of the product.".into(),
            h1s: vec!["Team notes".into()],
            noindex: true,
            words: 60,
            depth: 1,
            ..html("https://example.com/private")
        },
        Page {
            status: 301,
            title: String::new(),
            redirect_to: "https://example.com/pricing".into(),
            depth: 1,
            ..html("https://example.com/old-pricing")
        },
        Page {
            status: 404,
            depth: 1,
            ..html("https://example.com/promo")
        },
        Page {
            status: 500,
            content_type: "text/plain".into(),
            depth: 2,
            ..html("https://example.com/api/health")
        },
    ];
    pages[6].content_type = "text/html".into();
    pages[7].content_type = "text/html".into();

    pages[0].canonical = "https://example.com/".into();
    pages[1].canonical = "https://example.com/pricing".into();
    pages[3].canonical = "https://example.com/blog/calm".into();
    pages[4].canonical = "https://example.com/blog/calm".into();
    pages[0].images = 2;
    pages[0].images_missing_alt = 1;
    pages[2].images = 3;
    pages[2].images_missing_alt = 3;
    pages[0].links = Arc::new(vec![
        "https://example.com/pricing".into(),
        "https://example.com/promo".into(),
        "https://example.com/blog/calm".into(),
    ]);
    pages[3].links = Arc::new(vec![
        "https://example.com/blog/notes".into(),
        "https://example.com/guides/launch".into(),
    ]);
    for page in &mut pages {
        page.indexability = sample_assessment(page);
        page.noindex = page.indexability.has_noindex();
        page.canonical = page
            .indexability
            .first_valid_canonical()
            .unwrap_or_default()
            .to_string();
        page.link_evidence = Arc::new(
            page.links
                .iter()
                .enumerate()
                .map(|(order, link)| {
                    let anchor = link
                        .rsplit('/')
                        .next()
                        .filter(|part| !part.is_empty())
                        .unwrap_or("Home")
                        .replace('-', " ");
                    LinkOccurrence {
                        source: page.url.clone(),
                        order: order as u32,
                        raw_href: link.clone(),
                        resolved: link.clone(),
                        key: link.clone(),
                        anchor,
                        stage: EvidenceStage::Document,
                        ..LinkOccurrence::default()
                    }
                })
                .collect(),
        );
    }
    pages
}

/// The nine-page sample plus mixed indexability states for offline review scenes.
pub fn indexability_sample_pages() -> Vec<Page> {
    let mut pages = sample_pages();
    let page = |url: &str| Page {
        url: url.into(),
        status: 200,
        content_type: "text/html; charset=utf-8".into(),
        title: String::new(),
        meta_description: String::new(),
        h1s: Vec::new(),
        noindex: false,
        words: 0,
        redirect_to: String::new(),
        error: String::new(),
        depth: 1,
        canonical: String::new(),
        images: 0,
        images_missing_alt: 0,
        links: Arc::new(Vec::new()),
        link_evidence: Arc::new(Vec::new()),
        link_evidence_limit: String::new(),
        extraction: Arc::new(Vec::new()),
        extraction_limit: String::new(),
        content: Arc::new(ContentEvidence::default()),
        indexability: Assessment::default(),
    };
    let generic = |raw: &str| indexability::parse_meta_directive("robots", raw).unwrap();
    let google = |raw: &str| indexability::parse_meta_directive("googlebot", raw).unwrap();
    let bing = |raw: &str| indexability::parse_meta_directive("bingbot", raw).unwrap();
    let mut push =
        |mut page: Page, directives: Vec<RobotsDirective>, canonicals: &[&str], read: ReadState| {
            let base = Url::parse(&page.url).unwrap();
            let evidence = PageEvidence {
                directives,
                canonicals: canonicals
                    .iter()
                    .map(|raw| indexability::canonical_declaration(Some(raw), &base))
                    .collect(),
                read,
                truncated: false,
                ..PageEvidence::default()
            };
            page.indexability = indexability::assess(
                &page.url,
                page.status,
                &page.content_type,
                &page.redirect_to,
                &page.error,
                evidence,
                indexability::CaptureInfo::html(),
            );
            page.noindex = page.indexability.has_noindex();
            page.canonical = page
                .indexability
                .first_valid_canonical()
                .unwrap_or_default()
                .to_string();
            pages.push(page);
        };

    push(
        Page {
            title: "Wholesale enquiry notes for the spring range".into(),
            meta_description:
                "How the wholesale range is planned, packed and shipped to stockists each season."
                    .into(),
            h1s: vec!["Wholesale notes".into()],
            words: 132,
            ..page("https://example.com/wholesale")
        },
        vec![
            generic("noindex"),
            google("noindex, nofollow"),
            bing("noindex"),
        ],
        &["https://example.com/blog/calm", "/wholesale"],
        ReadState::Complete,
    );
    push(
        Page {
            title: "Spring campaign landing page".into(),
            meta_description:
                "A campaign page with two competing canonical declarations for the review scene."
                    .into(),
            h1s: vec!["Spring campaign".into()],
            words: 118,
            ..page("https://example.com/campaign")
        },
        Vec::new(),
        &["/campaign", "/spring"],
        ReadState::Complete,
    );
    push(
        Page {
            title: "Seasonal report 2026".into(),
            meta_description: "A PDF report linked from the blog.".into(),
            words: 0,
            content_type: "application/pdf".into(),
            ..page("https://example.com/report-2026.pdf")
        },
        Vec::new(),
        &[],
        ReadState::NotRead,
    );
    push(
        Page {
            title: "Old guide".into(),
            redirect_to: "https://example.com/pricing".into(),
            status: 301,
            ..page("https://example.com/legacy")
        },
        Vec::new(),
        &[],
        ReadState::NotRead,
    );
    push(
        Page {
            title: "Long feed page".into(),
            words: 340,
            ..page("https://example.com/feed")
        },
        Vec::new(),
        &["/feed"],
        ReadState::Truncated,
    );
    push(
        Page {
            title: "Status page".into(),
            status: 503,
            ..page("https://example.com/status")
        },
        Vec::new(),
        &[],
        ReadState::NotRead,
    );
    push(
        Page {
            error: "connection reset by peer".into(),
            status: 0,
            content_type: String::new(),
            ..page("https://example.com/offline")
        },
        Vec::new(),
        &[],
        ReadState::Failed,
    );
    push(
        Page {
            title: "Quiet notes page".into(),
            meta_description:
                "A nofollow page that still has no noindex rule and no canonical declaration."
                    .into(),
            h1s: vec!["Quiet notes".into()],
            words: 96,
            ..page("https://example.com/nofollow")
        },
        vec![generic("nofollow")],
        &[],
        ReadState::Complete,
    );
    pages
}

fn sample_assessment(page: &Page) -> Assessment {
    let mut evidence = PageEvidence {
        // Mirror the transport: only a 200 HTML response has a body to read.
        read: if page.status == 200 && page.content_type.to_ascii_lowercase().contains("html") {
            ReadState::Complete
        } else {
            ReadState::NotRead
        },
        ..PageEvidence::default()
    };
    if page.noindex {
        evidence.directives.push(RobotsDirective {
            source: indexability::EvidenceSource::Meta,
            stage: indexability::EvidenceStage::Document,
            agent: None,
            raw: "noindex".into(),
            noindex: true,
            nofollow: false,
            truncated: false,
        });
    }
    if !page.canonical.is_empty()
        && let Ok(url) = Url::parse(&page.url)
    {
        evidence
            .canonicals
            .push(indexability::canonical_declaration(
                Some(&page.canonical),
                &url,
            ));
    }
    indexability::assess(
        &page.url,
        page.status,
        &page.content_type,
        &page.redirect_to,
        &page.error,
        evidence,
        indexability::CaptureInfo::html(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn optional_retry_after_parses_seconds_http_dates_and_past_dates() {
        use std::time::{Duration, UNIX_EPOCH};
        let date = UNIX_EPOCH + Duration::from_secs(784111777);
        for header in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(
                retry_after_delay(Some(header), date - Duration::from_secs(7)),
                Some(Duration::from_secs(7))
            );
            assert_eq!(
                retry_after_delay(Some(header), date + Duration::from_secs(7)),
                Some(Duration::ZERO)
            );
        }
        assert_eq!(
            retry_after_delay(Some(" 7 "), date),
            Some(Duration::from_secs(7))
        );
        assert_eq!(retry_after_delay(Some("nonsense"), date), None);
    }

    #[test]
    fn optional_huge_crawl_delay_is_limited_without_a_request() {
        let origin = "https://example.test";
        let mut robots = HashMap::from([(
            origin.to_string(),
            RobotsPolicy::Known(Some(Robots {
                crawl_delay_ms: u64::MAX,
                ..Robots::default()
            })),
        )]);
        let mut starts = HashMap::from([(origin.to_string(), Instant::now())]);
        let mut budget = 1;
        let result = optional_check_result(
            "https://example.test/target",
            &fast_map(0),
            &mut fast_map(0),
            &fast_set(0),
            "internal.test",
            &check_agent(),
            &mut robots,
            &mut starts,
            &mut HashMap::new(),
            &AtomicBool::new(false),
            &mut budget,
        );
        assert_eq!(result.state, LinkCheckState::Limited);
        assert_eq!(budget, 1, "an impossible delay must not issue a request");
    }

    #[test]
    fn optional_retry_after_huge_seconds_is_explicitly_unrepresentable() {
        let mut cooldowns = HashMap::new();
        record_retry_cooldown(
            &mut cooldowns,
            "https://example.test",
            Some("18446744073709551615"),
        );
        assert!(matches!(
            cooldowns.get("https://example.test"),
            Some(RetryCooldown::Unrepresentable)
        ));
    }

    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn html_fetch(status: u16, body: &str) -> Fetched {
        Fetched {
            status,
            content_type: "text/html; charset=utf-8".into(),
            body: body.into(),
            read: ReadState::Complete,
            ..Fetched::default()
        }
    }

    #[test]
    fn normalization_buffer_matches_the_allocating_form() {
        let samples = [
            "  Hello   There ",
            "MIXED case\twith\nnewlines",
            "unicode Straße Σ",
            "",
            "   ",
        ];
        let mut buffer = String::new();
        for sample in samples {
            assert_eq!(
                norm_into(sample, &mut buffer).to_string(),
                norm(sample),
                "{sample:?}"
            );
        }
    }

    #[test]
    fn source_only_browser_capture_keeps_document_stage_link_evidence() {
        let capture = Capture {
            status: 200,
            content_type: "text/html".into(),
            source_html: Some("<a href=\"/from-source\">Source link</a>".into()),
            source_state: SourceState::Captured,
            rendered_html: None,
            state: RenderState::Captured,
            ..Capture::default()
        };
        let (_, parsed, source, info) = browser_page(&capture, &ContentBudget::default(), None);
        assert!(parsed.is_none(), "no rendered DOM is audited");
        let source = source.expect("source fallback is retained");
        assert_eq!(source.anchors.len(), 1);
        assert_eq!(source.anchors[0].href, "/from-source");
        assert!(!source.content.exact_eligible);
        assert!(!info.is_complete(), "the HTML capture remains incomplete");
    }

    #[test]
    fn anchor_evidence_caps_without_truncating_page_discovery_links() {
        let body = (0..links::MAX_PAGE_OCCURRENCES + 1)
            .map(|index| format!("<a href=\"/p{index}\">Page</a>"))
            .collect::<String>();
        let parsed = parse_html(&body);
        assert_eq!(parsed.anchors.len(), links::MAX_PAGE_OCCURRENCES);
        assert!(parsed.anchors_truncated);
        assert_eq!(parsed.links.len(), links::MAX_PAGE_OCCURRENCES + 1);
    }

    #[test]
    fn optional_waits_honor_the_absolute_deadline_and_cancellation() {
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_millis(60);
        let started = Instant::now();
        let stopped = sleep_cancellable_until(Duration::from_secs(60), &cancel, deadline);
        assert!(matches!(stopped, Err(CheckStop::Deadline)));
        assert!(started.elapsed() < Duration::from_secs(2));

        cancel.store(true, Ordering::Relaxed);
        let stopped = sleep_cancellable_until(
            Duration::from_secs(60),
            &cancel,
            Instant::now() + Duration::from_secs(30),
        );
        assert!(matches!(stopped, Err(CheckStop::Cancelled)));
    }

    #[test]
    fn combined_rendered_and_source_evidence_share_the_page_cap() {
        let mut evidence = Vec::new();
        let mut limit = String::new();
        for order in 0..6_000 {
            retain_occurrence(
                &mut evidence,
                LinkOccurrence {
                    order,
                    stage: EvidenceStage::Rendered,
                    ..LinkOccurrence::default()
                },
                &mut limit,
            );
        }
        for order in 6_000..11_000 {
            retain_occurrence(
                &mut evidence,
                LinkOccurrence {
                    order,
                    stage: EvidenceStage::Document,
                    ..LinkOccurrence::default()
                },
                &mut limit,
            );
        }
        assert_eq!(evidence.len(), links::MAX_PAGE_OCCURRENCES);
        assert!(limit.contains("10,000"));
        assert_eq!(
            evidence.last().map(|occurrence| occurrence.stage),
            Some(EvidenceStage::Document)
        );
    }

    #[test]
    fn evidence_cap_does_not_cut_ordinary_page_discovery() {
        let body = format!(
            "{}<a href='/tail'>Tail</a>",
            "<a href='/'>Home</a>".repeat(links::MAX_PAGE_OCCURRENCES)
        );
        let out = crawl_with(
            "http://example.test/",
            Limits {
                max_pages: 2,
                max_depth: 1,
            },
            |url| Fetched {
                status: 200,
                content_type: "text/html".into(),
                body: if url.ends_with("/tail") {
                    "<p>Tail</p>".into()
                } else {
                    body.clone()
                },
                read: ReadState::Complete,
                ..Fetched::default()
            },
        );
        assert!(
            out.pages.iter().any(|page| page.url.ends_with("/tail")),
            "evidence cap removed ordinary discovery: {}",
            out.message
        );
    }

    #[test]
    fn discovery_beyond_the_evidence_cap_still_rejects_embedded_credentials() {
        use std::sync::{Arc, Mutex};
        let requested = Arc::new(Mutex::new(Vec::new()));
        let seen = requested.clone();
        let body = format!(
            "{}<a href='http://user:pass@example.test/secret'>Secret</a>",
            "<a href='/'>Home</a>".repeat(links::MAX_PAGE_OCCURRENCES)
        );
        let _ = crawl_with(
            "http://example.test/",
            Limits {
                max_pages: 2,
                max_depth: 1,
            },
            move |url| {
                seen.lock().unwrap().push(url.to_string());
                Fetched {
                    status: 200,
                    content_type: "text/html".into(),
                    body: body.clone(),
                    read: ReadState::Complete,
                    ..Fetched::default()
                }
            },
        );
        assert!(
            requested
                .lock()
                .unwrap()
                .iter()
                .all(|url| !url.contains("user:pass")),
            "credentials in a tail anchor were requested"
        );
    }

    #[test]
    fn overlong_hrefs_are_declined_without_requesting_a_truncated_destination() {
        use std::sync::{Arc, Mutex};
        let requested = Arc::new(Mutex::new(Vec::new()));
        let seen = requested.clone();
        let long = format!(
            "https://example.test/{}/{}",
            "segment".repeat(links::MAX_URL_BYTES / 7),
            "tail-marker"
        );
        let body = format!("<a href='{long}'>Long</a><a href='/real'>Real</a>");
        let out = crawl_with(
            "https://example.test/",
            Limits {
                max_pages: 4,
                max_depth: 1,
            },
            move |url| {
                seen.lock().unwrap().push(url.to_string());
                Fetched {
                    status: 200,
                    content_type: "text/html".into(),
                    body: if url.ends_with("/real") {
                        "<p>Real</p>".into()
                    } else {
                        body.clone()
                    },
                    read: ReadState::Complete,
                    ..Fetched::default()
                }
            },
        );
        assert!(out.pages.iter().any(|page| page.url.ends_with("/real")));
        let requested = requested.lock().unwrap();
        assert!(
            requested
                .iter()
                .all(|url| url.len() <= links::MAX_URL_BYTES && !url.contains('…')),
            "a truncated URL was requested: {:?}",
            requested
                .iter()
                .filter(|url| url.len() > links::MAX_URL_BYTES || url.contains('…'))
                .collect::<Vec<_>>()
        );
        let home = out
            .pages
            .iter()
            .find(|page| page.url == "https://example.test/")
            .expect("home");
        let long = home
            .link_evidence
            .iter()
            .find(|link| link.anchor == "Long")
            .expect("overlong evidence");
        assert_eq!(long.check_state(), LinkCheckState::Limited);
        assert!(long.truncated);
        assert!(long.state_reason().contains("8 KiB"));
    }

    #[test]
    fn parses_the_fields_the_audits_need() {
        let body = r#"<!doctype html><html><head>
            <title>  Hello   there </title>
            <base href="https://example.com/docs/">
            <meta name="description" content="A description.">
            <meta name="robots" content="noindex, nofollow">
            <link rel="canonical" href="/canonical">
            </head><body>
            <h1>Welcome</h1><h1>Again</h1>
            <p>Two words</p>
            <img src="a.png"><img src="b.png" alt=""><img src="c.png" alt="Cat">
            <script>var ignore = "lots of script words here";</script>
            <a href="/pricing">Pricing</a>
            <a href="mailto:a@b.c">Mail</a>
            <a href="https://other.test/out">Out</a>
            </body></html>"#;
        let parsed = parse_html(body);
        assert_eq!(parsed.title, "Hello there");
        assert_eq!(parsed.meta, "A description.");
        assert_eq!(parsed.h1s, vec!["Welcome".to_string(), "Again".to_string()]);
        assert!(
            parsed
                .robots_directives
                .iter()
                .any(|directive| directive.noindex)
        );
        assert_eq!(parsed.canonicals, vec![Some("/canonical".to_string())]);
        assert_eq!(parsed.images, 3);
        assert_eq!(parsed.images_missing_alt, 2);
        assert!(parsed.links.iter().any(|link| link.href == "/pricing"));
        assert!(
            parsed
                .links
                .iter()
                .any(|link| link.href.contains("other.test"))
        );
        assert!(resolve(&Url::parse("https://example.com/").unwrap(), "mailto:a@b.c").is_none());
        assert_eq!(
            resolve(
                &Url::parse("https://example.com/docs/page").unwrap(),
                "/pricing"
            )
            .unwrap()
            .as_str(),
            "https://example.com/pricing"
        );
        assert!(parsed.words >= 2);
        assert!(!parsed.links.is_empty());
    }

    #[test]
    fn stays_on_the_host_and_respects_depth_and_the_page_cap() {
        let fetch = |url: &str| match url {
            "https://example.com/" => html_fetch(
                200,
                r#"<title>Home page title that is comfortably long</title><a href="/a">a</a><a href="/missing">m</a><a href="https://other.test/x">o</a><a href="/photo.png">p</a>"#,
            ),
            "https://example.com/a" => html_fetch(
                200,
                r#"<title>A second page title that is comfortably long</title><a href="/b">b</a>"#,
            ),
            "https://example.com/b" => html_fetch(
                200,
                r#"<title>A third page title that is comfortably long</title>"#,
            ),
            "https://example.com/missing" => Fetched {
                status: 404,
                content_type: "text/html".into(),
                body: "missing".into(),
                ..Fetched::default()
            },
            other => Fetched {
                error: format!("unexpected {other}"),
                ..Fetched::default()
            },
        };
        let outcome = crawl_with(
            "example.com",
            Limits {
                max_pages: 10,
                max_depth: 1,
            },
            fetch,
        );
        let urls: Vec<_> = outcome.pages.iter().map(|page| page.url.as_str()).collect();
        assert!(urls.contains(&"https://example.com/"), "{urls:?}");
        assert!(urls.contains(&"https://example.com/a"), "{urls:?}");
        assert!(urls.contains(&"https://example.com/missing"), "{urls:?}");
        assert!(
            !urls.iter().any(|url| url.contains("other.test")),
            "{urls:?}"
        );
        assert!(
            !urls.contains(&"https://example.com/b"),
            "depth stops before /b: {urls:?}"
        );
        assert!(!urls.iter().any(|url| url.ends_with(".png")), "{urls:?}");
        assert!(outcome.pages.iter().any(|page| page.status == 404));

        let capped = crawl_with(
            "https://example.com/",
            Limits {
                max_pages: 1,
                max_depth: 3,
            },
            fetch,
        );
        assert_eq!(capped.pages.len(), 1);
        assert!(capped.message.contains("stopped"));
    }

    #[test]
    fn follows_a_same_host_redirect_and_records_it() {
        let fetch = |url: &str| match url {
            "https://example.com/old" => Fetched {
                status: 301,
                content_type: "text/html".into(),
                location: "/pricing".into(),
                ..Fetched::default()
            },
            "https://example.com/pricing" => html_fetch(
                200,
                "<title>Pricing page title that is long enough</title><h1>Pricing</h1><p>Plans for teams that want a calmer homepage.</p>",
            ),
            other => Fetched {
                error: format!("unexpected {other}"),
                ..Fetched::default()
            },
        };
        let outcome = crawl_with(
            "https://example.com/old",
            Limits {
                max_pages: 10,
                max_depth: 2,
            },
            fetch,
        );
        let old = outcome
            .pages
            .iter()
            .find(|page| page.url.ends_with("/old"))
            .unwrap();
        assert_eq!(old.status, 301);
        assert_eq!(old.redirect_to, "https://example.com/pricing");
        assert!(response_issue(old).contains("Redirect"));
        assert!(
            outcome
                .pages
                .iter()
                .any(|page| page.url == "https://example.com/pricing" && page.status == 200)
        );
    }

    #[test]
    fn sample_site_has_one_issue_of_each_kind_the_tabs_count() {
        let pages = sample_pages();
        let counts = counts(&pages);
        assert_eq!(counts.pages, 9);
        assert_eq!(counts.title, 3, "short duplicate, and the long guide");
        assert_eq!(counts.meta, 3);
        assert_eq!(
            counts.h1, 4,
            "missing, multiple, and both copies of the duplicate"
        );
        assert_eq!(counts.response, 3, "301, 404, 500");
        let audit = Audit::from_pages(&pages);
        let home = &pages[0];
        assert_eq!(title_issue(home, &audit), "Duplicate, too short");
        assert_eq!(title_issue(&pages[1], &audit), "Duplicate, too short");
        assert_eq!(h1_issue(&pages[1], &audit), "Missing");
        assert_eq!(h1_issue(&pages[2], &audit), "Multiple");
        assert_eq!(h1_issue(&pages[4], &audit), "Duplicate");
        assert_eq!(pages[5].indexability.label.text(), "Noindex");
        assert!(
            title_issue(&pages[6], &audit).is_empty(),
            "redirects are not missing-title issues"
        );
        assert!(response_issue(&pages[7]).contains("404"));
    }

    #[test]
    fn parallel_workers_fill_the_page_cap_and_terminate() {
        let fetch = |url: &str| {
            let n = url
                .rsplit('/')
                .next()
                .and_then(|part| part.strip_prefix('p').unwrap_or(part).parse::<usize>().ok())
                .unwrap_or(0);
            let mut links = String::new();
            for k in 1..=4 {
                links.push_str(&format!("<a href=\"/p{}\">next</a>", n * 4 + k));
            }
            html_fetch(
                200,
                &format!("<title>Page {n} with a title that is long enough</title>{links}"),
            )
        };
        let outcome = crawl_with(
            "https://example.com/",
            Limits {
                max_pages: 40,
                max_depth: 3,
            },
            fetch,
        );
        assert_eq!(outcome.pages.len(), 40);
        assert!(outcome.message.contains("stopped"), "{}", outcome.message);
        let unique: HashSet<_> = outcome.pages.iter().map(|page| page.url.as_str()).collect();
        assert_eq!(unique.len(), 40, "no page is fetched twice");
    }

    #[test]
    fn robots_rules_honor_specificity_wildcards_and_crawl_delay() {
        let robots = Robots::parse(
            "User-agent: *\nDisallow: /private\nAllow: /private/ok\nDisallow: /*.pdf$\nCrawl-delay: 1.5\n",
            USER_AGENT,
        );
        assert!(!robots.allows("/private"));
        assert!(!robots.allows("/private/deep"));
        assert!(robots.allows("/private/ok"), "the longer Allow wins");
        assert!(!robots.allows("/guide.pdf"), "$ anchors the pattern");
        assert!(robots.allows("/guide.pdf?v=2"));
        assert!(robots.allows("/"));
        assert_eq!(robots.crawl_delay(), Some(Duration::from_millis(1_500)));

        let specific = Robots::parse(
            "User-agent: *\nDisallow: /\n\nUser-agent: Calmtoad\nDisallow: /admin\n",
            USER_AGENT,
        );
        assert!(
            specific.allows("/"),
            "the specific group replaces the wildcard"
        );
        assert!(!specific.allows("/admin"));

        let sitemap_between_agents = Robots::parse(
            "User-agent: Calmtoad\nSitemap: https://example.com/map.xml\nUser-agent: *\nDisallow: /\n",
            USER_AGENT,
        );
        assert!(
            !sitemap_between_agents.allows("/"),
            "a Sitemap directive does not split adjacent user-agent groups"
        );
    }

    #[test]
    fn robots_txt_blocks_disallowed_links_and_a_disallowed_seed() {
        let fetch = |url: &str| match url {
            "https://example.com/robots.txt" => Fetched {
                status: 200,
                content_type: "text/plain".into(),
                body: "User-agent: *\nDisallow: /private\n".into(),
                ..Fetched::default()
            },
            "https://example.com/" => html_fetch(
                200,
                r#"<title>Home page title that is comfortably long</title><a href="/private">p</a><a href="/public">q</a>"#,
            ),
            "https://example.com/public" => {
                html_fetch(200, "<title>Public page title that is long enough</title>")
            }
            "https://example.com/private" => {
                html_fetch(200, "<title>Private page title that is long enough</title>")
            }
            other => Fetched {
                error: format!("unexpected {other}"),
                ..Fetched::default()
            },
        };
        let limits = Limits {
            max_pages: 10,
            max_depth: 2,
        };
        let outcome = crawl_with("https://example.com/", limits, fetch);
        let urls: Vec<_> = outcome.pages.iter().map(|page| page.url.as_str()).collect();
        assert!(urls.contains(&"https://example.com/"), "{urls:?}");
        assert!(urls.contains(&"https://example.com/public"), "{urls:?}");
        assert!(
            !urls.contains(&"https://example.com/private"),
            "robots.txt kept us out: {urls:?}"
        );

        let blocked = crawl_with("https://example.com/private", limits, fetch);
        assert!(blocked.pages.is_empty());
        assert_eq!(blocked.message, "Blocked by robots.txt · example.com");
    }

    #[test]
    fn streaming_reports_each_page_and_cancel_stops_the_crawl() {
        let fetch = |url: &str| {
            if url.ends_with("/robots.txt") {
                return Fetched {
                    status: 404,
                    ..Fetched::default()
                };
            }
            let n = url
                .rsplit('/')
                .next()
                .and_then(|part| part.strip_prefix('p'))
                .and_then(|part| part.parse::<usize>().ok())
                .unwrap_or(0);
            let links: String = (1..=3)
                .map(|k| format!("<a href=\"/p{}\">next</a>", n * 3 + k))
                .collect();
            html_fetch(
                200,
                &format!("<title>Streamed page {n} title that is long enough</title>{links}"),
            )
        };

        let all = std::cell::RefCell::new(Vec::new());
        let no_stop = AtomicBool::new(false);
        let outcome = crawl_engine(
            "https://example.com/",
            Limits {
                max_pages: 6,
                max_depth: 2,
            },
            fetch,
            |_, _before, _backoff| sitemap::FetchedBytes::default(),
            false,
            CrawlOptions::default(),
            Some(&no_stop),
            None,
            None,
            &mut |event| {
                if let CrawlEvent::Page(page) = event {
                    all.borrow_mut().push(page.url.clone());
                }
            },
        );
        assert_eq!(outcome.pages.len(), 6);
        let streamed = all.into_inner();
        assert_eq!(
            streamed,
            outcome
                .pages
                .iter()
                .map(|page| page.url.clone())
                .collect::<Vec<_>>()
        );

        let stop_after_one = AtomicBool::new(false);
        let seen = std::cell::RefCell::new(Vec::new());
        let outcome = crawl_engine(
            "https://example.com/",
            Limits {
                max_pages: 6,
                max_depth: 2,
            },
            fetch,
            |_, _before, _backoff| sitemap::FetchedBytes::default(),
            false,
            CrawlOptions::default(),
            Some(&stop_after_one),
            None,
            None,
            &mut |event| {
                if let CrawlEvent::Page(page) = event {
                    seen.borrow_mut().push(page.url.clone());
                    stop_after_one.store(true, Ordering::Relaxed);
                }
            },
        );
        assert_eq!(seen.borrow().len(), 1);
        assert_eq!(outcome.pages.len(), 1);
        assert!(
            outcome
                .message
                .starts_with("Stopped · 1 pages on example.com"),
            "{}",
            outcome.message
        );
    }

    #[test]
    fn retry_policy_targets_transient_failures_only() {
        for status in [0, 429, 500, 502, 503, 504] {
            assert!(retryable(&Fetched {
                status,
                ..Fetched::default()
            }));
        }
        for status in [200, 301, 404, 410] {
            assert!(!retryable(&Fetched {
                status,
                ..Fetched::default()
            }));
        }
        assert_eq!(retry_wait(1), 250);
        assert_eq!(retry_wait(2), 750);
        assert_eq!(retry_wait(9), 2_000, "backoff is capped");
        let cancelled = AtomicBool::new(true);
        assert!(!sleep_cancellable(
            Duration::from_secs(30),
            Some(&cancelled)
        ));
    }

    #[test]
    fn incomplete_html_bodies_are_not_used_for_content_audits() {
        let mut page = sample_pages().remove(0);
        page.indexability.evidence.read = ReadState::Failed;
        assert!(!auditable(&page));
        page.indexability.evidence.read = ReadState::Truncated;
        assert!(!auditable(&page));
        page.indexability.evidence.read = ReadState::Complete;
        assert!(auditable(&page));
    }

    #[test]
    fn request_slots_share_the_robots_crawl_delay() {
        let delay = Duration::from_millis(25);
        let shared = Mutex::new(Work {
            crawl_delay: Some(delay),
            ..Work::default()
        });
        let wake = Condvar::new();
        let cancel = AtomicBool::new(false);
        assert!(reserve_request_slot(&shared, &wake, &cancel));
        let started = Instant::now();
        assert!(reserve_request_slot(&shared, &wake, &cancel));
        assert!(started.elapsed() >= delay);
    }

    #[test]
    fn incremental_audit_matches_a_full_rebuild() {
        let pages = sample_pages();
        let mut incremental = Audit::default();
        for page in &pages {
            incremental.push(page);
        }
        // The home page links to /promo, which only arrives as a 404 later in the list.
        incremental.rebuild_broken_links(&pages);
        let full = Audit::from_pages(&pages);
        assert_eq!(
            counts_with(&pages, &incremental),
            counts_with(&pages, &full)
        );
        for page in &pages {
            assert_eq!(incremental.inbound(page), full.inbound(page));
            assert_eq!(
                links_issue(page, &incremental),
                links_issue(page, &full),
                "{}",
                page.url
            );
            assert_eq!(title_issue(page, &incremental), title_issue(page, &full));
            assert_eq!(meta_issue(page, &incremental), meta_issue(page, &full));
            assert_eq!(h1_issue(page, &incremental), h1_issue(page, &full));
        }
    }

    #[test]
    fn audits_canonicals_images_links_and_redirect_chains() {
        let mut pages = sample_pages();
        let counts = counts(&pages);
        assert_eq!(counts.canonical, 3, "two missing, one points elsewhere");
        assert_eq!(counts.images, 2);
        assert_eq!(counts.links, 1, "the home page links to the 404");

        let audit = Audit::from_pages(&pages);
        assert_eq!(canonical_issue(&pages[0]), "");
        assert_eq!(canonical_issue(&pages[2]), "Missing");
        assert_eq!(canonical_issue(&pages[4]), "Points to /blog/calm");
        assert_eq!(image_issue(&pages[0]), "1 of 2 images without alt");
        assert_eq!(image_issue(&pages[2]), "3 images without alt");
        assert_eq!(links_issue(&pages[0], &audit), "Broken link to /promo");
        assert_eq!(audit.inbound(&pages[1]), 1);

        let mut chain = pages[6].clone();
        chain.url = "https://example.com/legacy".into();
        chain.redirect_to = "https://example.com/old-pricing".into();
        pages.push(chain);
        let audit = Audit::from_pages(&pages);
        let legacy = pages.last().unwrap();
        assert_eq!(redirect_issue(legacy, &audit), "Chain of 2");
        assert_eq!(
            redirect_issue(&pages[6], &audit),
            "",
            "a single redirect is not a chain"
        );
    }

    #[test]
    fn workers_fetch_in_parallel_instead_of_one_at_a_time() {
        let in_flight = std::sync::atomic::AtomicUsize::new(0);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        let fetch = |url: &str| {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            // Stand in for network latency so overlap is observable.
            std::thread::sleep(Duration::from_millis(5));
            in_flight.fetch_sub(1, Ordering::SeqCst);
            if url.ends_with("/robots.txt") {
                return Fetched {
                    status: 404,
                    ..Fetched::default()
                };
            }
            let n = url
                .rsplit('/')
                .next()
                .and_then(|part| part.strip_prefix('p'))
                .and_then(|part| part.parse::<usize>().ok())
                .unwrap_or(0);
            let links: String = (1..=3)
                .map(|k| format!("<a href=\"/p{}\">next</a>", n * 3 + k))
                .collect();
            html_fetch(
                200,
                &format!("<title>Parallel page {n} title that is long enough</title>{links}"),
            )
        };
        let outcome = crawl_with(
            "https://example.com/",
            Limits {
                max_pages: 40,
                max_depth: 3,
            },
            fetch,
        );
        assert_eq!(outcome.pages.len(), 40);
        assert!(
            peak.load(Ordering::SeqCst) >= 4,
            "at most {} fetches ever overlapped",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn crawls_a_local_site_over_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for _ in 0..8 {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0_u8; 2048];
                let _ = sock.read(&mut buf);
                let req = String::from_utf8_lossy(&buf);
                let path = req
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");
                let (status, extra, body) = match path {
                    "/" => (
                        "200 OK",
                        "",
                        "<title>Local home title that is long enough</title><h1>Home</h1><p>Hello local spider.</p><a href=\"/pricing\">Pricing</a><a href=\"/gone\">Gone</a><a href=\"https://other.test/no\">No</a>",
                    ),
                    "/pricing" => (
                        "200 OK",
                        "",
                        "<title>Local pricing title that is long enough</title><h1>Pricing</h1><p>Plans.</p>",
                    ),
                    "/gone" => ("404 Not Found", "", "missing"),
                    _ => ("404 Not Found", "", "no"),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        let outcome = crawl_site(&format!("http://{addr}/"));
        let urls: Vec<_> = outcome.pages.iter().map(|page| page.url.clone()).collect();
        assert!(
            urls.iter().any(|url| url.ends_with("/pricing")),
            "{urls:?} {}",
            outcome.message
        );
        assert!(
            outcome.pages.iter().any(|page| page.status == 404),
            "{urls:?}"
        );
        assert!(
            !urls.iter().any(|url| url.contains("other.test")),
            "{urls:?}"
        );
        let home = outcome
            .pages
            .iter()
            .find(|page| page.url.ends_with('/') || page.depth == 0)
            .unwrap();
        assert_eq!(home.title, "Local home title that is long enough");
        assert_eq!(home.h1s, vec!["Home".to_string()]);
    }

    #[test]
    fn repeated_header_directives_and_body_truncation_are_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0_u8; 2048];
                let _ = sock.read(&mut buf);
                let req = String::from_utf8_lossy(&buf);
                let path = req
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");
                let (extra, body) = match path {
                    "/headers" => (
                        "X-Robots-Tag: googlebot: noindex\r\nX-Robots-Tag: noindex\r\n",
                        r#"<!doctype html><html><head><title>Header evidence</title><meta name="robots" content="nofollow"></head><body><p>ok</p></body></html>"#.to_string(),
                    ),
                    "/cap" => {
                        let metas: String = (0..indexability::MAX_EVIDENCE_ITEMS)
                            .map(|_| r#"<meta name="robots" content="index">"#)
                            .collect();
                        (
                            "X-Robots-Tag: noindex\r\n",
                            format!(
                                "<!doctype html><html><head><title>Cap evidence</title>{metas}</head><body><p>ok</p></body></html>"
                            ),
                        )
                    }
                    "/big" => {
                        let mut body = String::from(
                            "<!doctype html><html><head><title>Big body</title></head><body>",
                        );
                        body.push_str(&"x".repeat(MAX_BODY_BYTES as usize));
                        body.push_str("</body></html>");
                        ("", body)
                    }
                    "/partial" => (
                        "X-Robots-Tag: noindex\r\n",
                        "<!doctype html><html><head><title>Partial page title</title></head>"
                            .to_string(),
                    ),
                    _ => ("", String::new()),
                };
                let content_length = if path == "/partial" {
                    body.len() + 64
                } else {
                    body.len()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    content_length
                );
                let _ = sock.write_all(resp.as_bytes());
            }
        });

        let headers = fetch(agent(), &format!("http://{addr}/headers"));
        assert_eq!(headers.read, ReadState::Complete);
        assert_eq!(headers.header_directives.len(), 2);
        assert!(
            headers
                .header_directives
                .iter()
                .all(|directive| directive.noindex)
        );
        assert_eq!(
            headers.header_directives[0].agent.as_deref(),
            Some("googlebot")
        );
        assert_eq!(headers.header_directives[1].agent, None);
        let parsed = parse_html(&headers.body);
        let mut directives = parsed.robots_directives.clone();
        directives.extend(headers.header_directives.clone());
        let assessment = indexability::assess(
            &format!("http://{addr}/headers"),
            headers.status,
            &headers.content_type,
            "",
            "",
            PageEvidence {
                directives,
                ..PageEvidence::default()
            },
            indexability::CaptureInfo::default(),
        );
        assert_eq!(assessment.label, indexability::Label::Noindex);
        assert!(
            assessment
                .evidence
                .directives
                .iter()
                .any(|directive| directive.nofollow && directive.agent.is_none())
        );

        let big = fetch(agent(), &format!("http://{addr}/big"));
        assert_eq!(big.read, ReadState::Truncated);
        assert_eq!(big.body.len(), MAX_BODY_BYTES as usize);
        let assessment = indexability::assess(
            &format!("http://{addr}/big"),
            big.status,
            &big.content_type,
            "",
            "",
            PageEvidence {
                read: big.read,
                ..PageEvidence::default()
            },
            indexability::CaptureInfo::default(),
        );
        assert_eq!(assessment.label, indexability::Label::NotAssessed);
        assert!(assessment.reason_text().contains("truncated"));

        // The engine appends header directives after every meta tag. A noindex header that
        // lands just past the retained cap must still win the assessment.
        let cap = fetch(agent(), &format!("http://{addr}/cap"));
        let parsed = parse_html(&cap.body);
        assert_eq!(
            parsed.robots_directives.len(),
            indexability::MAX_EVIDENCE_ITEMS
        );
        let mut directives = parsed.robots_directives.clone();
        directives.extend(cap.header_directives.clone());
        let assessment = indexability::assess(
            &format!("http://{addr}/cap"),
            cap.status,
            &cap.content_type,
            "",
            "",
            PageEvidence {
                directives,
                truncated: cap.header_truncated,
                omitted: cap.header_omitted,
                ..PageEvidence::default()
            },
            indexability::CaptureInfo::default(),
        );
        assert_eq!(assessment.label, indexability::Label::Noindex);
        assert!(assessment.has_noindex());
        assert!(assessment.has_confirmed_issue());
        assert!(assessment.evidence.truncated);
        assert!(assessment.evidence.omitted.applicable_noindex);
        assert!(
            assessment
                .evidence_text()
                .contains("among directives not retained")
        );

        let partial = fetch(agent(), &format!("http://{addr}/partial"));
        assert_eq!(partial.status, 200);
        assert_eq!(partial.read, ReadState::Failed);
        assert!(partial.body.contains("Partial page title"));
        let parsed = parse_html(&partial.body);
        let assessment = indexability::assess(
            &format!("http://{addr}/partial"),
            partial.status,
            &partial.content_type,
            "",
            &partial.error,
            PageEvidence {
                directives: partial.header_directives.clone(),
                read: partial.read,
                truncated: partial.header_truncated,
                omitted: partial.header_omitted,
                ..PageEvidence::default()
            },
            indexability::CaptureInfo::default(),
        );
        assert_eq!(assessment.label, indexability::Label::FetchFailed);
        assert!(assessment.has_noindex());
        assert!(
            assessment
                .findings
                .iter()
                .any(|finding| finding.code == indexability::FindingCode::Noindex)
        );
        let mut incomplete_page = sample_pages().remove(0);
        incomplete_page.status = partial.status;
        incomplete_page.content_type = partial.content_type;
        incomplete_page.title = parsed.title;
        incomplete_page.error = partial.error;
        incomplete_page.indexability = assessment;
        assert!(!auditable(&incomplete_page));
        assert!(title_issue(&incomplete_page, &Audit::default()).is_empty());
    }

    fn sitemap_xml(locs: &[&str]) -> sitemap::FetchedBytes {
        let urls: String = locs
            .iter()
            .map(|loc| format!("<url><loc>{loc}</loc></url>"))
            .collect();
        sitemap::FetchedBytes {
            status: 200,
            content_type: "application/xml".into(),
            body: format!(
                r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{urls}</urlset>"#
            )
            .into_bytes(),
            ..sitemap::FetchedBytes::default()
        }
    }

    fn missing_sitemap(status: u16) -> sitemap::FetchedBytes {
        sitemap::FetchedBytes {
            status,
            ..sitemap::FetchedBytes::default()
        }
    }

    #[test]
    fn sitemap_urls_are_fetched_once_alongside_the_link_crawl() {
        let fetch = |url: &str| match url {
            "https://example.com/robots.txt" => Fetched {
                status: 404,
                ..Fetched::default()
            },
            "https://example.com/" => {
                html_fetch(200, "<title>Home title that is long enough</title>")
            }
            "https://example.com/only" => {
                html_fetch(200, "<title>Sitemap only title that is long enough</title>")
            }
            "https://example.com/shared" => {
                html_fetch(200, "<title>Shared title that is long enough</title>")
            }
            other => Fetched {
                error: format!("unexpected {other}"),
                ..Fetched::default()
            },
        };
        let fetch_sitemap = |url: &str| match url {
            "https://example.com/sitemap.xml" => sitemap_xml(&[
                "https://example.com/only",
                "https://example.com/shared",
                "https://example.com/",
            ]),
            _ => missing_sitemap(404),
        };
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 10,
                max_depth: 0,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            fetch_sitemap,
            None,
            |_| {},
        );
        let urls: Vec<_> = result.pages.iter().map(|page| page.url.as_str()).collect();
        for expected in [
            "https://example.com/",
            "https://example.com/only",
            "https://example.com/shared",
        ] {
            assert_eq!(
                urls.iter().filter(|url| **url == expected).count(),
                1,
                "{urls:?}"
            );
        }
        assert_eq!(result.sitemap.urls.len(), 3);
        assert!(
            result
                .sitemap
                .urls
                .iter()
                .all(|entry| entry.state == sitemap::UrlState::Checked),
            "{:?}",
            result.sitemap.urls
        );
        assert_eq!(result.sitemap.summary().checked, 3);
        assert!(
            result
                .sitemap
                .files
                .iter()
                .any(|file| file.origin == sitemap::SitemapOrigin::Guessed)
        );
    }

    #[test]
    fn robots_declarations_supply_sitemaps_and_block_listed_urls() {
        let requested = Mutex::new(Vec::new());
        let fetch = |url: &str| {
            requested.lock().unwrap().push(url.to_string());
            match url {
                "https://example.com/robots.txt" => Fetched {
                    status: 200,
                    content_type: "text/plain".into(),
                    body:
                        "User-agent: *\nDisallow: /private\nSitemap: https://example.com/map.xml\n"
                            .into(),
                    ..Fetched::default()
                },
                "https://example.com/" => {
                    html_fetch(200, "<title>Home title that is long enough</title>")
                }
                "https://example.com/a" => {
                    html_fetch(200, "<title>Allowed title that is long enough</title>")
                }
                other => Fetched {
                    error: format!("unexpected {other}"),
                    ..Fetched::default()
                },
            }
        };
        let fetch_sitemap = |url: &str| match url {
            "https://example.com/map.xml" => {
                sitemap_xml(&["https://example.com/a", "https://example.com/private/b"])
            }
            other => panic!("unexpected sitemap fetch: {other}"),
        };
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 10,
                max_depth: 0,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            fetch_sitemap,
            None,
            |_| {},
        );
        assert_eq!(result.sitemap.files.len(), 1);
        assert_eq!(
            result.sitemap.files[0].origin,
            sitemap::SitemapOrigin::Robots
        );
        let blocked = result.sitemap.url("https://example.com/private/b").unwrap();
        assert_eq!(blocked.state, sitemap::UrlState::Blocked);
        let requested = requested.into_inner().unwrap();
        assert!(
            !requested
                .iter()
                .any(|url| url.contains("/private/b") || url.ends_with("/sitemap.xml")),
            "{requested:?}"
        );
    }

    #[test]
    fn the_combined_page_cap_marks_unvisited_sitemap_urls_not_checked() {
        let fetch = |url: &str| match url {
            "https://example.com/robots.txt" => Fetched {
                status: 404,
                ..Fetched::default()
            },
            _ => html_fetch(200, "<title>A page title that is comfortably long</title>"),
        };
        let fetch_sitemap = |url: &str| match url {
            "https://example.com/sitemap.xml" => sitemap_xml(&[
                "https://example.com/a",
                "https://example.com/b",
                "https://example.com/c",
            ]),
            _ => missing_sitemap(404),
        };
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 2,
                max_depth: 0,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            fetch_sitemap,
            None,
            |_| {},
        );
        assert_eq!(result.pages.len(), 2);
        assert!(result.message.contains("stopped"), "{}", result.message);
        let summary = result.sitemap.summary();
        assert!(summary.not_checked >= 1, "{summary:?}");
        assert!(
            result
                .sitemap
                .urls
                .iter()
                .filter(|entry| entry.state == sitemap::UrlState::NotChecked)
                .all(|entry| entry.issue == "Page limit"),
            "{:?}",
            result.sitemap.urls
        );
    }

    #[test]
    fn gzip_sitemaps_are_read_and_malformed_ones_do_not_stop_the_crawl() {
        let fetch = |url: &str| match url {
            "https://example.com/robots.txt" => Fetched {
                status: 404,
                ..Fetched::default()
            },
            _ => html_fetch(200, "<title>A page title that is comfortably long</title>"),
        };
        let gzipped = {
            use std::io::Write;
            let body = br#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>https://example.com/zipped</loc></url></urlset>"#;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(body).unwrap();
            encoder.finish().unwrap()
        };
        let fetch_sitemap = move |url: &str| match url {
            "https://example.com/sitemap.xml" => sitemap::FetchedBytes {
                status: 200,
                content_type: "application/gzip".into(),
                body: gzipped.clone(),
                ..sitemap::FetchedBytes::default()
            },
            _ => missing_sitemap(404),
        };
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 10,
                max_depth: 0,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            fetch_sitemap,
            None,
            |_| {},
        );
        assert_eq!(result.sitemap.files[0].state, sitemap::FileState::Ok);
        assert!(
            result
                .pages
                .iter()
                .any(|page| page.url == "https://example.com/zipped")
        );

        let broken = |url: &str| match url {
            "https://example.com/sitemap.xml" => sitemap::FetchedBytes {
                status: 200,
                body: b"<urlset><url>".to_vec(),
                ..sitemap::FetchedBytes::default()
            },
            _ => missing_sitemap(404),
        };
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 10,
                max_depth: 0,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            broken,
            None,
            |_| {},
        );
        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.sitemap.files[0].state, sitemap::FileState::Error);
        assert!(result.sitemap.partial.is_some());
    }

    #[test]
    fn sitemap_entries_carry_the_pages_own_findings() {
        let fetch = |url: &str| match url {
            "https://example.com/robots.txt" => Fetched {
                status: 404,
                ..Fetched::default()
            },
            "https://example.com/" => {
                html_fetch(200, "<title>Home title that is long enough</title>")
            }
            "https://example.com/gone" => Fetched {
                status: 404,
                content_type: "text/html".into(),
                body: "missing".into(),
                ..Fetched::default()
            },
            "https://example.com/noindex" => html_fetch(
                200,
                r#"<title>Noindex page title that is long enough</title><meta name="robots" content="noindex">"#,
            ),
            "https://example.com/canonical" => html_fetch(
                200,
                r#"<title>Canonical page title that is long enough</title><link rel="canonical" href="/elsewhere">"#,
            ),
            other => Fetched {
                error: format!("unexpected {other}"),
                ..Fetched::default()
            },
        };
        let fetch_sitemap = |url: &str| match url {
            "https://example.com/sitemap.xml" => sitemap_xml(&[
                "https://example.com/gone",
                "https://example.com/noindex",
                "https://example.com/canonical",
            ]),
            _ => missing_sitemap(404),
        };
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 10,
                max_depth: 0,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            fetch_sitemap,
            None,
            |_| {},
        );
        let entry = |url: &str| result.sitemap.url(url).unwrap();
        assert_eq!(entry("https://example.com/gone").status, 404);
        assert!(entry("https://example.com/gone").issue.contains("404"));
        assert!(
            entry("https://example.com/noindex")
                .issue
                .contains("Meta robots: noindex")
        );
        assert!(
            entry("https://example.com/canonical")
                .issue
                .contains("Canonical points to /elsewhere")
        );
        assert_eq!(
            entry("https://example.com/noindex").assessment.label,
            crate::indexability::Label::Noindex
        );
        assert_eq!(
            entry("https://example.com/canonical").assessment.label,
            crate::indexability::Label::CanonicalElsewhere
        );
        for page in &result.pages {
            if let Some(entry) = result.sitemap.url(&page.url) {
                assert_eq!(
                    *entry.assessment, page.indexability,
                    "sitemap and page assessment disagree for {}",
                    page.url
                );
                assert_eq!(
                    entry.issue,
                    page.indexability.reason_text(),
                    "sitemap reason disagrees for {}",
                    page.url
                );
            }
        }
    }

    #[test]
    fn zero_page_limit_skips_all_work_even_with_sitemaps() {
        let fetch = |_: &str| panic!("page fetch with a zero budget");
        let fetch_sitemap = |_: &str| panic!("sitemap fetch with a zero budget");
        let cancel = AtomicBool::new(false);
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 0,
                max_depth: 2,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            fetch_sitemap,
            Some(&cancel),
            |_| {},
        );
        assert!(result.pages.is_empty());
        assert!(result.sitemap.files.is_empty());
        assert!(result.message.contains("0 pages"));
    }

    #[test]
    fn stopping_before_discovery_finishes_marks_the_sitemap_report_partial() {
        let cancel = AtomicBool::new(true);
        let fetch = |url: &str| match url {
            "https://example.com/robots.txt" => Fetched {
                status: 404,
                ..Fetched::default()
            },
            _ => panic!("page fetch after cancellation"),
        };
        let fetch_sitemap = |_: &str| panic!("sitemap fetch after cancellation");
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 4,
                max_depth: 0,
            },
            CrawlOptions {
                sitemaps: true,
                ..CrawlOptions::default()
            },
            fetch,
            fetch_sitemap,
            Some(&cancel),
            |_| {},
        );
        assert!(result.message.starts_with("Stopped"));
        assert!(result.sitemap.discovery_done);
        assert_eq!(result.sitemap.partial.as_deref(), Some("Stopped"));
        assert!(result.sitemap.files.is_empty());
    }

    #[test]
    fn turning_sitemaps_off_keeps_the_crawl_links_only() {
        let requested = Mutex::new(Vec::new());
        let fetch = |url: &str| {
            requested.lock().unwrap().push(url.to_string());
            match url {
                "https://example.com/robots.txt" => Fetched {
                    status: 404,
                    ..Fetched::default()
                },
                _ => html_fetch(200, "<title>A page title that is comfortably long</title>"),
            }
        };
        let fetch_sitemap = |_: &str| panic!("sitemap fetch while disabled");
        let result = crawl_with_options(
            "https://example.com/",
            Limits {
                max_pages: 4,
                max_depth: 0,
            },
            CrawlOptions::default(),
            fetch,
            fetch_sitemap,
            None,
            |_| {},
        );
        assert_eq!(result.pages.len(), 1);
        assert!(result.sitemap.files.is_empty());
        assert!(result.sitemap.urls.is_empty());
        assert!(result.sitemap.discovery_done);
        let requested = requested.into_inner().unwrap();
        assert!(
            !requested.iter().any(|url| url.ends_with("/sitemap.xml")),
            "{requested:?}"
        );
    }
}
