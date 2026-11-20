//! Duplicate and near-duplicate content evidence over already captured pages.
//!
//! ## Selection and text rules (policy v1)
//!
//! * One subtree is selected per page, in this order: the first nonempty `main` or
//!   `[role="main"]`, then the largest nonempty `article` (earlier DOM order wins ties), then
//!   `body`.
//! * `script`, `style`, `template`, `noscript`, `nav`, `footer`, `aside`, navigation-role
//!   subtrees, and explicitly hidden or inert subtrees are excluded. `header` is excluded only
//!   on the body fallback; a header inside the selected main/article remains content.
//! * Block elements and `br` create a text boundary; inline elements do not. Thus
//!   `co<em>op</em>` becomes `coop`, while adjacent paragraphs stay separate words.
//! * The text is DOM text, not a claim of CSS-visible text. Source fallback is never treated as
//!   rendered content.
//!
//! Exact normalization is ASCII case folding, Unicode-whitespace collapse, and preservation of
//! punctuation, digits, and order. SHA-256 covers the entire normalized document. A page over
//! the retention bound is excluded rather than hashing a prefix.
//!
//! Near analysis tokenizes Unicode alphanumeric runs (combining marks stay in a token) with
//! ASCII folding. Punctuation separates tokens. It keeps unique five-token shingles, verifies
//! Jaccard on the retained shingle sets, and displays verified overlap rather than signature
//! agreement. Unspaced scripts and non-ASCII case pairs are documented limitations.

use std::collections::{BTreeSet, HashMap, HashSet};

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use ego_tree::NodeRef;
use scraper::{ElementRef, Html, Node};
use sha2::{Digest, Sha256};

use crate::indexability::EvidenceStage;

pub const POLICY_VERSION: u8 = 1;
pub const MIN_TOKENS: usize = 50;
pub const MAX_NEAR_TOKENS: usize = 10_000;
pub const MAX_TOKEN_CHARS: usize = 512;
pub const MAX_NORMALIZED_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_EXCERPT_CHARS: usize = 2_000;
pub const SHINGLE_TOKENS: usize = 5;
pub const SIGNATURE_COORDS: usize = 120;
pub const LSH_BANDS: usize = 24;
pub const LSH_ROWS: usize = 5;

pub const MAX_BUCKET_ENTRIES: usize = 256;
pub const MAX_CANDIDATES_PER_REP: usize = 200;
pub const MAX_VERIFICATIONS: usize = 100_000;
pub const MAX_NEAR_PAIRS: usize = 10_000;
pub const CONTENT_MEMORY_BUDGET: usize = 512 * 1024 * 1024;

/// URL copies retained at once by analysis metadata: the input URL, the page map key, the
/// URL-state map key, and the Not compared diagnostic.
const METADATA_URL_COPIES: usize = 4;
/// Structural overhead charged for each retained URL copy (String header and map bucket).
const METADATA_COPY_OVERHEAD: usize = 64;
/// Fixed retained scaffolding per input beyond the URL payload: map capacity, the NotCompared
/// row and reason, and temporary URL-index entries.
const METADATA_INPUT_OVERHEAD: usize = 384;

/// A shared, bounded near-analysis allocation counter for one crawl.
#[derive(Debug, Default)]
pub struct ContentBudget {
    used: AtomicUsize,
}

impl ContentBudget {
    pub fn try_reserve(&self, bytes: usize) -> bool {
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > CONTENT_MEMORY_BUDGET {
                return false;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    pub fn with_used(used: usize) -> Self {
        Self {
            used: AtomicUsize::new(used.min(CONTENT_MEMORY_BUDGET)),
        }
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    pub fn remaining(&self) -> usize {
        CONTENT_MEMORY_BUDGET.saturating_sub(self.used())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ContentScope {
    Main,
    Article,
    #[default]
    Body,
}

impl ContentScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::Article => "Article",
            Self::Body => "Body fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NearState {
    #[default]
    NotEligible,
    Eligible,
    TooShort,
    TooLarge,
    ResourceLimited,
}

impl NearState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotEligible => "Not eligible",
            Self::Eligible => "Compared",
            Self::TooShort => "Too short",
            Self::TooLarge => "Too large",
            Self::ResourceLimited => "Resource limited",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnalysisState {
    #[default]
    Pending,
    Compared,
    CoveredByExactGroup,
    Ineligible,
    TooShort,
    TooLarge,
    ResourceLimited,
    SearchLimited,
    Cancelled,
}

impl AnalysisState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Compared => "Compared",
            Self::CoveredByExactGroup => "Covered by exact group",
            Self::Ineligible => "Not eligible",
            Self::TooShort => "Too short",
            Self::TooLarge => "Too large",
            Self::ResourceLimited => "Resource limited",
            Self::SearchLimited => "Search limited",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn is_compared(self) -> bool {
        matches!(self, Self::Compared | Self::CoveredByExactGroup)
    }
}

pub type Signature = Arc<[u64; SIGNATURE_COORDS]>;

/// Immutable content facts retained on a page. Heavy shingle/signature storage is shared through
/// `Arc` so page clones, inventory snapshots, and UI frames never deep-copy it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentEvidence {
    pub exact_eligible: bool,
    /// Why exact comparison is unavailable, when it is.
    pub exact_reason: Arc<str>,
    pub stage: EvidenceStage,
    pub policy_version: u8,
    pub scope: Option<ContentScope>,
    pub hash: [u8; 32],
    pub normalized_chars: usize,
    pub token_count: usize,
    pub near_state: NearState,
    pub near_reason: Arc<str>,
    pub excerpt: Arc<str>,
    pub excerpt_truncated: bool,
    pub shingles: Arc<[u64]>,
    pub signature: Option<Signature>,
}

impl Default for ContentEvidence {
    fn default() -> Self {
        Self {
            exact_eligible: false,
            exact_reason: Arc::from(""),
            stage: EvidenceStage::Document,
            policy_version: POLICY_VERSION,
            scope: None,
            hash: [0; 32],
            normalized_chars: 0,
            token_count: 0,
            near_state: NearState::NotEligible,
            near_reason: Arc::from(""),
            excerpt: Arc::from(""),
            excerpt_truncated: false,
            shingles: Arc::from([]),
            signature: None,
        }
    }
}

impl ContentEvidence {
    pub fn ineligible(reason: impl Into<Arc<str>>, stage: EvidenceStage) -> Self {
        Self {
            exact_reason: reason.into(),
            stage,
            ..Self::default()
        }
    }

    pub fn hash_hex(&self) -> String {
        hash_hex(&self.hash)
    }

    pub fn near_is_available(&self) -> bool {
        self.near_state == NearState::Eligible
            && self.signature.is_some()
            && self.shingles.len() >= 2
    }

    /// Approximate retained bytes owned by this evidence, including excerpt, shingles and
    /// signature. Used to seed the analysis cap with evidence retained during the crawl.
    pub fn retained_bytes(&self) -> usize {
        self.excerpt
            .len()
            .saturating_add(
                self.shingles
                    .len()
                    .saturating_mul(std::mem::size_of::<u64>()),
            )
            .saturating_add(if self.signature.is_some() {
                std::mem::size_of::<[u64; SIGNATURE_COORDS]>()
            } else {
                0
            })
            .saturating_add(256)
    }
}

fn hash_hex(hash: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Bound structural selection work even for adversarial DOMs. Reaching it marks extraction
/// incomplete rather than hashing a silently truncated prefix.
pub const MAX_TEXT_NODES: usize = 2_000_000;

struct SelectedContent {
    text: String,
    scope: ContentScope,
    complete: bool,
}

/// Extract content from the DOM already parsed for the selected audit capture.
///
/// `capture_complete` is the caller's eligibility decision from response status, HTML type, read
/// state, and Browser rendered-capture state. Ineligible captures retain only a reason.
pub fn extract(
    doc: &Html,
    stage: EvidenceStage,
    capture_complete: bool,
    budget: &ContentBudget,
) -> ContentEvidence {
    extract_with_cancel(doc, stage, capture_complete, budget, None)
}

/// Extraction variant with a crawl cancellation checkpoint through structural traversal.
pub fn extract_with_cancel(
    doc: &Html,
    stage: EvidenceStage,
    capture_complete: bool,
    budget: &ContentBudget,
    cancel: Option<&AtomicBool>,
) -> ContentEvidence {
    if !capture_complete {
        return ContentEvidence::ineligible("Capture is not complete", stage);
    }
    let selected = select_content(doc, cancel);
    if !selected.complete {
        return ContentEvidence {
            stage,
            scope: Some(selected.scope),
            exact_reason: Arc::from(
                "Selected text extraction was incomplete or cancelled before full traversal",
            ),
            ..ContentEvidence::default()
        };
    }
    let (normalized, too_large) = normalize(&selected.text);
    if normalized.is_empty() {
        return ContentEvidence {
            stage,
            scope: Some(selected.scope),
            exact_reason: Arc::from("Selected text is empty"),
            ..ContentEvidence::default()
        };
    }
    if too_large {
        return ContentEvidence {
            stage,
            scope: Some(selected.scope),
            normalized_chars: normalized.chars().count(),
            exact_reason: Arc::from("Selected text exceeds the 2 MiB retention limit"),
            ..ContentEvidence::default()
        };
    }
    let hash: [u8; 32] = Sha256::digest(normalized.as_bytes()).into();
    let (excerpt, excerpt_truncated) = excerpt(&normalized, MAX_EXCERPT_CHARS);
    let mut evidence = ContentEvidence {
        stage,
        scope: Some(selected.scope),
        hash,
        normalized_chars: normalized.chars().count(),
        excerpt: Arc::from(excerpt),
        excerpt_truncated,
        ..ContentEvidence::default()
    };
    let exact_bytes = evidence.excerpt.len().saturating_add(256);
    if !budget.try_reserve(exact_bytes) {
        return ContentEvidence {
            stage,
            scope: Some(selected.scope),
            exact_reason: Arc::from(
                "Content-analysis memory budget reached before exact evidence was retained",
            ),
            ..ContentEvidence::default()
        };
    }
    evidence.exact_eligible = true;
    let tokenized = tokenize(&normalized);
    evidence.token_count = tokenized.tokens.len();
    if tokenized.too_many {
        evidence.near_state = NearState::TooLarge;
        evidence.near_reason = Arc::from("More than 10,000 tokens; near analysis skipped");
        return evidence;
    }
    if tokenized.oversized_token {
        evidence.near_state = NearState::TooLarge;
        evidence.near_reason = Arc::from("Token exceeds 512 characters; near analysis skipped");
        return evidence;
    }
    if evidence.token_count < MIN_TOKENS {
        evidence.near_state = NearState::TooShort;
        evidence.near_reason = Arc::from(format!(
            "{} tokens; near comparison needs at least {MIN_TOKENS}",
            evidence.token_count
        ));
        return evidence;
    }
    let shingles = shingles(&tokenized.tokens);
    if shingles.len() < 2 {
        evidence.near_state = NearState::TooShort;
        evidence.near_reason = Arc::from("No distinct five-token shingles");
        return evidence;
    }
    let signature = signature(&shingles);
    let bytes = shingles
        .len()
        .saturating_mul(std::mem::size_of::<u64>())
        .saturating_add(std::mem::size_of::<[u64; SIGNATURE_COORDS]>())
        .saturating_add(128);
    if !budget.try_reserve(bytes) {
        evidence.near_state = NearState::ResourceLimited;
        evidence.near_reason =
            Arc::from("Run content-analysis memory budget reached; near analysis skipped");
        return evidence;
    }
    evidence.shingles = Arc::from(shingles);
    evidence.signature = Some(Arc::new(signature));
    evidence.near_state = NearState::Eligible;
    evidence
}

fn select_content(doc: &Html, cancel: Option<&AtomicBool>) -> SelectedContent {
    let mut best_article: Option<(String, usize, bool)> = None;
    let mut total_nodes = 0_usize;
    for node in doc.tree.nodes() {
        let Some(element) = ElementRef::wrap(node) else {
            continue;
        };
        let name = element.value().name();
        let role = element.attr("role").unwrap_or("");
        let main_candidate = name == "main" || role.eq_ignore_ascii_case("main");
        let article_candidate = name == "article";
        if main_candidate || article_candidate {
            if !eligible_root(element, false) {
                continue;
            }
            let (text, complete) = clean_subtree(element, false, cancel, &mut total_nodes);
            if !text.trim().is_empty() {
                if main_candidate {
                    return SelectedContent {
                        text,
                        scope: ContentScope::Main,
                        complete,
                    };
                }
                let length = text.chars().count();
                if best_article
                    .as_ref()
                    .is_none_or(|(_, best_length, _)| length > *best_length)
                {
                    best_article = Some((text, length, complete));
                }
            }
            if !complete && cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                return SelectedContent {
                    text: String::new(),
                    scope: ContentScope::Main,
                    complete: false,
                };
            }
        }
    }
    if let Some((text, _, complete)) = best_article
        && !text.trim().is_empty()
    {
        return SelectedContent {
            text,
            scope: ContentScope::Article,
            complete,
        };
    }
    let body = doc
        .tree
        .nodes()
        .find_map(|node| ElementRef::wrap(node).filter(|element| element.value().name() == "body"));
    match body {
        Some(body) => {
            let (text, complete) = clean_subtree(body, true, cancel, &mut total_nodes);
            SelectedContent {
                text,
                scope: ContentScope::Body,
                complete,
            }
        }
        None => SelectedContent {
            text: String::new(),
            scope: ContentScope::Body,
            complete: true,
        },
    }
}

fn eligible_root(root: ElementRef<'_>, body_fallback: bool) -> bool {
    if excluded_element(root, root.value().name(), body_fallback) {
        return false;
    }
    let mut ancestor = root.parent();
    while let Some(parent) = ancestor {
        let Some(element) = ElementRef::wrap(parent) else {
            break;
        };
        if excluded_element(element, element.value().name(), body_fallback) {
            return false;
        }
        ancestor = parent.parent();
    }
    true
}

fn clean_subtree(
    root: ElementRef<'_>,
    body_fallback: bool,
    cancel: Option<&AtomicBool>,
    total_nodes: &mut usize,
) -> (String, bool) {
    if excluded_element(root, root.value().name(), body_fallback) {
        return (String::new(), true);
    }
    let mut out = String::new();
    let mut stack: Vec<WalkItem<'_>> = root.children().rev().map(WalkItem::Node).collect();
    let mut complete = true;
    while let Some(item) = stack.pop() {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            complete = false;
            break;
        }
        *total_nodes += 1;
        if *total_nodes > MAX_TEXT_NODES {
            complete = false;
            break;
        }
        match item {
            WalkItem::Boundary => boundary(&mut out),
            WalkItem::Node(node) => {
                if let Some(element) = ElementRef::wrap(node) {
                    let name = element.value().name();
                    if excluded_element(element, name, body_fallback) {
                        continue;
                    }
                    let block = is_block(name) || name == "br";
                    if block {
                        boundary(&mut out);
                        stack.push(WalkItem::Boundary);
                    }
                    let mut children = node.children().collect::<Vec<_>>();
                    children.reverse();
                    stack.extend(children.into_iter().map(WalkItem::Node));
                } else if let Node::Text(text) = node.value() {
                    out.push_str(&text.text);
                }
            }
        }
    }
    (out, complete)
}

enum WalkItem<'a> {
    Node(NodeRef<'a, Node>),
    Boundary,
}

fn excluded_element(element: ElementRef<'_>, name: &str, body_fallback: bool) -> bool {
    if matches!(
        name,
        "script" | "style" | "template" | "noscript" | "nav" | "footer" | "aside"
    ) {
        return true;
    }
    if body_fallback && name == "header" {
        return true;
    }
    let role = element.attr("role").unwrap_or("");
    if role.eq_ignore_ascii_case("navigation") {
        return true;
    }
    if element.attr("hidden").is_some() || element.attr("inert").is_some() {
        return true;
    }
    if element
        .attr("aria-hidden")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if let Some(style) = element.attr("style") {
        let style = style.to_ascii_lowercase().replace(' ', "");
        if style.contains("display:none") || style.contains("visibility:hidden") {
            return true;
        }
    }
    false
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "blockquote"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn boundary(out: &mut String) {
    if !out.is_empty() && !out.chars().last().is_some_and(char::is_whitespace) {
        out.push(' ');
    }
}

fn normalize(raw: &str) -> (String, bool) {
    let mut out = String::with_capacity(raw.len().min(MAX_NORMALIZED_BYTES));
    let mut too_large = false;
    for ch in raw.chars() {
        let ch = if ch.is_ascii_uppercase() {
            ch.to_ascii_lowercase()
        } else {
            ch
        };
        if ch.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
        if out.len() > MAX_NORMALIZED_BYTES {
            too_large = true;
            break;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    (out, too_large)
}

fn excerpt(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    (out, true)
}

struct Tokenized {
    tokens: Vec<String>,
    too_many: bool,
    oversized_token: bool,
}

fn tokenize(text: &str) -> Tokenized {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut too_many = false;
    let mut oversized_token = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() || is_combining_mark(ch) {
            if is_combining_mark(ch) && current.is_empty() {
                continue;
            }
            if current.chars().count() < MAX_TOKEN_CHARS {
                current.push(if ch.is_ascii_uppercase() {
                    ch.to_ascii_lowercase()
                } else {
                    ch
                });
            } else {
                oversized_token = true;
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
            if tokens.len() > MAX_NEAR_TOKENS {
                too_many = true;
                break;
            }
        }
    }
    if !current.is_empty() && !too_many {
        tokens.push(current);
        if tokens.len() > MAX_NEAR_TOKENS {
            too_many = true;
        }
    }
    Tokenized {
        tokens,
        too_many,
        oversized_token,
    }
}

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
    )
}

fn shingles(tokens: &[String]) -> Vec<u64> {
    if tokens.len() < SHINGLE_TOKENS {
        return Vec::new();
    }
    let mut set = BTreeSet::new();
    for window in tokens.windows(SHINGLE_TOKENS) {
        set.insert(shingle_hash(window));
    }
    set.into_iter().collect()
}

fn shingle_hash(tokens: &[String]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for token in tokens {
        hash ^= token.len() as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        for byte in token.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    splitmix64(hash)
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn seeds() -> &'static [u64; SIGNATURE_COORDS] {
    static SEEDS: OnceLock<[u64; SIGNATURE_COORDS]> = OnceLock::new();
    SEEDS.get_or_init(|| {
        let mut state = 0x243f6a8885a308d3_u64;
        let mut seeds = [0_u64; SIGNATURE_COORDS];
        for seed in &mut seeds {
            state = splitmix64(state);
            *seed = state;
        }
        seeds
    })
}

fn signature(shingles: &[u64]) -> [u64; SIGNATURE_COORDS] {
    let seeds = seeds();
    let mut signature = [u64::MAX; SIGNATURE_COORDS];
    for shingle in shingles {
        for (index, seed) in seeds.iter().enumerate() {
            let value = splitmix64(shingle ^ seed);
            if value < signature[index] {
                signature[index] = value;
            }
        }
    }
    signature
}

#[derive(Clone, Debug)]
pub struct ContentInput {
    pub url: String,
    pub content: Arc<ContentEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactMember {
    pub url: String,
    pub scope: Option<ContentScope>,
    pub stage: EvidenceStage,
    pub normalized_chars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactGroup {
    pub id: String,
    pub representative: String,
    pub hash_hex: String,
    pub stage: EvidenceStage,
    pub policy_version: u8,
    pub scope: Option<ContentScope>,
    pub normalized_chars: usize,
    pub members: Vec<ExactMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NearPair {
    pub id: String,
    pub left: String,
    pub right: String,
    pub score: f64,
    pub left_group: String,
    pub right_group: String,
    pub left_members: usize,
    pub right_members: usize,
    pub left_stage: EvidenceStage,
    pub right_stage: EvidenceStage,
    pub left_scope: Option<ContentScope>,
    pub right_scope: Option<ContentScope>,
    pub left_tokens: usize,
    pub right_tokens: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotCompared {
    pub url: String,
    pub stage: EvidenceStage,
    pub scope: Option<ContentScope>,
    pub exact_eligible: bool,
    pub near_state: NearState,
    /// Analysis progress is separate from extraction eligibility.
    pub analysis_state: AnalysisState,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub captured: usize,
    pub exact_eligible: usize,
    pub exact_groups: usize,
    pub exact_affected: usize,
    pub exact_resource_limited: usize,
    pub near_eligible: usize,
    pub near_too_short: usize,
    pub near_too_large: usize,
    pub near_resource_limited: usize,
    pub near_not_eligible: usize,
    pub near_search_limited: usize,
    pub candidate_pairs: usize,
    pub verifications: usize,
    pub retained_pairs: usize,
    pub bucket_limited_entries: usize,
    pub candidate_limited_representatives: usize,
    pub cancelled: bool,
    pub limited_reasons: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ContentAnalysis {
    pub generation: u64,
    /// Every exact group, including singletons, for resolving pair references.
    pub all_groups: Vec<ExactGroup>,
    /// Only groups with more than one member, for the same-text view.
    pub groups: Vec<ExactGroup>,
    pub pairs: Vec<NearPair>,
    pub not_compared: Vec<NotCompared>,
    pub coverage: Coverage,
    /// Distinct affected URLs across exact-group memberships incident to near pairs.
    pub similar_affected_urls: usize,
    /// Approximate bytes retained by analysis together with the evidence that seeded it.
    pub retained_bytes: usize,
    pub pages: HashMap<String, Arc<ContentEvidence>>,
    pub url_states: HashMap<String, AnalysisState>,
    /// Unicode-lowercased member URL -> every exact group ID containing that member. Distinct
    /// case-sensitive URLs that fold together retain all associations; entries are per member,
    /// not per incident pair.
    pub member_groups: HashMap<String, Vec<String>>,
}

impl ContentAnalysis {
    pub fn same_text_affected(&self) -> usize {
        self.coverage.exact_affected
    }

    pub fn similar_affected(&self) -> usize {
        self.similar_affected_urls
    }
}

/// Count distinct affected URLs from exact-group memberships incident to near pairs. Group IDs
/// are deduplicated before membership is expanded, so a giant group repeated by many pairs is
/// visited once.
pub fn count_similar_affected(groups: &[ExactGroup], pairs: &[NearPair]) -> usize {
    let affected_group_ids = pairs
        .iter()
        .flat_map(|pair| [pair.left_group.as_str(), pair.right_group.as_str()])
        .collect::<BTreeSet<_>>();
    groups
        .iter()
        .filter(|group| affected_group_ids.contains(group.id.as_str()))
        .map(|group| group.members.len())
        .sum()
}

pub fn analyze(inputs: Vec<ContentInput>, generation: u64, cancel: &AtomicBool) -> ContentAnalysis {
    analyze_with_budget(inputs, generation, cancel, &ContentBudget::default())
}

pub fn analyze_with_budget(
    inputs: Vec<ContentInput>,
    generation: u64,
    cancel: &AtomicBool,
    budget: &ContentBudget,
) -> ContentAnalysis {
    let mut inputs = inputs;
    inputs.sort_by(|left, right| left.url.cmp(&right.url));
    let mut coverage = Coverage {
        captured: inputs.len(),
        ..Coverage::default()
    };
    // Reserve every URL copy this analysis retains simultaneously: the input itself, the `pages`
    // key, the `url_states` key, and the `not_compared` diagnostic. `+64` covers the String
    // header plus map/bucket overhead per copy, and the fixed per-input allowance covers map
    // capacity, the Not compared row/reason and temporary index entries.
    let metadata_bytes = inputs
        .iter()
        .map(|input| {
            input
                .url
                .len()
                .saturating_add(METADATA_COPY_OVERHEAD)
                .saturating_mul(METADATA_URL_COPIES)
                .saturating_add(METADATA_INPUT_OVERHEAD)
        })
        .sum::<usize>();
    if !budget.try_reserve(metadata_bytes) {
        coverage
            .limited_reasons
            .push("Content-analysis memory budget could not retain URL metadata".into());
        coverage.exact_resource_limited = inputs
            .iter()
            .filter(|input| input.content.exact_eligible)
            .count();
        coverage.near_resource_limited = inputs
            .iter()
            .filter(|input| input.content.near_state == NearState::Eligible)
            .count();
        // Do not allocate page/state maps or clone URLs after the metadata reservation failed.
        // The counts and reason are truthful; per-URL detail is intentionally unavailable.
        return ContentAnalysis {
            generation,
            coverage,
            ..ContentAnalysis::default()
        };
    }
    let mut cancelled = false;
    let mut pages = HashMap::with_capacity(inputs.len());
    let mut url_states = HashMap::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        if index % 512 == 0 && cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        pages.insert(input.url.clone(), input.content.clone());
        let state = if !input.content.exact_eligible {
            AnalysisState::Ineligible
        } else if input.content.near_state != NearState::Eligible {
            match input.content.near_state {
                NearState::TooShort => AnalysisState::TooShort,
                NearState::TooLarge => AnalysisState::TooLarge,
                NearState::ResourceLimited => AnalysisState::ResourceLimited,
                NearState::NotEligible => AnalysisState::Ineligible,
                NearState::Eligible => AnalysisState::Pending,
            }
        } else {
            AnalysisState::Pending
        };
        url_states.insert(input.url.clone(), state);
    }

    let mut buckets: HashMap<(EvidenceStage, u8, [u8; 32]), Vec<usize>> = HashMap::new();
    for (index, input) in inputs.iter().enumerate() {
        if index % 512 == 0 && cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if input.content.exact_eligible {
            coverage.exact_eligible += 1;
            buckets
                .entry((
                    input.content.stage,
                    input.content.policy_version,
                    input.content.hash,
                ))
                .or_default()
                .push(index);
        }
    }

    struct GroupDraft {
        stage: EvidenceStage,
        policy_version: u8,
        hash: [u8; 32],
        members: Vec<usize>,
    }
    let mut drafts = buckets
        .into_iter()
        .map(|((stage, policy_version, hash), mut members)| {
            members.sort_by(|left, right| inputs[*left].url.cmp(&inputs[*right].url));
            GroupDraft {
                stage,
                policy_version,
                hash,
                members,
            }
        })
        .collect::<Vec<_>>();
    drafts.sort_by(|left, right| {
        inputs[left.members[0]]
            .url
            .cmp(&inputs[right.members[0]].url)
    });

    let input_index_by_url = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| (input.url.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut groups = Vec::with_capacity(drafts.len());
    let mut exact_skipped = HashSet::new();
    for (draft_index, draft) in drafts.into_iter().enumerate() {
        if draft_index % 512 == 0 && cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let member_bytes = draft
            .members
            .iter()
            .map(|index| inputs[*index].url.len().saturating_add(64))
            .sum::<usize>();
        if !budget.try_reserve(member_bytes) {
            coverage.exact_resource_limited += draft.members.len();
            exact_skipped.extend(draft.members);
            continue;
        }
        let representative = inputs[draft.members[0]].url.clone();
        let id = format!(
            "exact-{}-{}-{}",
            stage_code(draft.stage),
            draft.policy_version,
            hash_hex(&draft.hash)
        );
        let members = draft
            .members
            .iter()
            .map(|index| {
                let input = &inputs[*index];
                ExactMember {
                    url: input.url.clone(),
                    scope: input.content.scope,
                    stage: input.content.stage,
                    normalized_chars: input.content.normalized_chars,
                }
            })
            .collect::<Vec<_>>();
        groups.push(ExactGroup {
            id,
            representative,
            hash_hex: hash_hex(&draft.hash),
            stage: draft.stage,
            policy_version: draft.policy_version,
            scope: inputs[draft.members[0]].content.scope,
            normalized_chars: inputs[draft.members[0]].content.normalized_chars,
            members,
        });
    }
    let member_bytes = groups
        .iter()
        .flat_map(|group| &group.members)
        .map(|member| member.url.len().saturating_mul(2).saturating_add(64))
        .sum::<usize>();
    let member_index_ok = budget.try_reserve(member_bytes);
    if !member_index_ok {
        coverage
            .limited_reasons
            .push("Member-URL search index was omitted by the content memory budget".into());
    }
    let mut member_groups: HashMap<String, Vec<String>> = HashMap::new();
    if member_index_ok {
        for (group_index, group) in groups.iter().enumerate() {
            if group_index % 512 == 0 && cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            for member in &group.members {
                member_groups
                    .entry(member.url.to_lowercase())
                    .or_default()
                    .push(group.id.clone());
            }
        }
    }
    coverage.exact_groups = groups
        .iter()
        .filter(|group| group.members.len() > 1)
        .count();
    coverage.exact_affected = groups
        .iter()
        .filter(|group| group.members.len() > 1)
        .map(|group| group.members.len())
        .sum();
    for (group_index, group) in groups.iter().enumerate() {
        if group_index % 512 == 0 && cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if group.members.len() < 2 {
            continue;
        }
        let representative_eligible = input_index_by_url
            .get(group.representative.as_str())
            .copied()
            .is_some_and(|index| inputs[index].content.near_state == NearState::Eligible);
        if representative_eligible {
            for member in &group.members[1..] {
                url_states.insert(member.url.clone(), AnalysisState::CoveredByExactGroup);
            }
        }
    }

    for index in exact_skipped {
        url_states.insert(inputs[index].url.clone(), AnalysisState::ResourceLimited);
    }
    if coverage.exact_resource_limited > 0 {
        coverage.limited_reasons.push(format!(
            "{} pages exceeded the exact evidence memory budget",
            coverage.exact_resource_limited
        ));
    }
    let reps = groups
        .iter()
        .enumerate()
        .map(|(group_index, group)| {
            let input_index = *input_index_by_url
                .get(group.representative.as_str())
                .expect("representative belongs to its group");
            (group_index, input_index)
        })
        .collect::<Vec<_>>();

    let lsh_bytes = reps.len().saturating_mul(
        LSH_BANDS
            .saturating_mul(std::mem::size_of::<(u8, u8, u8, [u64; LSH_ROWS])>())
            .saturating_add(64),
    );
    let lsh_ok = budget.try_reserve(lsh_bytes);
    if !lsh_ok {
        coverage
            .limited_reasons
            .push("Near candidate index was omitted by the content memory budget".into());
    }
    let mut bucket_entries: HashMap<(EvidenceStage, u8, u8, [u64; LSH_ROWS]), Vec<usize>> =
        HashMap::new();
    for (rep_index, (_, input_index)) in reps.iter().enumerate() {
        if !lsh_ok || (rep_index % 512 == 0 && cancel.load(Ordering::Relaxed)) {
            cancelled = cancelled || cancel.load(Ordering::Relaxed);
            break;
        }
        let content = &inputs[*input_index].content;
        let Some(signature) = content.signature.as_ref() else {
            continue;
        };
        for band in 0..LSH_BANDS {
            let mut rows = [0_u64; LSH_ROWS];
            rows.copy_from_slice(&signature[band * LSH_ROWS..band * LSH_ROWS + LSH_ROWS]);
            let entry = bucket_entries
                .entry((content.stage, content.policy_version, band as u8, rows))
                .or_default();
            if entry.len() < MAX_BUCKET_ENTRIES {
                entry.push(rep_index);
            } else {
                coverage.bucket_limited_entries += 1;
            }
        }
    }

    let mut pairs = Vec::new();
    let mut verifications = 0_usize;
    let mut candidate_pairs = 0_usize;
    let mut rep_processed = vec![false; reps.len()];
    let mut rep_limited = vec![false; reps.len()];
    'verify: for (rep_index, (group_index, input_index)) in reps.iter().enumerate() {
        if !lsh_ok {
            break;
        }
        if cancelled || cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let content = &inputs[*input_index].content;
        let Some(signature) = content.signature.as_ref() else {
            continue;
        };
        let mut candidates = BTreeSet::new();
        for band in 0..LSH_BANDS {
            let mut rows = [0_u64; LSH_ROWS];
            rows.copy_from_slice(&signature[band * LSH_ROWS..band * LSH_ROWS + LSH_ROWS]);
            if let Some(bucket) =
                bucket_entries.get(&(content.stage, content.policy_version, band as u8, rows))
            {
                for other in bucket {
                    if *other > rep_index {
                        candidates.insert(*other);
                        if candidates.len() > MAX_CANDIDATES_PER_REP {
                            coverage.candidate_limited_representatives += 1;
                            rep_limited[rep_index] = true;
                            break;
                        }
                    }
                }
            }
            if candidates.len() > MAX_CANDIDATES_PER_REP {
                break;
            }
        }
        for other in candidates.into_iter().take(MAX_CANDIDATES_PER_REP) {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'verify;
            }
            if verifications >= MAX_VERIFICATIONS {
                rep_limited[rep_index] = true;
                coverage.limited_reasons.push(format!(
                    "Verification cap reached at {MAX_VERIFICATIONS} pairs"
                ));
                break 'verify;
            }
            verifications += 1;
            candidate_pairs += 1;
            let (other_group, other_input) = reps[other];
            let left = &inputs[*input_index];
            let right = &inputs[other_input];
            if left.content.stage != right.content.stage
                || left.content.policy_version != right.content.policy_version
            {
                continue;
            }
            if !size_passes(left.content.shingles.len(), right.content.shingles.len()) {
                continue;
            }
            let Some((intersection, union)) =
                shingle_overlap(&left.content.shingles, &right.content.shingles)
            else {
                continue;
            };
            if intersection * 5 < union * 4 {
                continue;
            }
            if pairs.len() >= MAX_NEAR_PAIRS {
                rep_limited[rep_index] = true;
                coverage
                    .limited_reasons
                    .push(format!("Near-pair cap reached at {MAX_NEAR_PAIRS} pairs"));
                break 'verify;
            }
            let pair_bytes = std::mem::size_of::<NearPair>()
                .saturating_add(left.url.len())
                .saturating_add(right.url.len())
                .saturating_add(groups[*group_index].id.len())
                .saturating_add(groups[other_group].id.len());
            if !budget.try_reserve(pair_bytes) {
                rep_limited[rep_index] = true;
                coverage
                    .limited_reasons
                    .push("Near-pair evidence memory budget reached".into());
                break 'verify;
            }
            let left_group = &groups[*group_index];
            let right_group = &groups[other_group];
            pairs.push(NearPair {
                id: format!(
                    "near-{}-{}",
                    stable_url_hash(&left.url),
                    stable_url_hash(&right.url)
                ),
                left: left.url.clone(),
                right: right.url.clone(),
                score: intersection as f64 / union as f64,
                left_group: left_group.id.clone(),
                right_group: right_group.id.clone(),
                left_members: left_group.members.len(),
                right_members: right_group.members.len(),
                left_stage: left.content.stage,
                right_stage: right.content.stage,
                left_scope: left.content.scope,
                right_scope: right.content.scope,
                left_tokens: left.content.token_count,
                right_tokens: right.content.token_count,
            });
        }
        rep_processed[rep_index] = true;
    }
    pairs.sort_by(|left, right| {
        left.left
            .cmp(&right.left)
            .then_with(|| left.right.cmp(&right.right))
    });
    let mut similar_affected_urls = count_similar_affected(&groups, &pairs);
    let group_copy_bytes = groups
        .iter()
        .map(|group| {
            group
                .id
                .len()
                .saturating_add(group.representative.len())
                .saturating_add(group.hash_hex.len())
                .saturating_add(
                    group
                        .members
                        .iter()
                        .map(|member| member.url.len().saturating_add(64))
                        .sum::<usize>(),
                )
                .saturating_add(128)
        })
        .sum::<usize>();
    let final_copy_limited_reason =
        "Final exact result copies could not be retained within the content-analysis budget";
    let mut final_copy_failed = false;
    if !budget.try_reserve(group_copy_bytes) {
        final_copy_failed = true;
        coverage.exact_resource_limited += groups
            .iter()
            .map(|group| group.members.len())
            .sum::<usize>();
        coverage
            .limited_reasons
            .push(final_copy_limited_reason.into());
        groups.clear();
        pairs.clear();
        member_groups.clear();
        similar_affected_urls = 0;
        coverage.exact_groups = 0;
        coverage.exact_affected = 0;
        for input in &inputs {
            if input.content.exact_eligible {
                url_states.insert(input.url.clone(), AnalysisState::ResourceLimited);
            }
        }
    }

    coverage.cancelled = cancelled;
    if cancelled {
        coverage
            .limited_reasons
            .push("Comparison stopped before near verification finished".into());
    }
    coverage.candidate_pairs = candidate_pairs;
    coverage.verifications = verifications;
    coverage.retained_pairs = pairs.len();
    coverage.near_eligible = inputs
        .iter()
        .filter(|input| input.content.near_state == NearState::Eligible)
        .count();
    coverage.near_too_short = inputs
        .iter()
        .filter(|input| input.content.near_state == NearState::TooShort)
        .count();
    coverage.near_too_large = inputs
        .iter()
        .filter(|input| input.content.near_state == NearState::TooLarge)
        .count();
    coverage.near_resource_limited = inputs
        .iter()
        .filter(|input| input.content.near_state == NearState::ResourceLimited)
        .count();
    coverage.near_not_eligible = inputs
        .iter()
        .filter(|input| {
            !input.content.exact_eligible && input.content.near_state == NearState::NotEligible
        })
        .count();
    if coverage.bucket_limited_entries > 0 {
        coverage.limited_reasons.push(format!(
            "{} band entries were not added after a bucket reached {MAX_BUCKET_ENTRIES}",
            coverage.bucket_limited_entries
        ));
    }
    if coverage.candidate_limited_representatives > 0 {
        coverage.limited_reasons.push(format!(
            "{} representatives took only the first {MAX_CANDIDATES_PER_REP} sorted candidates",
            coverage.candidate_limited_representatives
        ));
    }
    if coverage.near_resource_limited > 0 {
        coverage.limited_reasons.push(format!(
            "{} pages were skipped for near analysis by the content memory budget",
            coverage.near_resource_limited
        ));
    }
    let bucket_limited = coverage.bucket_limited_entries > 0;
    for (rep_index, (_, input_index)) in reps.iter().enumerate() {
        let input = &inputs[*input_index];
        if input.content.near_state != NearState::Eligible {
            continue;
        }
        // A resource-limited result from exact evidence or the final group-copy reservation is
        // terminal. Representative finalization must not turn it back into Compared/SearchLimited.
        if url_states.get(&input.url).copied() == Some(AnalysisState::ResourceLimited) {
            continue;
        }
        let state = if rep_limited[rep_index] || bucket_limited {
            AnalysisState::SearchLimited
        } else if rep_processed[rep_index] {
            AnalysisState::Compared
        } else if cancelled {
            AnalysisState::Cancelled
        } else {
            AnalysisState::SearchLimited
        };
        url_states.insert(input.url.clone(), state);
    }
    // A representative's terminal near-search state applies to every exact member. Exact
    // membership remains available, but members must not read as compared when near search was
    // limited or cancelled for their group.
    for group in &groups {
        let terminal = url_states.get(&group.representative).copied();
        if !matches!(
            terminal,
            Some(AnalysisState::SearchLimited | AnalysisState::Cancelled)
        ) {
            continue;
        }
        for member in &group.members {
            if member.url == group.representative {
                continue;
            }
            if let Some(index) = input_index_by_url.get(member.url.as_str()).copied()
                && inputs[index].content.near_state == NearState::Eligible
            {
                url_states.insert(member.url.clone(), terminal.unwrap());
            }
        }
    }
    for input in &inputs {
        if input.content.near_state != NearState::Eligible {
            continue;
        }
        let state = url_states
            .get(&input.url)
            .copied()
            .unwrap_or(AnalysisState::Pending);
        if state == AnalysisState::Pending {
            url_states.insert(
                input.url.clone(),
                if cancelled {
                    AnalysisState::Cancelled
                } else {
                    AnalysisState::Compared
                },
            );
        }
    }
    coverage.near_search_limited = url_states
        .values()
        .filter(|state| **state == AnalysisState::SearchLimited)
        .count();

    let mut not_compared = Vec::new();
    for input in &inputs {
        let content = &input.content;
        let state = url_states
            .get(&input.url)
            .copied()
            .unwrap_or(AnalysisState::Ineligible);
        if state.is_compared() {
            continue;
        }
        let reason = match state {
            AnalysisState::Ineligible => content.exact_reason.to_string(),
            AnalysisState::TooShort | AnalysisState::TooLarge => content.near_reason.to_string(),
            AnalysisState::ResourceLimited => {
                if final_copy_failed && content.exact_eligible {
                    final_copy_limited_reason.into()
                } else if content.near_reason.is_empty() {
                    coverage
                        .limited_reasons
                        .iter()
                        .rev()
                        .find(|reason| reason.contains("memory budget"))
                        .cloned()
                        .unwrap_or_else(|| "Content-analysis memory budget limited this URL".into())
                } else {
                    content.near_reason.to_string()
                }
            }
            AnalysisState::Cancelled => {
                "Analysis cancelled before this URL's near search finished".into()
            }
            AnalysisState::SearchLimited => coverage
                .limited_reasons
                .first()
                .cloned()
                .unwrap_or_else(|| "Near search was limited".into()),
            AnalysisState::Compared
            | AnalysisState::CoveredByExactGroup
            | AnalysisState::Pending => {
                continue;
            }
        };
        let reason = if reason.is_empty() {
            format!("{}: comparison did not complete", state.label())
        } else {
            reason
        };
        not_compared.push(NotCompared {
            url: input.url.clone(),
            stage: content.stage,
            scope: content.scope,
            exact_eligible: content.exact_eligible,
            near_state: content.near_state,
            analysis_state: state,
            reason,
        });
    }

    ContentAnalysis {
        generation,
        all_groups: groups.clone(),
        groups: groups
            .into_iter()
            .filter(|group| group.members.len() > 1)
            .collect(),
        pairs,
        not_compared,
        coverage,
        similar_affected_urls,
        retained_bytes: budget.used(),
        pages,
        url_states,
        member_groups,
    }
}

fn size_passes(left: usize, right: usize) -> bool {
    let min = left.min(right);
    let max = left.max(right);
    min.saturating_mul(5) >= max.saturating_mul(4)
}

fn shingle_overlap(left: &[u64], right: &[u64]) -> Option<(usize, usize)> {
    let mut left_index = 0;
    let mut right_index = 0;
    let mut intersection = 0;
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                intersection += 1;
                left_index += 1;
                right_index += 1;
            }
        }
    }
    let union = left.len() + right.len() - intersection;
    (union > 0).then_some((intersection, union))
}

fn stable_url_hash(url: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    splitmix64(hash)
}

fn stage_code(stage: EvidenceStage) -> &'static str {
    match stage {
        EvidenceStage::Document => "document",
        EvidenceStage::Rendered => "rendered",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str) -> Html {
        Html::parse_document(html)
    }

    fn evidence(html: &str) -> ContentEvidence {
        extract(
            &doc(html),
            EvidenceStage::Document,
            true,
            &ContentBudget::default(),
        )
    }

    #[test]
    fn inline_text_is_joined_and_blocks_get_boundaries() {
        let content = evidence("<body><p>co<em>op</em></p><p>next</p></body>");
        assert_eq!(&*content.excerpt, "coop next");
        assert_eq!(content.scope, Some(ContentScope::Body));
    }

    #[test]
    fn main_article_body_selection_and_hidden_navigation_are_deterministic() {
        let content = evidence(
            "<body><nav>Menu</nav><main><h1>Hello</h1><p>World</p></main><article>Other</article></body>",
        );
        assert_eq!(&*content.excerpt, "hello world");
        assert_eq!(content.scope, Some(ContentScope::Main));

        let article = evidence(
            "<body><aside>Ad</aside><article><header><h1>Story</h1></header><p>Body</p></article></body>",
        );
        assert_eq!(&*article.excerpt, "story body");
        assert_eq!(article.scope, Some(ContentScope::Article));

        let body = evidence(
            "<body><header>Site</header><p hidden>hidden</p><p>Keep</p><footer>End</footer></body>",
        );
        assert_eq!(&*body.excerpt, "keep");
        assert_eq!(body.scope, Some(ContentScope::Body));
    }

    #[test]
    fn exact_normalization_preserves_punctuation_digits_and_negation() {
        assert_eq!(
            normalize("  Hello,   WORLD! 42  not   bad "),
            ("hello, world! 42 not bad".into(), false)
        );
        let hash = evidence("<p>Hello, WORLD! 42</p>");
        assert_eq!(hash.normalized_chars, 16);
        assert!(hash.exact_eligible);
    }

    #[test]
    fn empty_and_incomplete_documents_are_not_exact_matches() {
        let empty = evidence("<body><script>ignored</script></body>");
        assert!(!empty.exact_eligible);
        assert_eq!(&*empty.exact_reason, "Selected text is empty");
        let incomplete = extract(
            &doc("<p>text</p>"),
            EvidenceStage::Rendered,
            false,
            &ContentBudget::default(),
        );
        assert!(!incomplete.exact_eligible);
    }

    #[test]
    fn tokens_preserve_combining_marks_and_punctuation_boundaries() {
        let tokenized = tokenize("Red-blue can't 42 cafe\u{301}");
        let tokens = tokenized.tokens;
        assert_eq!(tokens, vec!["red", "blue", "can", "t", "42", "cafe\u{301}"]);
    }

    #[test]
    fn short_content_is_exact_eligible_but_near_too_short() {
        let content = evidence("<p>Short but nonempty</p>");
        assert!(content.exact_eligible);
        assert_eq!(content.near_state, NearState::TooShort);
        assert_eq!(content.token_count, 3);
    }

    #[test]
    fn largest_article_wins_with_earlier_dom_order_on_ties() {
        let html = "<body><article><p>short</p></article><article><p>longer article body</p></article><article><p>equal length two</p></article></body>";
        let content = evidence(html);
        assert_eq!(content.scope, Some(ContentScope::Article));
        assert_eq!(&*content.excerpt, "longer article body");

        let tie =
            evidence("<body><article><p>same</p></article><article><p>same</p></article></body>");
        assert_eq!(&*tie.excerpt, "same");
    }

    #[test]
    fn too_many_tokens_keeps_exact_but_skips_near() {
        let tokens = (0..=MAX_NEAR_TOKENS)
            .map(|index| format!("token{index}"))
            .collect::<Vec<_>>();
        let content = evidence(&format!("<p>{}</p>", tokens.join(" ")));
        assert!(content.exact_eligible);
        assert_eq!(content.near_state, NearState::TooLarge);
        assert!(!content.near_reason.is_empty());
    }

    #[test]
    fn comparison_partitions_by_capture_stage() {
        let budget = ContentBudget::default();
        let text = (0..80)
            .map(|index| format!("stage-token-{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let document = extract(
            &doc(&format!("<p>{text}</p>")),
            EvidenceStage::Document,
            true,
            &budget,
        );
        let rendered = extract(
            &doc(&format!("<p>{text}</p>")),
            EvidenceStage::Rendered,
            true,
            &budget,
        );
        let analysis = analyze(
            vec![
                ContentInput {
                    url: "https://example.com/source".into(),
                    content: Arc::new(document),
                },
                ContentInput {
                    url: "https://example.com/rendered".into(),
                    content: Arc::new(rendered),
                },
            ],
            1,
            &AtomicBool::new(false),
        );
        assert_eq!(analysis.groups.len(), 0);
        assert_eq!(analysis.all_groups.len(), 2);
        assert!(analysis.pairs.is_empty());
    }

    #[test]
    fn comparison_partitions_by_policy_version() {
        let budget = ContentBudget::default();
        let text = (0..80)
            .map(|index| format!("policy-token-{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let one = extract(
            &doc(&format!("<p>{text}</p>")),
            EvidenceStage::Document,
            true,
            &budget,
        );
        let mut two = one.clone();
        two.policy_version = 2;
        let analysis = analyze(
            vec![
                ContentInput {
                    url: "https://example.com/one".into(),
                    content: Arc::new(one),
                },
                ContentInput {
                    url: "https://example.com/two".into(),
                    content: Arc::new(two),
                },
            ],
            1,
            &AtomicBool::new(false),
        );
        assert!(analysis.pairs.is_empty());
        assert_eq!(analysis.all_groups.len(), 2);
        let _ = one;
    }

    #[test]
    fn cancellation_returns_partial_coverage_promptly() {
        let budget = ContentBudget::default();
        let text = (0..80)
            .map(|index| format!("token{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let one = Arc::new(extract(
            &doc(&format!("<p>{text}</p>")),
            EvidenceStage::Document,
            true,
            &budget,
        ));
        let inputs = (0..5)
            .map(|index| ContentInput {
                url: format!("https://example.com/{index}"),
                content: one.clone(),
            })
            .collect::<Vec<_>>();
        let cancel = AtomicBool::new(true);
        let analysis = analyze(inputs, 3, &cancel);
        assert!(analysis.coverage.cancelled);
        assert!(analysis.pairs.is_empty());
        assert!(
            analysis
                .not_compared
                .iter()
                .any(|item| item.analysis_state == AnalysisState::Cancelled)
        );
    }

    #[test]
    fn evidence_and_analysis_charges_share_the_run_budget_and_report_limits() {
        let budget = ContentBudget::default();
        let content = extract(
            &doc(&format!(
                "<p>{}</p>",
                (0..80)
                    .map(|i| format!("budget{i}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
            EvidenceStage::Document,
            true,
            &budget,
        );
        assert!(budget.used() > content.excerpt.len());
        let almost_full = ContentBudget::with_used(CONTENT_MEMORY_BUDGET - 1);
        let analysis = analyze_with_budget(
            vec![ContentInput {
                url: "https://example.com/budget".into(),
                content: Arc::new(content),
            }],
            1,
            &AtomicBool::new(false),
            &almost_full,
        );
        assert!(!analysis.coverage.limited_reasons.is_empty());
        assert!(
            analysis
                .not_compared
                .iter()
                .all(|item| !item.reason.is_empty())
        );
    }

    #[test]
    fn exact_group_members_inherit_terminal_near_search_limits() {
        let extraction_budget = ContentBudget::default();
        let mut inputs = Vec::new();
        for group in 0..100 {
            let text = (0..80)
                .map(|index| format!("limit-group{group}-token{index}"))
                .collect::<Vec<_>>()
                .join(" ");
            let content = Arc::new(extract(
                &doc(&format!("<p>{text}</p>")),
                EvidenceStage::Document,
                true,
                &extraction_budget,
            ));
            inputs.push(ContentInput {
                url: format!("https://example.com/{group}/a"),
                content: content.clone(),
            });
            inputs.push(ContentInput {
                url: format!("https://example.com/{group}/b"),
                content,
            });
        }
        let budget = ContentBudget::with_used(CONTENT_MEMORY_BUDGET - 280_000);
        let analysis = analyze_with_budget(inputs, 1, &AtomicBool::new(false), &budget);
        assert!(
            analysis
                .coverage
                .limited_reasons
                .iter()
                .any(|reason| reason.contains("candidate index"))
        );
        for group in &analysis.all_groups {
            let states = group
                .members
                .iter()
                .map(|member| analysis.url_states.get(&member.url).copied())
                .collect::<Vec<_>>();
            assert!(
                states.iter().all(|state| {
                    *state == Some(AnalysisState::SearchLimited)
                        || *state == Some(AnalysisState::ResourceLimited)
                }),
                "group states were {states:?}"
            );
        }
        assert!(
            analysis
                .not_compared
                .iter()
                .filter(|item| item.analysis_state == AnalysisState::SearchLimited)
                .count()
                >= 20
        );
    }

    #[test]
    fn many_small_exact_groups_use_indexed_representative_lookup() {
        let budget = ContentBudget::default();
        let mut inputs = Vec::new();
        for group in 0..2_000 {
            let text = (0..80)
                .map(|token| format!("group{group}-token{token}"))
                .collect::<Vec<_>>()
                .join(" ");
            let content = Arc::new(extract(
                &doc(&format!("<p>{text}</p>")),
                EvidenceStage::Document,
                true,
                &budget,
            ));
            inputs.push(ContentInput {
                url: format!("https://example.com/{group}/a"),
                content: content.clone(),
            });
            inputs.push(ContentInput {
                url: format!("https://example.com/{group}/b"),
                content,
            });
        }
        let started = std::time::Instant::now();
        let analysis = analyze(inputs, 1, &AtomicBool::new(false));
        assert_eq!(analysis.groups.len(), 2_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "many small groups took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn case_colliding_member_urls_keep_every_group_association() {
        let budget = ContentBudget::default();
        let one = Arc::new(extract(
            &doc(
                "<main>Group one shared text with enough tokens here for near analysis one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen</main>",
            ),
            EvidenceStage::Document,
            true,
            &budget,
        ));
        let two = Arc::new(extract(
            &doc(
                "<main>Group two other text with enough tokens here for near analysis one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen</main>",
            ),
            EvidenceStage::Document,
            true,
            &budget,
        ));
        let analysis = analyze(
            vec![
                ContentInput {
                    url: "https://x/Product".into(),
                    content: one.clone(),
                },
                ContentInput {
                    url: "https://x/a".into(),
                    content: one,
                },
                ContentInput {
                    url: "https://x/product".into(),
                    content: two.clone(),
                },
                ContentInput {
                    url: "https://x/b".into(),
                    content: two,
                },
            ],
            1,
            &AtomicBool::new(false),
        );
        let groups = analysis
            .member_groups
            .get("https://x/product")
            .expect("folded member key");
        assert_eq!(
            groups.len(),
            2,
            "both case-distinct groups keep their association"
        );
        assert!(groups.contains(&analysis.all_groups[0].id));
        assert!(groups.contains(&analysis.all_groups[1].id));
    }

    #[test]
    fn large_group_affected_count_is_distinct_and_bounded_work() {
        let member = |index: usize| ExactMember {
            url: format!("https://x/member/{index}"),
            scope: Some(ContentScope::Main),
            stage: EvidenceStage::Document,
            normalized_chars: 80,
        };
        let group = ExactGroup {
            id: "exact-large".into(),
            representative: "https://x/member/0".into(),
            hash_hex: "hash".into(),
            stage: EvidenceStage::Document,
            policy_version: POLICY_VERSION,
            scope: Some(ContentScope::Main),
            normalized_chars: 80,
            members: (0..40_000).map(member).collect(),
        };
        let other = ExactGroup {
            id: "exact-other".into(),
            representative: "https://x/other".into(),
            hash_hex: "other".into(),
            stage: EvidenceStage::Document,
            policy_version: POLICY_VERSION,
            scope: Some(ContentScope::Main),
            normalized_chars: 80,
            members: vec![ExactMember {
                url: "https://x/other".into(),
                scope: Some(ContentScope::Main),
                stage: EvidenceStage::Document,
                normalized_chars: 80,
            }],
        };
        let pair = NearPair {
            id: "pair".into(),
            left: group.representative.clone(),
            right: other.representative.clone(),
            score: 0.9,
            left_group: group.id.clone(),
            right_group: other.id.clone(),
            left_members: 40_000,
            right_members: 1,
            left_stage: EvidenceStage::Document,
            right_stage: EvidenceStage::Document,
            left_scope: Some(ContentScope::Main),
            right_scope: Some(ContentScope::Main),
            left_tokens: 80,
            right_tokens: 80,
        };
        let pairs = (0..200).map(|_| pair.clone()).collect::<Vec<_>>();
        let started = std::time::Instant::now();
        assert_eq!(count_similar_affected(&[group, other], &pairs), 40_001);
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
    }

    #[test]
    fn small_labeled_corpus_retrieves_all_verified_pairs_found_exhaustively() {
        let budget = ContentBudget::default();
        let base = (0..100)
            .map(|index| format!("label-token-{index}"))
            .collect::<Vec<_>>();
        let mut texts = vec![base.clone()];
        for changed in 1..=3 {
            let mut variant = base.clone();
            for (index, token) in variant.iter_mut().take(changed * 8).enumerate() {
                *token = format!("changed-{changed}-{index}");
            }
            texts.push(variant);
        }
        texts.push((0..100).map(|index| format!("unrelated-{index}")).collect());
        let inputs = texts
            .iter()
            .enumerate()
            .map(|(index, tokens)| ContentInput {
                url: format!("https://example.com/label-{index}"),
                content: Arc::new(extract(
                    &doc(&format!("<p>{}</p>", tokens.join(" "))),
                    EvidenceStage::Document,
                    true,
                    &budget,
                )),
            })
            .collect::<Vec<_>>();
        let expected = inputs
            .iter()
            .enumerate()
            .flat_map(|(left, left_input)| {
                inputs
                    .iter()
                    .enumerate()
                    .skip(left + 1)
                    .filter_map(move |(_, right_input)| {
                        let (intersection, union) = shingle_overlap(
                            &left_input.content.shingles,
                            &right_input.content.shingles,
                        )?;
                        (size_passes(
                            left_input.content.shingles.len(),
                            right_input.content.shingles.len(),
                        ) && intersection * 5 >= union * 4)
                            .then_some((left_input.url.clone(), right_input.url.clone()))
                    })
            })
            .collect::<BTreeSet<_>>();
        let analysis = analyze(inputs, 1, &AtomicBool::new(false));
        let found = analysis
            .pairs
            .iter()
            .map(|pair| (pair.left.clone(), pair.right.clone()))
            .collect::<BTreeSet<_>>();
        eprintln!(
            "labeled corpus: exhaustive {} pairs, verified candidates {} pairs",
            expected.len(),
            found.len()
        );
        assert!(found.is_subset(&expected));
        assert_eq!(found, expected);
    }

    #[test]
    fn giant_exact_group_stays_one_group_without_materializing_pairs() {
        let budget = ContentBudget::default();
        let one = Arc::new(extract(
            &doc(&format!(
                "<p>{}</p>",
                (0..80)
                    .map(|i| format!("same{i}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
            EvidenceStage::Document,
            true,
            &budget,
        ));
        let inputs = (0..500)
            .map(|index| ContentInput {
                url: format!("https://example.com/page-{index:03}"),
                content: one.clone(),
            })
            .collect::<Vec<_>>();
        let analysis = analyze(inputs, 1, &AtomicBool::new(false));
        assert_eq!(analysis.groups.len(), 1);
        assert_eq!(analysis.groups[0].members.len(), 500);
        assert!(analysis.pairs.is_empty());
    }

    #[test]
    #[ignore = "local 10k/50k content-analysis measurement"]
    fn benchmark_deterministic_10k_and_50k_corpora() {
        for count in [10_000usize, 50_000] {
            let budget = ContentBudget::default();
            let started = std::time::Instant::now();
            let mut inputs = Vec::with_capacity(count);
            for index in 0..count {
                let cluster = index % 100;
                let mut tokens = (0..80)
                    .map(|token| format!("cluster{cluster}-token{token}"))
                    .collect::<Vec<_>>();
                if index >= 50 {
                    tokens[0] = format!("variant-{index}");
                    tokens[1] = format!("variant-{index}-b");
                }
                let html = format!("<main><p>{}</p></main>", tokens.join(" "));
                let content = extract(&doc(&html), EvidenceStage::Document, true, &budget);
                inputs.push(ContentInput {
                    url: format!("https://example.com/{index:05}"),
                    content: Arc::new(content),
                });
            }
            let extracted = started.elapsed();
            let started = std::time::Instant::now();
            let analysis = analyze(inputs, count as u64, &AtomicBool::new(false));
            let analyzed = started.elapsed();
            eprintln!(
                "{count} pages: extract {extracted:?}, analyze {analyzed:?}, budget {} MiB, groups {}, pairs {}, candidates {}, verifications {}, limited {}",
                budget.used() / (1024 * 1024),
                analysis.groups.len(),
                analysis.pairs.len(),
                analysis.coverage.candidate_pairs,
                analysis.coverage.verifications,
                analysis.coverage.limited_reasons.len(),
            );
            let cancel = AtomicBool::new(true);
            let started = std::time::Instant::now();
            let cancelled = analyze(
                analysis
                    .pages
                    .iter()
                    .map(|(url, content)| ContentInput {
                        url: url.clone(),
                        content: content.clone(),
                    })
                    .collect(),
                count as u64 + 1,
                &cancel,
            );
            eprintln!(
                "{count} pages: cancelled analyze {:?}, cancelled {}",
                started.elapsed(),
                cancelled.coverage.cancelled
            );
            assert!(cancelled.coverage.cancelled);
        }
    }

    #[test]
    fn size_pruning_matches_the_eighty_percent_contract() {
        assert!(size_passes(80, 100));
        assert!(size_passes(100, 80));
        assert!(!size_passes(79, 100));
    }

    #[test]
    fn near_analysis_finds_hand_calculated_overlap_and_prunes_size() {
        let budget = ContentBudget::default();
        let a_tokens = (0..100)
            .map(|index| format!("token{index}"))
            .collect::<Vec<_>>();
        let mut b_tokens = a_tokens.clone();
        b_tokens[0] = "changed".into();
        let a_html = format!("<p>{}</p>", a_tokens.join(" "));
        let b_html = format!("<p>{}</p>", b_tokens.join(" "));
        let a = extract(&doc(&a_html), EvidenceStage::Document, true, &budget);
        let b = extract(&doc(&b_html), EvidenceStage::Document, true, &budget);
        assert_eq!(a.near_state, NearState::Eligible);
        assert_eq!(b.near_state, NearState::Eligible);
        let (intersection, union) = shingle_overlap(&a.shingles, &b.shingles).unwrap();
        assert_eq!(intersection, 95);
        assert_eq!(union, 97);

        let inputs = vec![
            ContentInput {
                url: "https://example.com/a".into(),
                content: Arc::new(a),
            },
            ContentInput {
                url: "https://example.com/b".into(),
                content: Arc::new(b),
            },
        ];
        let analysis = analyze(inputs, 1, &AtomicBool::new(false));
        assert_eq!(analysis.groups.len(), 0);
        assert_eq!(analysis.pairs.len(), 1);
        assert!((analysis.pairs[0].score - 95.0 / 97.0).abs() < 1e-12);
    }

    #[test]
    fn exact_groups_are_stable_and_non_transitive_near_pairs_remain_pairs() {
        let budget = ContentBudget::default();
        let base = (0..120)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>();
        let mut b = base.clone();
        for token in b.iter_mut().take(12) {
            *token = format!("{token}x");
        }
        let mut c = b.clone();
        for token in c.iter_mut().take(12) {
            *token = format!("{token}x");
        }
        let make = |url: &str, tokens: &[String]| ContentInput {
            url: url.into(),
            content: Arc::new(extract(
                &doc(&format!("<p>{}</p>", tokens.join(" "))),
                EvidenceStage::Document,
                true,
                &budget,
            )),
        };
        let inputs = vec![
            make("https://example.com/z", &base),
            make("https://example.com/a", &base),
            make("https://example.com/b", &b),
            make("https://example.com/c", &c),
        ];
        let analysis = analyze(inputs, 7, &AtomicBool::new(false));
        assert_eq!(analysis.generation, 7);
        assert_eq!(analysis.groups.len(), 1);
        assert_eq!(analysis.groups[0].representative, "https://example.com/a");
        assert_eq!(analysis.groups[0].members.len(), 2);
        assert!(analysis.pairs.iter().all(|pair| pair.left != pair.right));
        assert!(analysis.pairs.iter().any(|pair| {
            (pair.left.ends_with("/a") && pair.right.ends_with("/b"))
                || (pair.left.ends_with("/b") && pair.right.ends_with("/a"))
        }));
        assert_eq!(analysis.similar_affected(), 4);
    }
}
