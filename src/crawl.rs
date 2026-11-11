//! One-host crawl and the four on-page audits around it.
//!
//! The spider stores every fetched URL. Titles, meta descriptions and H1s are judged only on
//! HTTP 200 HTML. Response codes cover every URL, including redirects, 404s and 500s.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasherDefault;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use scraper::{ElementRef, Html, Node};
use url::Url;

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

const USER_AGENT: &str = "Calmtoad/0.1 (SEO spider; +https://example.com/calmtoad)";
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
    pub noindex: bool,
    pub words: usize,
    pub redirect_to: String,
    pub error: String,
    pub depth: u8,
    /// Absolute canonical URL, normalized like `url`, when the page declares one.
    pub canonical: String,
    pub images: usize,
    pub images_missing_alt: usize,
    /// Same-host links found on the page, normalized like `url`.
    pub links: Vec<String>,
}

impl Page {
    pub fn indexable(&self) -> bool {
        auditable(self) && !self.noindex
    }

    pub fn h1(&self) -> String {
        self.h1s.join(" · ")
    }
}

#[derive(Clone, Debug)]
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
    /// `X-Robots-Tag: noindex`, which can come from the server as well as the markup.
    pub header_noindex: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// The streaming UI consumes pages as they land; the final outcome keeps them for callers
    /// that want the whole set at once (and for tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub pages: Vec<Page>,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
struct Robots {
    rules: Vec<RobotsRule>,
    crawl_delay_ms: u64,
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
        for raw in body.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            let field = field.trim();
            let value = value.trim();
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
            for link in &page.links {
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
        for link in &page.links {
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
            for link in &page.links {
                if self.broken_targets.contains(link) {
                    self.record_broken_link(page, link);
                }
            }
        }
    }

    pub fn inbound(&self, page: &Page) -> usize {
        self.inbound.get(&page.url).copied().unwrap_or(0)
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
        counts.blocked += (!(auditable && !page.noindex)) as usize;
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
    auditable && (page.canonical.is_empty() || page.canonical != page.url)
}

pub fn auditable(page: &Page) -> bool {
    page.status == 200 && is_html(&page.content_type, "") && page.error.is_empty()
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
    if page.canonical.is_empty() {
        return "Missing".into();
    }
    if page.canonical == page.url {
        String::new()
    } else {
        format!("Points to {}", short_target(&page.canonical))
    }
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

pub fn index_issue(page: &Page) -> &'static str {
    if page.indexable() {
        ""
    } else if page.noindex {
        "Noindex"
    } else if page.status != 200 {
        "Not indexable"
    } else {
        "Not HTML"
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
    crawl_engine(
        seed,
        Limits::default(),
        |url| fetch(agent(), url),
        true,
        None,
        |_| {},
    )
}

/// Crawl while reporting every page as soon as it lands. `cancel` is polled between pages so
/// the UI can stop a crawl without waiting for the whole queue to drain.
pub fn crawl_site_stream(
    seed: &str,
    limits: Limits,
    cancel: &AtomicBool,
    on_page: impl FnMut(&Page),
) -> Outcome {
    crawl_engine(
        seed,
        limits,
        |url| fetch(agent(), url),
        true,
        Some(cancel),
        on_page,
    )
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
struct FetchedPage {
    url: String,
    depth: u8,
    fetched: Fetched,
    parsed: Option<Parsed>,
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

#[cfg_attr(not(test), allow(dead_code))]
pub fn crawl_with(seed: &str, limits: Limits, fetch: impl Fn(&str) -> Fetched + Sync) -> Outcome {
    crawl_engine(seed, limits, fetch, false, None, |_| {})
}

fn crawl_engine(
    seed: &str,
    limits: Limits,
    fetch: impl Fn(&str) -> Fetched + Sync,
    cache_robots: bool,
    cancel: Option<&AtomicBool>,
    mut on_page: impl FnMut(&Page),
) -> Outcome {
    let cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed));
    let seed = match normalize_seed(seed) {
        Ok(seed) => seed,
        Err(message) => {
            return Outcome {
                message,
                ..Outcome::default()
            };
        }
    };
    let host = host_of(&seed).unwrap_or_default();

    if limits.max_pages == 0 {
        return Outcome {
            pages: Vec::new(),
            message: format!("0 pages on {host}"),
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
        return Outcome {
            pages: Vec::new(),
            message: format!("Blocked by robots.txt · {host}"),
        };
    }

    // Reserve a modest starting amount. The user can set a large page limit, so
    // allocating the full crawl budget before discovering any URLs is wasteful.
    let mut seen: FastSet<String> = fast_set(limits.max_pages.min(256).saturating_mul(4).max(16));
    seen.insert(seed.clone());
    let mut pages = Vec::with_capacity(limits.max_pages.min(256));
    let mut committed = 1_usize;
    let mut over_cap = false;
    let mut stopped_early = false;

    let shared = Mutex::new(Work {
        queue: VecDeque::from([(seed, 0_u8)]),
        crawl_delay: robots.as_ref().and_then(Robots::crawl_delay),
        ..Work::default()
    });
    let wake = Condvar::new();
    let workers = limits.max_pages.min(PARALLEL_REQUESTS);

    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<FetchedPage>();

        for _ in 0..workers {
            let tx = tx.clone();
            let shared = &shared;
            let wake = &wake;
            let fetch = &fetch;
            scope.spawn(move || {
                loop {
                    let (url, depth) = {
                        let mut work = shared.lock().unwrap();
                        loop {
                            if work.done {
                                return;
                            }
                            let now = Instant::now();
                            // A 429/503 pauses the whole pool, not just one worker.
                            if let Some(until) = work.pause_until {
                                if until > now {
                                    let (guard, _) = wake.wait_timeout(work, until - now).unwrap();
                                    work = guard;
                                    continue;
                                }
                                work.pause_until = None;
                            }
                            // Crawl-delay is a minimum spacing between request starts.
                            if let Some(next_start) = work.next_start {
                                let now = Instant::now();
                                if next_start > now {
                                    let (guard, _) =
                                        wake.wait_timeout(work, next_start - now).unwrap();
                                    work = guard;
                                    continue;
                                }
                            }
                            if let Some(job) = work.queue.pop_front() {
                                if let Some(delay) = work.crawl_delay {
                                    let start = work.next_start.map_or(now, |next| next.max(now));
                                    work.next_start = Some(start + delay);
                                }
                                break job;
                            }
                            work = wake.wait(work).unwrap();
                        }
                    };
                    // A panicking fetcher must still produce a result, or the crawl loop would
                    // wait forever for this URL.
                    let (fetched, parsed) = catch_unwind(AssertUnwindSafe(|| {
                        let fetched = fetch(&url);
                        let parsed = if fetched.status == 200
                            && is_html(&fetched.content_type, &fetched.body)
                        {
                            Some(parse_html(&fetched.body))
                        } else {
                            None
                        };
                        // The body is no longer needed once parsed; dropping it here keeps large
                        // pages from crossing the channel into the crawl loop.
                        let mut fetched = fetched;
                        fetched.body = String::new();
                        (fetched, parsed)
                    }))
                    .unwrap_or_else(|_| {
                        (
                            Fetched {
                                error: "crawler worker panicked".into(),
                                ..Fetched::default()
                            },
                            None,
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
                        .send(FetchedPage {
                            url,
                            depth,
                            fetched,
                            parsed,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        drop(tx);
        let _finish = Finish {
            shared: &shared,
            wake: &wake,
        };

        let mut pending = 1_usize;
        while pending > 0 {
            if cancelled() {
                stopped_early = true;
                break;
            }
            let result = match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            pending -= 1;

            let FetchedPage {
                url,
                depth,
                fetched,
                parsed,
            } = result;
            let page_url = if parsed.is_some() || (300..400).contains(&fetched.status) {
                Url::parse(&url).ok()
            } else {
                None
            };
            let mut new_jobs: Vec<(String, u8)> = Vec::new();
            let mut page_links: Vec<String> = Vec::new();
            let mut canonical = String::new();

            if let (Some(parsed), Some(page_url)) = (parsed.as_ref(), page_url.as_ref()) {
                let link_base = parsed
                    .base
                    .as_deref()
                    .and_then(|href| resolve(page_url, href))
                    .unwrap_or_else(|| page_url.clone());
                canonical = parsed
                    .canonical
                    .as_deref()
                    .and_then(|href| resolve(&link_base, href))
                    .map(url_key)
                    .unwrap_or_default();
                for href in &parsed.links {
                    let Some(next) = resolve(&link_base, href) else {
                        continue;
                    };
                    if !same_host(&next, &host) || looks_like_asset(&next) {
                        continue;
                    }
                    if robots
                        .as_ref()
                        .is_some_and(|robots| !robots.allows(&request_path(&next)))
                    {
                        continue;
                    }
                    // Every same-host link counts for the link audit, even when the page was
                    // already seen or we are too deep to follow it.
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
            }

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

            let had_html = parsed.is_some();
            let (title, meta_description, h1s, noindex, words, images, images_missing_alt) =
                match parsed {
                    Some(parsed) => (
                        parsed.title,
                        parsed.meta,
                        parsed.h1s,
                        parsed.noindex || fetched.header_noindex,
                        parsed.words,
                        parsed.images,
                        parsed.images_missing_alt,
                    ),
                    None => (
                        String::new(),
                        String::new(),
                        Vec::new(),
                        fetched.header_noindex,
                        0,
                        0,
                        0,
                    ),
                };
            let content_type = if fetched.content_type.is_empty() && had_html {
                "text/html".into()
            } else {
                fetched.content_type
            };
            let page = Page {
                url,
                status: fetched.status,
                content_type,
                title,
                meta_description,
                h1s,
                noindex,
                words,
                redirect_to,
                error: fetched.error,
                depth,
                canonical,
                images,
                images_missing_alt,
                links: page_links,
            };
            on_page(&page);
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

    let mut message = if stopped_early {
        format!("Stopped · {} pages on {host}", pages.len())
    } else {
        format!("{} pages on {host}", pages.len())
    };
    if over_cap && !stopped_early {
        message.push_str(&format!(
            " · stopped at the {} page limit",
            limits.max_pages
        ));
    }
    if !stopped_early && pages.iter().all(|page| page.status == 0) {
        let why = pages
            .first()
            .map(|page| page.error.as_str())
            .filter(|e| !e.is_empty())
            .unwrap_or("no response");
        message = format!("Crawl failed · {why}");
    }
    Outcome { pages, message }
}

struct Parsed {
    title: String,
    meta: String,
    h1s: Vec<String>,
    noindex: bool,
    words: usize,
    links: Vec<String>,
    /// `<base href>`, resolved by the crawler against the page URL before joining links.
    base: Option<String>,
    canonical: Option<String>,
    images: usize,
    images_missing_alt: usize,
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
fn parse_html(body: &str) -> Parsed {
    let doc = Html::parse_document(body);

    let mut title: Option<String> = None;
    let mut meta = String::new();
    let mut noindex = false;
    let mut saw_description = false;
    let mut saw_robots = false;
    let mut saw_googlebot = false;
    let mut h1s = Vec::new();
    let mut links = Vec::new();
    let mut base: Option<String> = None;
    let mut canonical: Option<String> = None;
    let mut saw_canonical = false;
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
                    if !(saw_description && saw_robots && saw_googlebot) {
                        let name = element.attr("name").unwrap_or("");
                        if !saw_description && name.eq_ignore_ascii_case("description") {
                            saw_description = true;
                            meta = element.attr("content").map(clean).unwrap_or_default();
                        } else if !saw_robots && name.eq_ignore_ascii_case("robots") {
                            saw_robots = true;
                            noindex = element.attr("content").is_some_and(is_noindex);
                        } else if !saw_googlebot && name.eq_ignore_ascii_case("googlebot") {
                            saw_googlebot = true;
                            noindex |= element.attr("content").is_some_and(is_noindex);
                        }
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
                    if let Some(href) = element.attr("href") {
                        links.push(href.to_string());
                    }
                }
                "base" => {
                    if base.is_none() {
                        base = element.attr("href").map(str::to_string);
                    }
                }
                "link" => {
                    if !saw_canonical
                        && element.attr("rel").is_some_and(|rel| {
                            rel.split_whitespace()
                                .any(|token| token.eq_ignore_ascii_case("canonical"))
                        })
                    {
                        saw_canonical = true;
                        canonical = element.attr("href").map(str::to_string);
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

    Parsed {
        title: title.unwrap_or_default(),
        meta,
        h1s,
        noindex,
        words,
        links,
        base,
        canonical,
        images,
        images_missing_alt,
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
    let header_noindex = response.header("x-robots-tag").is_some_and(has_noindex);
    let mut body = String::new();
    // Only a 200 is parsed, so downloading the body of a redirect or an error page is wasted
    // bandwidth and time.
    if status == 200 && wants_body(&content_type) {
        let capacity = response
            .header("content-length")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0)
            .min(MAX_BODY_BYTES as usize);
        let mut bytes = Vec::with_capacity(capacity);
        if response
            .into_reader()
            .take(MAX_BODY_BYTES)
            .read_to_end(&mut bytes)
            .is_ok()
        {
            body = match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
            };
        }
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
        header_noindex,
    }
}

fn wants_body(content_type: &str) -> bool {
    content_type.is_empty()
        || contains_ascii_ci(content_type, "html")
        || contains_ascii_ci(content_type, "text/plain")
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

fn url_key(mut url: Url) -> String {
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

fn is_noindex(content: &str) -> bool {
    content
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("noindex"))
}

/// `X-Robots-Tag` may be a comma-separated list, optionally prefixed with a crawler name
/// (`googlebot: noindex`).
fn has_noindex(value: &str) -> bool {
    value
        .split(',')
        .any(|part| is_noindex(part.rsplit(':').next().unwrap_or(part).trim()))
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
        links: Vec::new(),
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
    pages[0].links = vec![
        "https://example.com/pricing".into(),
        "https://example.com/promo".into(),
        "https://example.com/blog/calm".into(),
    ];
    pages[3].links = vec![
        "https://example.com/blog/notes".into(),
        "https://example.com/guides/launch".into(),
    ];
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn html_fetch(status: u16, body: &str) -> Fetched {
        Fetched {
            status,
            content_type: "text/html; charset=utf-8".into(),
            body: body.into(),
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
        assert!(parsed.noindex);
        assert_eq!(parsed.canonical.as_deref(), Some("/canonical"));
        assert_eq!(parsed.images, 3);
        assert_eq!(parsed.images_missing_alt, 2);
        assert!(parsed.links.contains(&"/pricing".to_string()));
        assert!(parsed.links.iter().any(|href| href.contains("other.test")));
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
        assert_eq!(index_issue(&pages[5]), "Noindex");
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
        let outcome = crawl_with("https://example.com/", limits.clone(), fetch);
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
            false,
            Some(&no_stop),
            |page| all.borrow_mut().push(page.url.clone()),
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
            false,
            Some(&stop_after_one),
            |page| {
                seen.borrow_mut().push(page.url.clone());
                stop_after_one.store(true, Ordering::Relaxed);
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

        assert!(has_noindex("noindex"));
        assert!(has_noindex("googlebot: noindex"));
        assert!(has_noindex("noindex, nofollow"));
        assert!(!has_noindex("index, follow"));
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
}
