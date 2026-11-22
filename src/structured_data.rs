//! JSON-LD block discovery and JSON syntax diagnostics over an already parsed DOM.
//!
//! This module deliberately stops at **inventory and syntax**. It does not expand contexts,
//! resolve identifiers, fetch `@context` or `src` resources, execute scripts, or claim that a
//! block is valid schema.org or eligible for a Google rich result. Those are separate products
//! with separate evidence.
//!
//! The parser is a bounded custom serde visitor so duplicate object keys are observed before a
//! map can silently replace them. Retained evidence is small: bounded raw text, bounded literal
//! `@type`/`@id` occurrences with JSON Pointer paths, and bounded duplicate-key diagnostics. No
//! `serde_json::Value` tree is retained for the UI.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use scraper::{ElementRef, Html, Node};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Deserializer;

use crate::indexability::EvidenceStage;

/// Retained block records per page. Matching blocks beyond this are still counted.
pub const MAX_BLOCKS_PER_PAGE: usize = 32;
/// Raw JSON input and retention bound for one block.
pub const MAX_BLOCK_INPUT_BYTES: usize = 64 * 1024;
/// Total raw JSON retained for one page.
pub const MAX_PAGE_RAW_BYTES: usize = 256 * 1024;
/// Own traversal depth bound. Deeper input is a parser limit, not a syntax finding.
pub const MAX_DEPTH: usize = 64;
/// Visited JSON values per block.
pub const MAX_VISITED_VALUES: usize = 8_192;
/// Retained literal type occurrences per page.
pub const MAX_TYPE_OCCURRENCES_PER_PAGE: usize = 128;
/// Retained literal `@id` occurrences per page.
pub const MAX_ID_OCCURRENCES_PER_PAGE: usize = 128;
/// Retained duplicate-key diagnostics per block.
pub const MAX_DUPLICATE_DIAGNOSTICS_PER_BLOCK: usize = 16;
/// Retained bytes for one literal value or JSON Pointer path.
pub const MAX_RETAINED_VALUE_BYTES: usize = 1_024;
/// Retained bytes for a script attribute or diagnostic message.
pub const MAX_ATTRIBUTE_BYTES: usize = 512;
/// Bounded duplicate-key bookkeeping for one object. Beyond this the object is still traversed,
/// but duplicate-key checking stops and the block says so.
const MAX_OBJECT_KEYS_TRACKED: usize = 4_096;
/// Total structured-data owned evidence/index budget for one crawl.
pub const STRUCTURED_DATA_BUDGET: usize = 128 * 1024 * 1024;

const BLOCK_BASE_BYTES: usize = 640;
const REPORT_BASE_BYTES: usize = 512;
const TYPE_SLOT_BYTES: usize = 320;
const ID_SLOT_BYTES: usize = 256;
const DUPLICATE_SLOT_BYTES: usize = 320;
const MAX_BLOCK_LIMITATIONS: usize = 8;
const MAX_PAGE_LIMITATIONS: usize = 12;

const LIMIT_SENTINEL: &str = "calmtoad-structured-data-limit";
const CANCEL_SENTINEL: &str = "calmtoad-structured-data-cancel";

/// A shared, bounded structured-data allocation counter for one crawl.
#[derive(Debug, Default)]
pub struct StructuredBudget {
    used: AtomicUsize,
}

impl StructuredBudget {
    pub fn try_reserve(&self, bytes: usize) -> bool {
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > STRUCTURED_DATA_BUDGET {
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

    /// Give back a reservation when a derived cache or snapshot is replaced or dropped.
    pub fn release(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let _ = self
            .used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(bytes))
            });
    }

    pub fn with_used(used: usize) -> Self {
        Self {
            used: AtomicUsize::new(used.min(STRUCTURED_DATA_BUDGET)),
        }
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    pub fn remaining(&self) -> usize {
        STRUCTURED_DATA_BUDGET.saturating_sub(self.used())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureCompleteness {
    /// The selected capture is unavailable; absence cannot be asserted.
    #[default]
    Unavailable,
    /// The selected capture completed.
    Complete,
    /// The selected capture is partial; observed blocks remain evidence, but parse failures are
    /// observations on incomplete evidence.
    Incomplete,
}

impl CaptureCompleteness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "Unavailable",
            Self::Complete => "Complete",
            Self::Incomplete => "Incomplete",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InspectionCoverage {
    #[default]
    Unavailable,
    Complete,
    Limited,
    Cancelled,
}

impl InspectionCoverage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "Unavailable",
            Self::Complete => "Complete",
            Self::Limited => "Limited",
            Self::Cancelled => "Cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyntaxOutcome {
    #[default]
    NotInspected,
    Parsed,
    ParseError,
    ParserLimit,
    Empty,
    ExternalSource,
    /// Traversal stopped because the crawl or app was cancelled. This is a coverage outcome,
    /// never malformed JSON.
    Cancelled,
}

impl SyntaxOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotInspected => "Not inspected",
            Self::Parsed => "JSON parsed",
            Self::ParseError => "JSON parse error",
            Self::ParserLimit => "Parser limit",
            Self::Empty => "Empty inline JSON",
            Self::ExternalSource => "External src not fetched",
            Self::Cancelled => "Cancelled",
        }
    }

    /// The table labels stay short enough to read in the narrow Blocks view; the inspector
    /// keeps the full wording above.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::NotInspected => "Not inspected",
            Self::Parsed => "Parsed",
            Self::ParseError => "Parse error",
            Self::ParserLimit => "Parser limit",
            Self::Empty => "Empty",
            Self::ExternalSource => "External src",
            Self::Cancelled => "Cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootShape {
    #[default]
    NotParsed,
    Object,
    Array,
    UnsupportedPrimitive,
}

impl RootShape {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotParsed => "Not parsed",
            Self::Object => "Object",
            Self::Array => "Array",
            Self::UnsupportedPrimitive => "Unsupported primitive",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InventoryState {
    /// Nothing was parsed, so no inventory claim is possible.
    #[default]
    NotEvaluated,
    Complete,
    Partial,
    Unsupported,
}

impl InventoryState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotEvaluated => "Not evaluated",
            Self::Complete => "Complete",
            Self::Partial => "Partial",
            Self::Unsupported => "Unsupported shape",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCategory {
    Syntax,
    Data,
    Eof,
    Io,
    Limit,
}

impl ErrorCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Syntax => "Syntax",
            Self::Data => "Data",
            Self::Eof => "End of input",
            Self::Io => "I/O",
            Self::Limit => "Parser limit",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxDiagnostic {
    pub category: ErrorCategory,
    pub message: Arc<str>,
    /// One-based line, as reported by serde_json.
    pub line: usize,
    /// Byte-based column. serde_json can report zero.
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeOccurrence {
    pub value: Arc<str>,
    pub path: Arc<str>,
    pub value_truncated: bool,
    pub path_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdOccurrence {
    pub value: Arc<str>,
    pub path: Arc<str>,
    pub value_truncated: bool,
    pub path_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateKey {
    pub key: Arc<str>,
    pub path: Arc<str>,
    pub key_truncated: bool,
    pub path_truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Block {
    /// One-based ordinal among matching JSON-LD script elements on this page, in DOM order.
    pub ordinal: usize,
    pub script_id: Arc<str>,
    pub script_id_truncated: bool,
    /// The original `type` declaration, including any parameters.
    pub declared_type: Arc<str>,
    pub declared_type_truncated: bool,
    pub src: Arc<str>,
    pub src_truncated: bool,
    pub syntax: SyntaxOutcome,
    pub error: Option<SyntaxDiagnostic>,
    pub root: RootShape,
    pub inventory: InventoryState,
    pub types: Vec<TypeOccurrence>,
    pub ids: Vec<IdOccurrence>,
    pub duplicates: Vec<DuplicateKey>,
    /// Every duplicate key observed, including diagnostics beyond the retained cap.
    pub duplicate_count: usize,
    pub duplicates_truncated: bool,
    pub raw: Arc<str>,
    pub raw_retained: bool,
    pub raw_bytes: usize,
    pub limitations: Vec<Arc<str>>,
}

impl Block {
    pub fn is_parse_finding(&self) -> bool {
        matches!(
            self.syntax,
            SyntaxOutcome::ParseError | SyntaxOutcome::Empty
        )
    }

    pub fn has_duplicate_finding(&self) -> bool {
        self.duplicate_count > 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageReport {
    pub stage: EvidenceStage,
    pub capture: CaptureCompleteness,
    pub capture_reason: Arc<str>,
    pub inspection: InspectionCoverage,
    pub inspection_reason: Arc<str>,
    /// Matching JSON-LD scripts seen. A cancelled scan leaves this a lower bound.
    pub blocks_observed: usize,
    pub observed_known: bool,
    pub blocks_inspected: usize,
    pub blocks_retained: usize,
    pub blocks_skipped: usize,
    pub blocks: Vec<Block>,
    /// Literal `@type` string values retained across retained blocks.
    pub literal_types_retained: usize,
    /// Literal type occurrences observed but not retained because of a page or run bound.
    pub literal_types_dropped: usize,
    pub literal_ids_retained: usize,
    pub literal_ids_dropped: usize,
    pub duplicate_diagnostics: usize,
    pub duplicate_diagnostics_dropped: usize,
    /// Literal `@context` subtrees skipped. Informational; this is not a failure.
    pub context_subtrees: usize,
    /// Literal `@value` objects whose payload and datatype were not treated as entity types.
    pub value_objects: usize,
    /// Non-string `@type` entries observed. An inventory advisory, not a JSON error.
    pub non_string_types: usize,
    /// Non-string `@id` values observed. Kept separate so the advisory names the field that
    /// was unsupported; JSON parsing can still be complete.
    pub non_string_ids: usize,
    pub limitations: Vec<Arc<str>>,
    pub retained_bytes: usize,
}

impl PageReport {
    pub fn unavailable(reason: impl Into<Arc<str>>, stage: EvidenceStage) -> Self {
        Self {
            stage,
            capture_reason: reason.into(),
            ..Self::default()
        }
    }

    /// A completed selected capture whose DOM scan found no matching JSON-LD scripts. Used by
    /// sample fixtures and by callers that already know the capture is complete.
    pub fn empty_complete(stage: EvidenceStage) -> Self {
        Self {
            stage,
            capture: CaptureCompleteness::Complete,
            capture_reason: Arc::from("Selected capture completed"),
            inspection: InspectionCoverage::Complete,
            inspection_reason: Arc::from("DOM scan completed with no retained blocks"),
            observed_known: true,
            ..Self::default()
        }
    }

    pub fn observed_syntax_problems(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.is_parse_finding())
            .count()
    }

    pub fn duplicate_blocks(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.has_duplicate_finding())
            .count()
    }

    /// A confirmed v1 finding requires a complete capture. On an incomplete capture a parse
    /// failure is an observation, not a confirmed syntax finding.
    pub fn confirmed_findings(&self) -> usize {
        if self.capture != CaptureCompleteness::Complete {
            return 0;
        }
        self.observed_syntax_problems() + self.duplicate_blocks()
    }

    pub fn has_confirmed_finding(&self) -> bool {
        self.confirmed_findings() > 0
    }

    pub fn is_incomplete(&self) -> bool {
        self.capture != CaptureCompleteness::Complete
            || self.inspection == InspectionCoverage::Limited
            || self.inspection == InspectionCoverage::Cancelled
            || self.inspection == InspectionCoverage::Unavailable
            || self.blocks_skipped > 0
            || self.blocks.iter().any(|block| {
                matches!(
                    block.syntax,
                    SyntaxOutcome::ParserLimit | SyntaxOutcome::Cancelled
                )
            })
            || self.literal_types_dropped > 0
            || self.literal_ids_dropped > 0
            || self.duplicate_diagnostics_dropped > 0
    }

    /// True only when a zero block count is an actual observation of absence.
    pub fn can_assert_absence(&self) -> bool {
        self.capture == CaptureCompleteness::Complete
            && self.inspection == InspectionCoverage::Complete
            && self.observed_known
            && self.blocks_observed == 0
    }

    pub fn blocks_observed_label(&self) -> String {
        if !self.observed_known {
            return "—".into();
        }
        if self.inspection == InspectionCoverage::Complete {
            self.blocks_observed.to_string()
        } else {
            format!("{}+", self.blocks_observed)
        }
    }

    pub fn literal_type_label(&self) -> String {
        let evaluated = self.blocks.iter().any(|block| {
            matches!(
                block.syntax,
                SyntaxOutcome::Parsed | SyntaxOutcome::ParserLimit
            )
        });
        if !evaluated {
            return "—".into();
        }
        if self.literal_types_dropped > 0 {
            format!("{}+", self.literal_types_retained)
        } else {
            self.literal_types_retained.to_string()
        }
    }

    pub fn syntax_label(&self) -> String {
        let problems = self.observed_syntax_problems();
        let duplicates = self.duplicate_blocks();
        if problems == 0 && duplicates == 0 {
            let cancelled = self
                .blocks
                .iter()
                .any(|block| block.syntax == SyntaxOutcome::Cancelled);
            return if self.capture == CaptureCompleteness::Complete
                && self.inspection == InspectionCoverage::Complete
                && !cancelled
            {
                "OK".into()
            } else {
                "—".into()
            };
        }
        let total = problems + duplicates;
        if self.capture == CaptureCompleteness::Complete {
            format!(
                "{total} {}",
                if total == 1 { "finding" } else { "findings" }
            )
        } else {
            format!("{total} observed")
        }
    }

    pub fn capture_label(&self) -> String {
        format!("{} · {}", self.stage.label(), self.capture.label())
    }

    /// Compact capture label for the Pages table, where the inspector and CSV carry the
    /// unabbreviated stage and capture completeness.
    pub fn capture_short_label(&self) -> String {
        let stage = match self.stage {
            EvidenceStage::Document => "Source",
            EvidenceStage::Rendered => "Rendered",
        };
        match self.capture {
            CaptureCompleteness::Complete | CaptureCompleteness::Unavailable => stage.into(),
            CaptureCompleteness::Incomplete => format!("{stage} · partial"),
        }
    }

    /// All retained type occurrences, in block and DOM order.
    pub fn iter_types(&self) -> impl Iterator<Item = &TypeOccurrence> {
        self.blocks.iter().flat_map(|block| block.types.iter())
    }

    pub fn iter_ids(&self) -> impl Iterator<Item = &IdOccurrence> {
        self.blocks.iter().flat_map(|block| block.ids.iter())
    }

    pub fn iter_duplicates(&self) -> impl Iterator<Item = (&Block, &DuplicateKey)> {
        self.blocks.iter().flat_map(|block| {
            block
                .duplicates
                .iter()
                .map(move |duplicate| (block, duplicate))
        })
    }
}

/// One structured-data inspection context. The shared budget lives for the crawl; the page
/// capture below owns only this page's bounded report.
pub struct Inspection<'a> {
    budget: &'a StructuredBudget,
    cancel: Option<&'a AtomicBool>,
}

impl<'a> Inspection<'a> {
    pub fn new(budget: &'a StructuredBudget, cancel: Option<&'a AtomicBool>) -> Self {
        Self { budget, cancel }
    }

    pub fn begin(&self, stage: EvidenceStage, capture_complete: bool) -> PageCapture<'a> {
        self.begin_with_reason(
            stage,
            capture_complete,
            if capture_complete {
                "Selected capture completed"
            } else {
                "Selected capture is incomplete; structured-data evidence is partial"
            },
        )
    }

    pub fn begin_with_reason(
        &self,
        stage: EvidenceStage,
        capture_complete: bool,
        reason: &str,
    ) -> PageCapture<'a> {
        PageCapture {
            budget: self.budget,
            cancel: self.cancel,
            report: PageReport {
                stage,
                capture: if capture_complete {
                    CaptureCompleteness::Complete
                } else {
                    CaptureCompleteness::Incomplete
                },
                capture_reason: Arc::from(reason),
                inspection: InspectionCoverage::Complete,
                retained_bytes: REPORT_BASE_BYTES,
                ..PageReport::default()
            },
            page_raw_bytes: 0,
            stopped: false,
            budget_exhausted: false,
            report_reserved: self.budget.try_reserve(REPORT_BASE_BYTES),
        }
    }

    /// Convenience for tests and non-streaming callers: walk the parsed DOM once.
    pub fn inspect(
        &self,
        doc: &Html,
        stage: EvidenceStage,
        capture_complete: bool,
    ) -> Arc<PageReport> {
        let mut capture = self.begin(stage, capture_complete);
        for node in doc.tree.nodes() {
            if let Node::Element(element) = node.value()
                && element.name() == "script"
                && let Some(element) = ElementRef::wrap(node)
            {
                capture.observe_script(element);
            }
        }
        capture.finish()
    }
}

struct PathPart {
    key: Option<String>,
    index: Option<usize>,
}

impl PathPart {
    fn key(key: &str) -> Self {
        Self {
            key: Some(key.to_string()),
            index: None,
        }
    }

    fn index(index: usize) -> Self {
        Self {
            key: None,
            index: Some(index),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum AbortReason {
    Depth,
    Values,
    Cancel,
}

struct WalkState<'a> {
    budget: &'a StructuredBudget,
    cancel: Option<&'a AtomicBool>,
    page: &'a mut PageInventory,
    /// Committed literal occurrences from earlier blocks on this page. The per-page cap is
    /// checked against this base plus the current block's retained entries.
    type_base: usize,
    id_base: usize,
    types: Vec<TypeOccurrence>,
    ids: Vec<IdOccurrence>,
    duplicates: Vec<DuplicateKey>,
    duplicate_count: usize,
    duplicates_truncated: bool,
    root: RootShape,
    visited: usize,
    path: Vec<PathPart>,
    aborted: Option<AbortReason>,
}

impl WalkState<'_> {
    fn retain_type_value(&mut self, value: &str) {
        let (text, value_truncated) = bounded(value, MAX_RETAINED_VALUE_BYTES);
        let (path, path_truncated) = pointer(&self.path, None);
        if self.type_base.saturating_add(self.page.types_retained) >= MAX_TYPE_OCCURRENCES_PER_PAGE
        {
            self.page.types_dropped += 1;
            return;
        }
        let cost = TYPE_SLOT_BYTES
            .saturating_add(text.len())
            .saturating_add(path.len());
        if !self.budget.try_reserve(cost) {
            self.page.types_dropped += 1;
            self.page.types_budget_limited = true;
            return;
        }
        self.page.types_retained += 1;
        self.page.types_bytes = self.page.types_bytes.saturating_add(cost);
        self.types.push(TypeOccurrence {
            value: text,
            path,
            value_truncated,
            path_truncated,
        });
    }

    fn retain_id_value(&mut self, value: &str) {
        let (text, value_truncated) = bounded(value, MAX_RETAINED_VALUE_BYTES);
        let (path, path_truncated) = pointer(&self.path, None);
        if self.id_base.saturating_add(self.page.ids_retained) >= MAX_ID_OCCURRENCES_PER_PAGE {
            self.page.ids_dropped += 1;
            return;
        }
        let cost = ID_SLOT_BYTES
            .saturating_add(text.len())
            .saturating_add(path.len());
        if !self.budget.try_reserve(cost) {
            self.page.ids_dropped += 1;
            self.page.ids_budget_limited = true;
            return;
        }
        self.page.ids_retained += 1;
        self.page.ids_bytes = self.page.ids_bytes.saturating_add(cost);
        self.ids.push(IdOccurrence {
            value: text,
            path,
            value_truncated,
            path_truncated,
        });
    }

    /// Track one object key for duplicate detection. Shared by node objects and the
    /// validation-only traversal of a non-string `@type`/`@id` container, so neither can hide
    /// a duplicate or skip the bounded key set.
    fn observe_key(&mut self, key: &str, keys: &mut HashSet<String>, keys_truncated: &mut bool) {
        if *keys_truncated {
            return;
        }
        if keys.len() < MAX_OBJECT_KEYS_TRACKED {
            if !keys.insert(key.to_string()) {
                self.record_duplicate(key);
            }
        } else {
            *keys_truncated = true;
            self.page.partial = true;
            self.page
                .note("Duplicate-key checking stopped after 4,096 distinct keys in one object");
        }
    }

    fn record_duplicate(&mut self, key: &str) {
        self.duplicate_count += 1;
        if self.duplicates.len() >= MAX_DUPLICATE_DIAGNOSTICS_PER_BLOCK {
            self.duplicates_truncated = true;
            self.page.duplicate_diagnostics_dropped += 1;
            self.page.partial = true;
            self.page
                .note("Duplicate-key diagnostics are limited to 16 retained keys per block");
            return;
        }
        let (text, key_truncated) = bounded(key, MAX_ATTRIBUTE_BYTES);
        let (path, path_truncated) = pointer(&self.path, Some(key));
        let cost = DUPLICATE_SLOT_BYTES
            .saturating_add(text.len())
            .saturating_add(path.len());
        if !self.budget.try_reserve(cost) {
            self.page.duplicate_diagnostics_dropped += 1;
            self.page.partial = true;
            self.page.budget_limited = true;
            self.page
                .note("Run structured-data budget stopped duplicate-key diagnostic retention");
            return;
        }
        self.page.duplicate_diagnostics += 1;
        self.page.duplicate_bytes = self.page.duplicate_bytes.saturating_add(cost);
        self.duplicates.push(DuplicateKey {
            key: text,
            path,
            key_truncated,
            path_truncated,
        });
    }
}

#[derive(Clone, Copy)]
struct TypeIdSnapshot {
    types_retained: usize,
    types_dropped: usize,
    types_bytes: usize,
    types_budget_limited: bool,
    ids_retained: usize,
    ids_dropped: usize,
    ids_bytes: usize,
    ids_budget_limited: bool,
    non_string_types: usize,
    non_string_ids: usize,
}

struct PageInventory {
    types_retained: usize,
    types_dropped: usize,
    types_bytes: usize,
    types_budget_limited: bool,
    ids_retained: usize,
    ids_dropped: usize,
    ids_bytes: usize,
    ids_budget_limited: bool,
    duplicate_diagnostics: usize,
    duplicate_diagnostics_dropped: usize,
    duplicate_bytes: usize,
    context_subtrees: usize,
    value_objects: usize,
    non_string_types: usize,
    non_string_ids: usize,
    limitations: Vec<Arc<str>>,
    unsupported: bool,
    partial: bool,
    budget_limited: bool,
}

impl PageInventory {
    fn new() -> Self {
        Self {
            types_retained: 0,
            types_dropped: 0,
            types_bytes: 0,
            types_budget_limited: false,
            ids_retained: 0,
            ids_dropped: 0,
            ids_bytes: 0,
            ids_budget_limited: false,
            duplicate_diagnostics: 0,
            duplicate_diagnostics_dropped: 0,
            duplicate_bytes: 0,
            context_subtrees: 0,
            value_objects: 0,
            non_string_types: 0,
            non_string_ids: 0,
            limitations: Vec::new(),
            unsupported: false,
            partial: false,
            budget_limited: false,
        }
    }

    fn type_id_snapshot(&self) -> TypeIdSnapshot {
        TypeIdSnapshot {
            types_retained: self.types_retained,
            types_dropped: self.types_dropped,
            types_bytes: self.types_bytes,
            types_budget_limited: self.types_budget_limited,
            ids_retained: self.ids_retained,
            ids_dropped: self.ids_dropped,
            ids_bytes: self.ids_bytes,
            ids_budget_limited: self.ids_budget_limited,
            non_string_types: self.non_string_types,
            non_string_ids: self.non_string_ids,
        }
    }

    /// Discard literal evidence collected while an object was provisionally treated as a node
    /// and then found to contain `@value`. Counters, bytes and the matching budget reservation
    /// must disappear together so later real nodes can still be retained.
    fn rollback_type_id(
        &mut self,
        snapshot: TypeIdSnapshot,
        budget: &StructuredBudget,
        type_start: usize,
        id_start: usize,
        types: &mut Vec<TypeOccurrence>,
        ids: &mut Vec<IdOccurrence>,
    ) {
        let released = self
            .types_bytes
            .saturating_sub(snapshot.types_bytes)
            .saturating_add(self.ids_bytes.saturating_sub(snapshot.ids_bytes));
        budget.release(released);
        self.types_retained = snapshot.types_retained;
        self.types_dropped = snapshot.types_dropped;
        self.types_bytes = snapshot.types_bytes;
        self.types_budget_limited = snapshot.types_budget_limited;
        self.ids_retained = snapshot.ids_retained;
        self.ids_dropped = snapshot.ids_dropped;
        self.ids_bytes = snapshot.ids_bytes;
        self.ids_budget_limited = snapshot.ids_budget_limited;
        self.non_string_types = snapshot.non_string_types;
        self.non_string_ids = snapshot.non_string_ids;
        types.truncate(type_start);
        ids.truncate(id_start);
    }

    fn note(&mut self, reason: &'static str) {
        if self.limitations.len() >= MAX_PAGE_LIMITATIONS
            || self.limitations.iter().any(|item| &**item == reason)
        {
            return;
        }
        self.limitations.push(Arc::from(reason));
    }

    fn note_owned(&mut self, reason: String) {
        if self.limitations.len() >= MAX_PAGE_LIMITATIONS {
            return;
        }
        self.limitations.push(Arc::from(reason));
    }
}

/// Per-page collector that is fed script elements from the production single-pass DOM walk.
pub struct PageCapture<'a> {
    budget: &'a StructuredBudget,
    cancel: Option<&'a AtomicBool>,
    report: PageReport,
    page_raw_bytes: usize,
    stopped: bool,
    budget_exhausted: bool,
    report_reserved: bool,
}

impl PageCapture<'_> {
    pub fn observe_script(&mut self, element: ElementRef<'_>) {
        if self.stopped {
            return;
        }
        let Some(declared) = element.attr("type") else {
            return;
        };
        if mime_essence(declared) != "application/ld+json" {
            return;
        }
        let ordinal = self.report.blocks_observed + 1;
        self.report.blocks_observed += 1;
        if self.cancelled() {
            self.report.inspection = InspectionCoverage::Cancelled;
            self.report.inspection_reason =
                Arc::from("Inspection was cancelled before the DOM scan completed");
            self.stopped = true;
            return;
        }
        if self.budget_exhausted {
            self.report.blocks_skipped += 1;
            self.note("The run structured-data budget was reached; later blocks were not retained");
            return;
        }
        if self.report.blocks_retained >= MAX_BLOCKS_PER_PAGE {
            self.report.blocks_skipped += 1;
            self.note("Only the first 32 JSON-LD blocks on a page keep a retained record");
            return;
        }
        let raw = script_text(element, MAX_BLOCK_INPUT_BYTES);
        let declared = bounded(declared, MAX_ATTRIBUTE_BYTES);
        let id = element.attr("id").map_or_else(
            || (Arc::from(""), false),
            |value| bounded(value, MAX_ATTRIBUTE_BYTES),
        );
        let src = element.attr("src").map_or_else(
            || (Arc::from(""), false),
            |value| bounded(value, MAX_ATTRIBUTE_BYTES),
        );
        let input_too_large = raw.limit_hit;
        let text = if input_too_large {
            ""
        } else {
            raw.text.as_str()
        };
        let block_reserve = BLOCK_BASE_BYTES
            .saturating_add(declared.0.len())
            .saturating_add(id.0.len())
            .saturating_add(src.0.len());
        if !self.budget.try_reserve(block_reserve) {
            self.report.blocks_skipped += 1;
            self.report.inspection = InspectionCoverage::Limited;
            self.report.inspection_reason =
                Arc::from("Run structured-data budget could not retain another block record");
            self.note(
                "The run structured-data budget was reached; later block records were not retained",
            );
            self.budget_exhausted = true;
            return;
        }
        self.report.retained_bytes = self.report.retained_bytes.saturating_add(block_reserve);
        let mut block = Block {
            ordinal,
            script_id: id.0,
            script_id_truncated: id.1,
            declared_type: declared.0,
            declared_type_truncated: declared.1,
            src: src.0,
            src_truncated: src.1,
            ..Block::default()
        };
        if input_too_large {
            block.syntax = SyntaxOutcome::NotInspected;
            block.limitations.push(Arc::from(
                "Block input exceeds the 64 KiB inspection bound; no prefix was parsed",
            ));
            self.report.blocks_skipped += 1;
            self.report.blocks_retained += 1;
            self.report.blocks.push(block);
            self.note("Blocks over the 64 KiB input bound were not inspected");
            return;
        }
        if self.page_raw_bytes.saturating_add(text.len()) > MAX_PAGE_RAW_BYTES {
            block.syntax = SyntaxOutcome::NotInspected;
            block.limitations.push(Arc::from(
                "Page raw JSON retention reached 256 KiB before this block",
            ));
            self.report.blocks_skipped += 1;
            self.report.blocks_retained += 1;
            self.report.blocks.push(block);
            self.report.inspection = InspectionCoverage::Limited;
            self.report.inspection_reason = Arc::from("Page raw JSON retention reached 256 KiB");
            self.note("Page raw JSON retention reached 256 KiB; later blocks were not inspected");
            return;
        }
        if !self.budget.try_reserve(text.len()) {
            block.syntax = SyntaxOutcome::NotInspected;
            block.limitations.push(Arc::from(
                "Run structured-data budget reached before this block was inspected",
            ));
            self.report.blocks_skipped += 1;
            self.report.blocks_retained += 1;
            self.report.blocks.push(block);
            self.report.inspection = InspectionCoverage::Limited;
            self.report.inspection_reason =
                Arc::from("Run structured-data budget reached before this block was inspected");
            self.note(
                "The run structured-data budget was reached; later blocks were not inspected",
            );
            self.budget_exhausted = true;
            return;
        }
        self.report.retained_bytes = self.report.retained_bytes.saturating_add(text.len());
        self.page_raw_bytes = self.page_raw_bytes.saturating_add(text.len());
        block.raw = Arc::from(text);
        block.raw_retained = true;
        block.raw_bytes = text.len();
        self.report.blocks_inspected += 1;
        self.report.blocks_retained += 1;
        if text.trim().is_empty() {
            if !block.src.is_empty() {
                block.syntax = SyntaxOutcome::ExternalSource;
                block.limitations.push(Arc::from(
                    "External src was not fetched; only the inline text would be inspected",
                ));
            } else {
                block.syntax = SyntaxOutcome::Empty;
                block.error = Some(SyntaxDiagnostic {
                    category: ErrorCategory::Eof,
                    message: Arc::from("Empty JSON-LD block"),
                    line: 1,
                    column: 0,
                });
            }
            self.report.blocks.push(block);
            return;
        }
        self.inspect_block(&mut block, text);
        self.report.blocks.push(block);
    }

    fn inspect_block(&mut self, block: &mut Block, text: &str) {
        let mut page = PageInventory::new();
        let mut state = WalkState {
            budget: self.budget,
            cancel: self.cancel,
            page: &mut page,
            type_base: self.report.literal_types_retained,
            id_base: self.report.literal_ids_retained,
            types: Vec::new(),
            ids: Vec::new(),
            duplicates: Vec::new(),
            duplicate_count: 0,
            duplicates_truncated: false,
            root: RootShape::NotParsed,
            visited: 0,
            path: Vec::new(),
            aborted: None,
        };
        let mut deserializer = Deserializer::from_str(text);
        let seed = Walk {
            state: &mut state,
            depth: 0,
            root_entry: false,
            no_inventory: false,
        };
        let outcome = seed.deserialize(&mut deserializer);
        let (syntax, error) = match outcome {
            Ok(()) => match deserializer.end() {
                Ok(()) => (SyntaxOutcome::Parsed, None),
                Err(error) => classify_parse_error(&error),
            },
            Err(error) => {
                if let Some(reason) = state.aborted {
                    match reason {
                        AbortReason::Cancel => (SyntaxOutcome::Cancelled, None),
                        AbortReason::Depth | AbortReason::Values => {
                            let limit = match reason {
                                AbortReason::Depth => {
                                    "Parser traversal stopped at the 64-level depth bound"
                                }
                                AbortReason::Values => {
                                    "Parser traversal stopped at the 8,192 visited-value bound"
                                }
                                AbortReason::Cancel => unreachable!(),
                            };
                            (
                                SyntaxOutcome::ParserLimit,
                                Some(SyntaxDiagnostic {
                                    category: ErrorCategory::Limit,
                                    message: Arc::from(limit),
                                    line: error.line(),
                                    column: error.column(),
                                }),
                            )
                        }
                    }
                } else {
                    classify_parse_error(&error)
                }
            }
        };
        if syntax == SyntaxOutcome::Cancelled {
            state
                .page
                .note("Traversal was cancelled; the remainder of this block is unvalidated");
            self.report.inspection = InspectionCoverage::Cancelled;
            self.report.inspection_reason =
                Arc::from("Structured-data inspection was cancelled during JSON traversal");
        }
        block.syntax = syntax;
        block.error = error;
        block.root = state.root;
        block.types = std::mem::take(&mut state.types);
        block.ids = std::mem::take(&mut state.ids);
        block.duplicates = std::mem::take(&mut state.duplicates);
        block.duplicate_count = state.duplicate_count;
        block.duplicates_truncated = state.duplicates_truncated;
        if state.page.types_dropped > 0 {
            state.page.partial = true;
            if state.page.types_budget_limited {
                state.page.budget_limited = true;
                state
                    .page
                    .note("Run structured-data budget stopped literal type retention on this page");
            } else {
                state
                    .page
                    .note("Literal type inventory is limited to 128 retained occurrences per page");
            }
        }
        if state.page.ids_dropped > 0 {
            state.page.partial = true;
            if state.page.ids_budget_limited {
                state.page.budget_limited = true;
                state
                    .page
                    .note("Run structured-data budget stopped literal @id retention on this page");
            } else {
                state
                    .page
                    .note("Literal @id inventory is limited to 128 retained occurrences per page");
            }
        }
        if state.page.non_string_types > 0 {
            state.page.partial = true;
        }
        match syntax {
            SyntaxOutcome::Parsed => {
                block.inventory = if state.page.unsupported {
                    InventoryState::Unsupported
                } else if state.page.partial || state.page.budget_limited {
                    InventoryState::Partial
                } else {
                    InventoryState::Complete
                };
            }
            SyntaxOutcome::ParserLimit => {
                block.inventory = InventoryState::Partial;
                state
                    .page
                    .note("Validation stopped at a parser limit; the remainder is unvalidated");
                state.page.partial = true;
            }
            _ => {
                block.root = RootShape::NotParsed;
                block.inventory = InventoryState::NotEvaluated;
            }
        }
        if state.page.non_string_types > 0 {
            let count = state.page.non_string_types;
            state.page.note_owned(format!(
                "{count} non-string @type entries were not counted as literal types"
            ));
        }
        if state.page.non_string_ids > 0 {
            let count = state.page.non_string_ids;
            state.page.note_owned(format!(
                "{count} non-string @id values were not counted as literal IDs"
            ));
        }
        drop(state);
        for limitation in page.limitations.iter() {
            if block.limitations.len() < MAX_BLOCK_LIMITATIONS
                && !block.limitations.iter().any(|item| item == limitation)
            {
                block.limitations.push(limitation.clone());
            }
        }
        self.report.literal_types_retained += page.types_retained;
        self.report.literal_types_dropped += page.types_dropped;
        self.report.literal_ids_retained += page.ids_retained;
        self.report.literal_ids_dropped += page.ids_dropped;
        self.report.duplicate_diagnostics += page.duplicate_diagnostics;
        self.report.duplicate_diagnostics_dropped += page.duplicate_diagnostics_dropped;
        self.report.context_subtrees += page.context_subtrees;
        self.report.value_objects += page.value_objects;
        self.report.non_string_types += page.non_string_types;
        self.report.non_string_ids += page.non_string_ids;
        self.report.retained_bytes = self.report.retained_bytes.saturating_add(
            page.types_bytes
                .saturating_add(page.ids_bytes)
                .saturating_add(page.duplicate_bytes),
        );
        if page.partial || page.budget_limited {
            self.report.inspection = InspectionCoverage::Limited;
            if self.report.inspection_reason.is_empty() {
                self.report.inspection_reason =
                    Arc::from("Literal inventory reached a documented page or run bound");
            }
        }
        for limitation in &page.limitations {
            self.note_str(limitation);
        }
    }

    fn note(&mut self, reason: &'static str) {
        self.note_str(reason);
    }

    fn note_str(&mut self, reason: &str) {
        if self.report.limitations.len() >= MAX_PAGE_LIMITATIONS
            || self.report.limitations.iter().any(|item| &**item == reason)
        {
            return;
        }
        self.report.limitations.push(Arc::from(reason));
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    }

    pub fn finish(mut self) -> Arc<PageReport> {
        if self.cancelled() {
            self.report.inspection = InspectionCoverage::Cancelled;
            self.report.inspection_reason =
                Arc::from("Inspection was cancelled before the DOM scan completed");
        }
        self.report.observed_known = true;
        if !self.report_reserved {
            self.report.inspection = InspectionCoverage::Limited;
            self.report.inspection_reason =
                Arc::from("Run structured-data budget could not retain the page report");
            self.report.blocks.clear();
            self.report.blocks_inspected = 0;
            self.report.blocks_retained = 0;
            self.report.retained_bytes = 0;
            self.report
                .limitations
                .push(Arc::from("The run structured-data budget was exhausted"));
        }
        Arc::new(self.report)
    }
}

fn classify_parse_error(error: &serde_json::Error) -> (SyntaxOutcome, Option<SyntaxDiagnostic>) {
    let message = error.to_string();
    let category = if is_parser_limit_message(&message) {
        ErrorCategory::Limit
    } else {
        match error.classify() {
            serde_json::error::Category::Io => ErrorCategory::Io,
            serde_json::error::Category::Syntax => ErrorCategory::Syntax,
            serde_json::error::Category::Data => ErrorCategory::Data,
            serde_json::error::Category::Eof => ErrorCategory::Eof,
        }
    };
    let limit = category == ErrorCategory::Limit;
    let (text, truncated) = bounded(&message, MAX_ATTRIBUTE_BYTES);
    let message = if truncated {
        Arc::from(format!("{text}…"))
    } else {
        text
    };
    (
        if limit {
            SyntaxOutcome::ParserLimit
        } else {
            SyntaxOutcome::ParseError
        },
        Some(SyntaxDiagnostic {
            category,
            message,
            line: error.line(),
            column: error.column(),
        }),
    )
}

/// The tested serde_json version maps its recursion and numeric-range rejections to `Syntax`.
/// These exact messages are the library's own resource limits, not RFC 8259 grammar violations.
fn is_parser_limit_message(message: &str) -> bool {
    message.contains("recursion limit exceeded")
        || message.contains("number out of range")
        || message.contains("number too large")
        || message.contains("number too small")
}

struct Walk<'s, 'a> {
    state: &'s mut WalkState<'a>,
    depth: usize,
    root_entry: bool,
    /// Validation-only traversal inside a literal `@context` or `@value` payload. Duplicate
    /// keys are still diagnosed; literal types and identifiers are not entity evidence.
    no_inventory: bool,
}

impl<'s, 'a> Walk<'s, 'a> {
    fn enter<E: de::Error>(&mut self) -> Result<(), E> {
        if self.depth > MAX_DEPTH {
            self.state.aborted = Some(AbortReason::Depth);
            return Err(E::custom(LIMIT_SENTINEL));
        }
        self.tally()
    }

    fn tally<E: de::Error>(&mut self) -> Result<(), E> {
        self.state.visited += 1;
        if self.state.visited > MAX_VISITED_VALUES {
            self.state.aborted = Some(AbortReason::Values);
            return Err(E::custom(LIMIT_SENTINEL));
        }
        if self
            .state
            .cancel
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            self.state.aborted = Some(AbortReason::Cancel);
            return Err(E::custom(CANCEL_SENTINEL));
        }
        Ok(())
    }

    fn primitive<E: de::Error>(&mut self) -> Result<(), E> {
        self.tally()?;
        if self.root_entry {
            self.state.page.unsupported = true;
            self.state
                .page
                .note("Top-level array contains a non-object entry; it is not a node object");
        }
        if self.depth == 0 {
            self.state.root = RootShape::UnsupportedPrimitive;
            self.state.page.unsupported = true;
            self.state.page.note(
                "JSON parsed, but the top-level value is not a JSON-LD object or array of objects",
            );
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for Walk<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(mut self, _value: bool) -> Result<(), E> {
        self.primitive()
    }

    fn visit_i64<E: de::Error>(mut self, _value: i64) -> Result<(), E> {
        self.primitive()
    }

    fn visit_u64<E: de::Error>(mut self, _value: u64) -> Result<(), E> {
        self.primitive()
    }

    fn visit_f64<E: de::Error>(mut self, _value: f64) -> Result<(), E> {
        self.primitive()
    }

    fn visit_str<E: de::Error>(mut self, _value: &str) -> Result<(), E> {
        self.primitive()
    }

    fn visit_unit<E: de::Error>(mut self) -> Result<(), E> {
        self.primitive()
    }

    fn visit_map<M: MapAccess<'de>>(mut self, mut map: M) -> Result<(), M::Error> {
        self.enter()?;
        if self.depth == 0 {
            self.state.root = RootShape::Object;
        }
        let type_start = self.state.types.len();
        let id_start = self.state.ids.len();
        let type_id_snapshot = self.state.page.type_id_snapshot();
        let mut keys: HashSet<String> = HashSet::new();
        let mut keys_truncated = false;
        let mut value_object = false;
        while let Some(key) = map.next_key::<String>()? {
            if let Some(cancel) = self.state.cancel
                && cancel.load(Ordering::Relaxed)
            {
                self.state.aborted = Some(AbortReason::Cancel);
                return Err(de::Error::custom(CANCEL_SENTINEL));
            }
            self.state.observe_key(&key, &mut keys, &mut keys_truncated);
            match key.as_str() {
                "@context" if !self.no_inventory => {
                    // Context semantics are outside v1, but the subtree is still validated so a
                    // duplicate key cannot disappear inside it.
                    self.state.path.push(PathPart::key(&key));
                    map.next_value_seed(Walk {
                        state: &mut *self.state,
                        depth: self.depth + 1,
                        root_entry: false,
                        no_inventory: true,
                    })?;
                    self.state.path.pop();
                    self.state.page.context_subtrees += 1;
                    continue;
                }
                "@value" if !self.no_inventory => {
                    value_object = true;
                    // The payload is validated for JSON structure and duplicate keys, but its
                    // literal @type values are not entity-type evidence.
                    self.state.path.push(PathPart::key(&key));
                    map.next_value_seed(Walk {
                        state: &mut *self.state,
                        depth: self.depth + 1,
                        root_entry: false,
                        no_inventory: true,
                    })?;
                    self.state.path.pop();
                    continue;
                }
                "@type" if !self.no_inventory => {
                    self.state.path.push(PathPart::key(&key));
                    map.next_value_seed(TypeSeed {
                        state: &mut *self.state,
                        depth: self.depth + 1,
                        id: false,
                        element: false,
                        validate_only: false,
                    })?;
                    self.state.path.pop();
                }
                "@id" if !self.no_inventory => {
                    self.state.path.push(PathPart::key(&key));
                    map.next_value_seed(TypeSeed {
                        state: &mut *self.state,
                        depth: self.depth + 1,
                        id: true,
                        element: false,
                        validate_only: false,
                    })?;
                    self.state.path.pop();
                }
                _ => {
                    self.state.path.push(PathPart::key(&key));
                    map.next_value_seed(Walk {
                        state: &mut *self.state,
                        depth: self.depth + 1,
                        root_entry: false,
                        no_inventory: self.no_inventory,
                    })?;
                    self.state.path.pop();
                }
            }
        }
        if value_object {
            self.state.page.rollback_type_id(
                type_id_snapshot,
                self.state.budget,
                type_start,
                id_start,
                &mut self.state.types,
                &mut self.state.ids,
            );
            if self.depth == 0 {
                self.state.page.unsupported = true;
                self.state
                    .page
                    .note("Top-level value object is JSON, but it is not a JSON-LD entity node");
            } else {
                self.state.page.value_objects += 1;
            }
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        self.enter()?;
        let root_array = self.depth == 0;
        if root_array {
            self.state.root = RootShape::Array;
        }
        if self.root_entry {
            // JSON-LD's document grammar is an array of node objects. A nested array directly
            // under the document root parses as JSON but is not a node object, so the literal
            // inventory cannot claim completion.
            self.state.page.unsupported = true;
            self.state.page.note(
                "Top-level array contains a nested array; it is not an array of node objects",
            );
        }
        let mut index = 0_usize;
        loop {
            self.state.path.push(PathPart::index(index));
            let next = seq.next_element_seed(Walk {
                state: &mut *self.state,
                depth: self.depth + 1,
                root_entry: root_array,
                no_inventory: self.no_inventory,
            });
            self.state.path.pop();
            if next?.is_none() {
                break;
            }
            index += 1;
            if index > MAX_VISITED_VALUES {
                self.state.aborted = Some(AbortReason::Values);
                return Err(de::Error::custom(LIMIT_SENTINEL));
            }
        }
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for Walk<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

struct TypeSeed<'s, 'a> {
    state: &'s mut WalkState<'a>,
    depth: usize,
    id: bool,
    /// This seed is one entry of an `@type`/`@id` array, so a non-string value is an
    /// inventory advisory as well as a validation subtree.
    element: bool,
    /// Nested inside a non-string entry: validate syntax, duplicates, depth/value limits and
    /// cancellation, but never retain inner strings as literal evidence.
    validate_only: bool,
}

impl<'de> DeserializeSeed<'de> for TypeSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(TypeValue {
            state: self.state,
            depth: self.depth,
            id: self.id,
            element: self.element,
            validate_only: self.validate_only,
        })
    }
}

struct TypeValue<'s, 'a> {
    state: &'s mut WalkState<'a>,
    depth: usize,
    id: bool,
    element: bool,
    validate_only: bool,
}

impl TypeValue<'_, '_> {
    fn tally<E: de::Error>(&mut self) -> Result<(), E> {
        if self.depth > MAX_DEPTH {
            self.state.aborted = Some(AbortReason::Depth);
            return Err(E::custom(LIMIT_SENTINEL));
        }
        self.state.visited += 1;
        if self.state.visited > MAX_VISITED_VALUES {
            self.state.aborted = Some(AbortReason::Values);
            return Err(E::custom(LIMIT_SENTINEL));
        }
        if self
            .state
            .cancel
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            self.state.aborted = Some(AbortReason::Cancel);
            return Err(E::custom(CANCEL_SENTINEL));
        }
        Ok(())
    }

    fn non_string<E: de::Error>(&mut self) -> Result<(), E> {
        self.tally()?;
        if self.validate_only {
            return Ok(());
        }
        if self.id {
            self.state.page.non_string_ids += 1;
        } else {
            self.state.page.non_string_types += 1;
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for TypeValue<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a string or an array of strings")
    }

    fn visit_str<E: de::Error>(mut self, value: &str) -> Result<(), E> {
        self.tally()?;
        if self.validate_only {
            return Ok(());
        }
        if self.id {
            self.state.retain_id_value(value);
        } else {
            self.state.retain_type_value(value);
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        if self.validate_only {
            self.tally()?;
        } else if self.element {
            self.non_string()?;
        } else {
            self.tally()?;
        }
        let mut index = 0_usize;
        loop {
            self.state.path.push(PathPart::index(index));
            let next = seq.next_element_seed(TypeSeed {
                state: &mut *self.state,
                depth: self.depth + 1,
                id: self.id,
                element: true,
                validate_only: self.validate_only || self.element,
            });
            self.state.path.pop();
            if next?.is_none() {
                break;
            }
            index += 1;
            if index > MAX_VISITED_VALUES {
                self.state.aborted = Some(AbortReason::Values);
                return Err(de::Error::custom(LIMIT_SENTINEL));
            }
        }
        Ok(())
    }

    fn visit_map<M: MapAccess<'de>>(mut self, mut map: M) -> Result<(), M::Error> {
        if self.validate_only {
            self.tally()?;
        } else {
            self.non_string()?;
        }
        let mut keys: HashSet<String> = HashSet::new();
        let mut keys_truncated = false;
        while let Some(key) = map.next_key::<String>()? {
            if let Some(cancel) = self.state.cancel
                && cancel.load(Ordering::Relaxed)
            {
                self.state.aborted = Some(AbortReason::Cancel);
                return Err(de::Error::custom(CANCEL_SENTINEL));
            }
            self.state.observe_key(&key, &mut keys, &mut keys_truncated);
            self.state.path.push(PathPart::key(&key));
            let next = map.next_value_seed(Walk {
                state: &mut *self.state,
                depth: self.depth + 1,
                root_entry: false,
                no_inventory: true,
            });
            self.state.path.pop();
            next?;
        }
        Ok(())
    }

    fn visit_bool<E: de::Error>(mut self, _value: bool) -> Result<(), E> {
        self.non_string()
    }

    fn visit_i64<E: de::Error>(mut self, _value: i64) -> Result<(), E> {
        self.non_string()
    }

    fn visit_u64<E: de::Error>(mut self, _value: u64) -> Result<(), E> {
        self.non_string()
    }

    fn visit_f64<E: de::Error>(mut self, _value: f64) -> Result<(), E> {
        self.non_string()
    }

    fn visit_unit<E: de::Error>(mut self) -> Result<(), E> {
        self.non_string()
    }
}

fn pointer(parts: &[PathPart], extra: Option<&str>) -> (Arc<str>, bool) {
    let mut out = String::new();
    let mut truncated = false;
    for part in parts {
        if let Some(key) = &part.key {
            append_key(&mut out, key, &mut truncated);
        } else if let Some(index) = part.index {
            append_index(&mut out, index, &mut truncated);
        }
    }
    if let Some(key) = extra {
        append_key(&mut out, key, &mut truncated);
    }
    if out.is_empty() {
        out.push('/');
    }
    (Arc::from(out), truncated)
}

fn append_key(out: &mut String, key: &str, truncated: &mut bool) {
    if *truncated {
        return;
    }
    out.push('/');
    for ch in key.chars() {
        match ch {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            _ => out.push(ch),
        }
        if out.len() > MAX_RETAINED_VALUE_BYTES {
            truncate_at_boundary(out, MAX_RETAINED_VALUE_BYTES);
            *truncated = true;
            return;
        }
    }
}

fn append_index(out: &mut String, index: usize, truncated: &mut bool) {
    if *truncated {
        return;
    }
    out.push('/');
    out.push_str(&index.to_string());
    if out.len() > MAX_RETAINED_VALUE_BYTES {
        truncate_at_boundary(out, MAX_RETAINED_VALUE_BYTES);
        *truncated = true;
    }
}

/// `String::truncate` panics when the byte index is inside a multi-byte character. JSON
/// Pointer keys are arbitrary Unicode, so shortening must walk back to a boundary.
fn truncate_at_boundary(out: &mut String, max: usize) {
    if out.len() <= max {
        return;
    }
    let mut end = max;
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
}

fn bounded(value: &str, max: usize) -> (Arc<str>, bool) {
    if value.len() <= max {
        (Arc::from(value), false)
    } else {
        (Arc::from(crate::links::truncate_bytes(value, max)), true)
    }
}

/// MIME essence comparison from the WHATWG MIME Sniffing algorithm, without full parameter
/// selection semantics. `application/ld+json; profile=...` matches; `application/ld+jsonx` does
/// not.
pub fn mime_essence(declaration: &str) -> String {
    declaration
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

struct ScriptText {
    text: String,
    limit_hit: bool,
}

/// Read script-data text directly. Entities are not decoded here because the HTML script-data
/// state does not decode character references; `\u0022` remains for the JSON parser.
fn script_text(element: ElementRef<'_>, max: usize) -> ScriptText {
    let mut text = String::new();
    let mut limit_hit = false;
    for child in element.children() {
        let Node::Text(value) = child.value() else {
            continue;
        };
        if text.len().saturating_add(value.text.len()) > max {
            let remaining = max.saturating_sub(text.len());
            text.push_str(&crate::links::truncate_bytes(&value.text, remaining));
            limit_hit = true;
            break;
        }
        text.push_str(&value.text);
    }
    ScriptText { text, limit_hit }
}

/// A bounded raw excerpt centered on a one-based diagnostic line. Returns the excerpt, the
/// one-based first and last line, and the total line count.
pub fn raw_excerpt(raw: &str, line: usize) -> (String, usize, usize, usize) {
    if raw.is_empty() {
        return (String::new(), 0, 0, 0);
    }
    let lines = raw.lines().collect::<Vec<_>>();
    let total = lines.len().max(1);
    let target = line.clamp(1, total);
    let start = target.saturating_sub(8).max(1);
    let end = (target + 7).min(total);
    let mut excerpt = lines[start - 1..end].join("\n");
    if excerpt.len() > 4_096 {
        excerpt = crate::links::truncate_bytes(&excerpt, 4_096);
    }
    (excerpt, start, end, total)
}

/// Serialize the retained literal types for an unambiguous CSV JSON column.
pub fn types_json(types: &[TypeOccurrence]) -> String {
    let values = types
        .iter()
        .map(|item| {
            serde_json::json!({
                "value": item.value.as_ref(),
                "path": item.path.as_ref(),
                "truncated": item.value_truncated || item.path_truncated,
            })
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(values).to_string()
}

pub fn ids_json(ids: &[IdOccurrence]) -> String {
    let values = ids
        .iter()
        .map(|item| {
            serde_json::json!({
                "value": item.value.as_ref(),
                "path": item.path.as_ref(),
                "truncated": item.value_truncated || item.path_truncated,
            })
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(values).to_string()
}

pub fn duplicates_json(duplicates: &[DuplicateKey]) -> String {
    let values = duplicates
        .iter()
        .map(|item| {
            serde_json::json!({
                "key": item.key.as_ref(),
                "path": item.path.as_ref(),
                "truncated": item.key_truncated || item.path_truncated,
            })
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(values).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn inspect_html(html: &str, complete: bool) -> Arc<PageReport> {
        let doc = Html::parse_document(html);
        let budget = StructuredBudget::default();
        Inspection::new(&budget, None).inspect(&doc, EvidenceStage::Document, complete)
    }

    fn inspect_html_with_budget(
        html: &str,
        complete: bool,
        budget: &StructuredBudget,
        cancel: &AtomicBool,
    ) -> Arc<PageReport> {
        let doc = Html::parse_document(html);
        Inspection::new(budget, Some(cancel)).inspect(&doc, EvidenceStage::Document, complete)
    }

    #[test]
    fn mime_essence_matches_exact_types_and_parameters_but_not_near_misses() {
        assert_eq!(mime_essence("application/ld+json"), "application/ld+json");
        assert_eq!(
            mime_essence(" Application/LD+JSON ; profile=\"x\" "),
            "application/ld+json"
        );
        assert_ne!(mime_essence("application/ld+jsonx"), "application/ld+json");
        assert_ne!(mime_essence("text/javascript"), "application/ld+json");
    }

    #[test]
    fn literals_duplicates_context_and_value_objects_are_separated() {
        let html = r##"
            <main>
              <script type="application/ld+json" id="org">
                {
                  "@context": {"@vocab": "https://schema.org/"},
                  "@type": ["Organization", "LocalBusiness"],
                  "@id": "https://example.test/#org",
                  "employee": {"@type": "Person", "@id": "#person"},
                  "price": {"@type": "PriceSpecification", "@value": "9.99", "currency": "USD"},
                  "same": 1,
                  "same": 2
                }
              </script>
            </main>"##;
        let report = inspect_html(html, true);
        assert_eq!(report.blocks_observed, 1);
        assert_eq!(report.blocks_inspected, 1);
        assert_eq!(report.confirmed_findings(), 1, "duplicate key finding");
        let block = &report.blocks[0];
        assert_eq!(block.syntax, SyntaxOutcome::Parsed);
        assert_eq!(block.root, RootShape::Object);
        assert_eq!(block.inventory, InventoryState::Complete);
        assert_eq!(block.duplicate_count, 1);
        assert_eq!(&*block.duplicates[0].key, "same");
        assert_eq!(&*block.duplicates[0].path, "/same");
        assert_eq!(
            report
                .iter_types()
                .map(|item| (&*item.value, &*item.path))
                .collect::<Vec<_>>(),
            vec![
                ("Organization", "/@type/0"),
                ("LocalBusiness", "/@type/1"),
                ("Person", "/employee/@type"),
            ],
            "value-object datatype @type is not entity-type evidence"
        );
        assert_eq!(
            report
                .iter_ids()
                .map(|item| (&*item.value, &*item.path))
                .collect::<Vec<_>>(),
            vec![
                ("https://example.test/#org", "/@id"),
                ("#person", "/employee/@id"),
            ]
        );
        assert_eq!(report.context_subtrees, 1);
        assert_eq!(report.value_objects, 1);
        assert!(
            report
                .limitations
                .iter()
                .all(|item| !item.contains("context")),
            "a normal schema.org context is informational, not a warning"
        );
    }

    #[test]
    fn duplicate_keys_inside_context_and_value_payloads_are_still_diagnosed() {
        let report = inspect_html(
            r#"<script type="application/ld+json">
            {
              "@context": {"name": {"@id": "x"}, "name": {"@id": "y"}},
              "payload": {"@value": {"a": 1, "a": 2}, "@type": "Datatype"},
              "@type": "Thing"
            }
            </script>"#,
            true,
        );
        let block = &report.blocks[0];
        assert_eq!(block.duplicate_count, 2);
        assert_eq!(
            block
                .duplicates
                .iter()
                .map(|item| item.path.as_ref())
                .collect::<Vec<_>>(),
            vec!["/@context/name", "/payload/@value/a"]
        );
        assert_eq!(
            report
                .iter_types()
                .map(|item| item.value.as_ref())
                .collect::<Vec<_>>(),
            vec!["Thing"],
            "context terms and value-object datatype @type are not entity types"
        );
        assert_eq!(report.value_objects, 1);
        assert_eq!(report.context_subtrees, 1);
    }

    #[test]
    fn top_level_array_paths_and_primitives_are_explicit() {
        let array = inspect_html(
            r#"<script type="application/ld+json">[{"@type":"A"},42]</script>"#,
            true,
        );
        assert_eq!(array.blocks[0].root, RootShape::Array);
        assert_eq!(array.blocks[0].inventory, InventoryState::Unsupported);
        assert_eq!(&*array.iter_types().next().unwrap().path, "/0/@type");

        let primitive = inspect_html(r#"<script type="application/ld+json">true</script>"#, true);
        assert_eq!(primitive.blocks[0].syntax, SyntaxOutcome::Parsed);
        assert_eq!(primitive.blocks[0].root, RootShape::UnsupportedPrimitive);
        assert_eq!(primitive.blocks[0].inventory, InventoryState::Unsupported);
        assert_eq!(primitive.confirmed_findings(), 0);
    }

    #[test]
    fn malformed_and_empty_json_are_findings_only_on_complete_captures() {
        let malformed = inspect_html(
            r#"<script type="application/ld+json">{"a":1,}</script>"#,
            true,
        );
        assert_eq!(malformed.blocks[0].syntax, SyntaxOutcome::ParseError);
        let error = malformed.blocks[0].error.as_ref().unwrap();
        assert!(error.line >= 1);
        assert_eq!(malformed.confirmed_findings(), 1);

        let incomplete = inspect_html(
            r#"<script type="application/ld+json">{"a":1,}</script>"#,
            false,
        );
        assert_eq!(incomplete.blocks[0].syntax, SyntaxOutcome::ParseError);
        assert_eq!(incomplete.confirmed_findings(), 0);
        assert!(incomplete.is_incomplete());

        let empty = inspect_html(r#"<script type="application/ld+json">   </script>"#, true);
        assert_eq!(empty.blocks[0].syntax, SyntaxOutcome::Empty);
        assert_eq!(empty.confirmed_findings(), 1);
    }

    #[test]
    fn external_src_is_detected_and_not_an_empty_inline_error() {
        let external = inspect_html(
            r#"<script type="application/ld+json" src="https://example.test/x.json"></script>"#,
            true,
        );
        assert_eq!(external.blocks[0].syntax, SyntaxOutcome::ExternalSource);
        assert_eq!(&*external.blocks[0].src, "https://example.test/x.json");
        assert_eq!(external.confirmed_findings(), 0);

        let both = inspect_html(
            r#"<script type="application/ld+json" src="x.json">{"@type":"Thing"}</script>"#,
            true,
        );
        assert_eq!(both.blocks[0].syntax, SyntaxOutcome::Parsed);
        assert_eq!(&*both.blocks[0].src, "x.json");
        assert_eq!(both.literal_types_retained, 1);
    }

    #[test]
    fn script_data_is_not_entity_decoded_and_a_closing_tag_ends_the_block() {
        let entities = inspect_html(
            r#"<script type="application/ld+json">{"@type":"A&quot;B"}</script>"#,
            true,
        );
        assert_eq!(&*entities.blocks[0].types[0].value, "A&quot;B");
        assert_eq!(entities.blocks[0].syntax, SyntaxOutcome::Parsed);

        let escaped = inspect_html(
            r#"<script type="application/ld+json">{"@type":"A\u0022B"}</script>"#,
            true,
        );
        assert_eq!(&*escaped.blocks[0].types[0].value, "A\"B");

        let closed = inspect_html(
            r#"<script type="application/ld+json">{"@type":"A</script><p>tail</p>"#,
            true,
        );
        assert_eq!(closed.blocks_observed, 1);
        assert_eq!(closed.blocks[0].syntax, SyntaxOutcome::ParseError);
    }

    #[test]
    fn parser_limits_are_not_reported_as_malformed_json() {
        let deep = format!(
            r#"<script type="application/ld+json">{}{}{}</script>"#,
            "[".repeat(80),
            "{}",
            "]".repeat(80)
        );
        let deep = inspect_html(&deep, true);
        assert_eq!(deep.blocks[0].syntax, SyntaxOutcome::ParserLimit);
        assert_eq!(
            deep.blocks[0].error.as_ref().unwrap().category,
            ErrorCategory::Limit
        );
        assert_eq!(deep.confirmed_findings(), 0);
        assert!(deep.is_incomplete());

        let exponent = inspect_html(
            r#"<script type="application/ld+json">{"a":1e400}</script>"#,
            true,
        );
        assert_eq!(
            exponent.blocks[0].syntax,
            SyntaxOutcome::ParserLimit,
            "numeric range is a library limit, not a grammar error: {:?}",
            exponent.blocks[0].error
        );
    }

    #[test]
    fn duplicate_diagnostics_stop_at_the_block_cap_but_the_count_remains() {
        let mut object = String::from("{");
        for index in 0..20 {
            object.push_str(&format!(r#""same":{index},"#));
        }
        object.push_str(r#""end":0}"#);
        let report = inspect_html(
            &format!(r#"<script type="application/ld+json">{object}</script>"#),
            true,
        );
        let block = &report.blocks[0];
        assert_eq!(block.duplicate_count, 19);
        assert_eq!(block.duplicates.len(), MAX_DUPLICATE_DIAGNOSTICS_PER_BLOCK);
        assert!(block.duplicates_truncated);
        assert!(report.duplicate_diagnostics_dropped > 0);
    }

    #[test]
    fn page_caps_keep_observed_counts_and_do_not_parse_truncated_prefixes() {
        let blocks = (0..40)
            .map(|index| {
                format!(r#"<script type="application/ld+json">{{"@type":"T{index}"}}</script>"#)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let report = inspect_html(&blocks, true);
        assert_eq!(report.blocks_observed, 40);
        assert_eq!(report.blocks_retained, MAX_BLOCKS_PER_PAGE);
        assert_eq!(report.blocks_inspected, MAX_BLOCKS_PER_PAGE);
        assert_eq!(report.blocks_skipped, 8);
        assert!(report.is_incomplete());

        let large = format!(
            r#"<script type="application/ld+json">{{"pad":"{}"}}</script>"#,
            "x".repeat(MAX_BLOCK_INPUT_BYTES + 10)
        );
        let report = inspect_html(&large, true);
        assert_eq!(report.blocks[0].syntax, SyntaxOutcome::NotInspected);
        assert!(!report.blocks[0].raw_retained);
        assert_eq!(report.blocks_inspected, 0);
    }

    #[test]
    fn page_raw_cap_skips_later_blocks_with_a_reason() {
        let block = |index: usize| {
            format!(
                r#"<script type="application/ld+json">{{"@type":"T{index}","pad":"{}"}}</script>"#,
                "x".repeat(60 * 1024)
            )
        };
        let html = (0..5).map(block).collect::<Vec<_>>().join("\n");
        let report = inspect_html(&html, true);
        assert_eq!(report.blocks_observed, 5);
        assert!(report.blocks_inspected < 5);
        assert!(report.blocks_skipped > 0);
        assert!(
            report
                .limitations
                .iter()
                .any(|item| item.contains("256 KiB"))
        );
        assert!(
            report
                .blocks
                .iter()
                .map(|block| block.raw_bytes)
                .sum::<usize>()
                <= MAX_PAGE_RAW_BYTES
        );
    }

    #[test]
    fn type_cap_is_a_partial_inventory_not_a_syntax_finding() {
        let values = (0..200)
            .map(|index| format!("\"T{index}\""))
            .collect::<Vec<_>>()
            .join(",");
        let report = inspect_html(
            &format!(r#"<script type="application/ld+json">{{"@type":[{values}]}}</script>"#),
            true,
        );
        assert_eq!(report.blocks[0].syntax, SyntaxOutcome::Parsed);
        assert_eq!(report.literal_types_retained, MAX_TYPE_OCCURRENCES_PER_PAGE);
        assert_eq!(report.literal_types_dropped, 72);
        assert_eq!(report.blocks[0].inventory, InventoryState::Partial);
        assert!(report.is_incomplete());
        assert_eq!(report.confirmed_findings(), 0);
    }

    #[test]
    fn exhausted_run_budget_keeps_a_bounded_summary_and_no_block_records() {
        let budget = StructuredBudget::with_used(STRUCTURED_DATA_BUDGET);
        let cancelled = AtomicBool::new(false);
        let report = inspect_html_with_budget(
            r#"<script type="application/ld+json">{"@type":"A"}</script>"#,
            true,
            &budget,
            &cancelled,
        );
        assert_eq!(report.blocks_observed, 1);
        assert!(report.blocks.is_empty());
        assert!(
            report.observed_known,
            "the DOM scan still counts observed blocks without retaining records"
        );
        assert_eq!(report.inspection, InspectionCoverage::Limited);
        assert!(
            report
                .limitations
                .iter()
                .any(|item| item.contains("budget"))
        );
        assert_eq!(budget.used(), STRUCTURED_DATA_BUDGET);
    }

    #[test]
    fn cancellation_is_terminal_and_not_an_absence_claim() {
        let budget = StructuredBudget::default();
        let cancelled = AtomicBool::new(true);
        let report = inspect_html_with_budget(
            r#"<script type="application/ld+json">{"@type":"A"}</script>"#,
            true,
            &budget,
            &cancelled,
        );
        assert_eq!(report.inspection, InspectionCoverage::Cancelled);
        assert!(!report.can_assert_absence());
        assert!(report.is_incomplete());
    }

    #[test]
    fn long_retained_values_and_paths_carry_explicit_truncation_flags() {
        let long = "x".repeat(2_000);
        let report = inspect_html(
            &format!(r#"<script type="application/ld+json">{{"@type":"{long}"}}</script>"#),
            true,
        );
        let item = &report.blocks[0].types[0];
        assert!(item.value_truncated);
        assert_eq!(item.value.len(), MAX_RETAINED_VALUE_BYTES);

        let deep = format!(
            r#"<script type="application/ld+json">{{"{}":{{"@type":"A"}}}}</script>"#,
            "k".repeat(2_000)
        );
        let report = inspect_html(&deep, true);
        let item = &report.blocks[0].types[0];
        assert!(item.path_truncated);
        assert!(item.path.len() <= MAX_RETAINED_VALUE_BYTES);
    }

    #[test]
    fn empty_raw_excerpt_is_safe_and_labeled_as_empty() {
        assert_eq!(raw_excerpt("", 1), (String::new(), 0, 0, 0));
        let (excerpt, start, end, total) = raw_excerpt("{\n  \"a\": 1\n}", 2);
        assert!(excerpt.contains("\"a\""));
        assert_eq!((start, end, total), (1, 3, 3));
    }

    #[test]
    fn a_complete_page_with_no_json_ld_can_assert_absence() {
        let report = inspect_html("<main><h1>No markup</h1></main>", true);
        assert!(report.can_assert_absence());
        assert_eq!(report.blocks_observed_label(), "0");
        assert_eq!(report.syntax_label(), "OK");
        assert_eq!(report.confirmed_findings(), 0);

        let incomplete = inspect_html("<main>No complete capture</main>", false);
        assert!(!incomplete.can_assert_absence());
        assert_eq!(
            incomplete.blocks_observed_label(),
            "0",
            "the DOM scan completed; only absence is unconfirmed"
        );
    }

    #[test]
    fn pointer_truncation_is_utf8_safe_at_the_retained_boundary() {
        // A 1,019-byte ASCII key leaves room for the next key's slash and part of a 4-byte
        // character. Shortening at byte 1,024 used to land inside that character and panic.
        let first = "a".repeat(1_019);
        let report = inspect_html(
            &format!(
                r#"<script type="application/ld+json">{{"{first}":{{"😀":{{"@type":"Thing"}}}}}}</script>"#
            ),
            true,
        );
        assert_eq!(report.blocks[0].syntax, SyntaxOutcome::Parsed);
        let item = &report.blocks[0].types[0];
        assert!(item.path_truncated);
        assert!(item.path.len() <= MAX_RETAINED_VALUE_BYTES);
        assert!(item.path.is_char_boundary(item.path.len()));
        assert!(
            !item.path.contains('😀'),
            "the partial multi-byte character is dropped rather than split"
        );
    }

    #[test]
    fn cancellation_during_traversal_is_cancelled_not_malformed_json() {
        let budget = StructuredBudget::default();
        let cancelled = AtomicBool::new(true);
        let mut capture =
            Inspection::new(&budget, Some(&cancelled)).begin(EvidenceStage::Document, true);
        let mut block = Block {
            ordinal: 1,
            ..Block::default()
        };
        capture.inspect_block(&mut block, r#"{"@type":"Thing","name":"Example"}"#);
        assert_eq!(block.syntax, SyntaxOutcome::Cancelled);
        assert!(!block.is_parse_finding());
        assert!(block.error.is_none());
        let report = capture.finish();
        assert_eq!(report.inspection, InspectionCoverage::Cancelled);
        assert_eq!(report.confirmed_findings(), 0);
        assert!(report.is_incomplete());
        assert_ne!(report.syntax_label(), "OK");
    }

    #[test]
    fn non_string_type_containers_use_the_bounded_duplicate_aware_walk() {
        let report = inspect_html(
            r#"<script type="application/ld+json">{"@type":{"x":1,"x":2}}</script>"#,
            true,
        );
        let block = &report.blocks[0];
        assert_eq!(block.syntax, SyntaxOutcome::Parsed);
        assert_eq!(block.duplicate_count, 1, "duplicate inside @type object");
        assert_eq!(&*block.duplicates[0].path, "/@type/x");
        assert_eq!(report.non_string_types, 1);
        assert_eq!(report.non_string_ids, 0);
        assert_eq!(block.inventory, InventoryState::Partial);
        assert_eq!(report.confirmed_findings(), 1);

        let id = inspect_html(
            r#"<script type="application/ld+json">{"@id":{"x":1,"x":2}}</script>"#,
            true,
        );
        assert_eq!(id.blocks[0].syntax, SyntaxOutcome::Parsed);
        assert_eq!(
            id.blocks[0].duplicate_count, 1,
            "duplicate inside @id object"
        );
        assert_eq!(id.blocks[0].duplicates[0].path.as_ref(), "/@id/x");

        let deep = format!(
            r#"<script type="application/ld+json">{{"@type":{}1{}}}</script>"#,
            "{\"a\":".repeat(70),
            "}".repeat(70)
        );
        let report = inspect_html(&deep, true);
        assert_eq!(
            report.blocks[0].syntax,
            SyntaxOutcome::ParserLimit,
            "the explicit depth bound still applies inside a non-string @type"
        );
        assert!(
            report.blocks[0]
                .error
                .as_ref()
                .is_some_and(|error| error.message.contains("depth bound")),
            "the explicit depth bound still applies inside a non-string @type: {:?}",
            report.blocks[0].error
        );
    }

    #[test]
    fn literal_occurrence_caps_are_page_wide_across_blocks() {
        let types = (0..100)
            .map(|index| format!("\"Type{index}\""))
            .collect::<Vec<_>>()
            .join(",");
        let ids = (0..100)
            .map(|index| format!("\"#id{index}\""))
            .collect::<Vec<_>>()
            .join(",");
        let block = format!(r#"{{"@type":[{types}],"@id":[{ids}]}}"#);
        let report = inspect_html(
            &format!(
                r#"<script type="application/ld+json">{block}</script>
                   <script type="application/ld+json">{block}</script>"#
            ),
            true,
        );
        assert_eq!(report.blocks_observed, 2);
        assert_eq!(report.literal_types_retained, MAX_TYPE_OCCURRENCES_PER_PAGE);
        assert_eq!(report.literal_types_dropped, 72);
        assert_eq!(report.literal_ids_retained, MAX_ID_OCCURRENCES_PER_PAGE);
        assert_eq!(report.literal_ids_dropped, 72);
        assert_eq!(report.blocks[0].inventory, InventoryState::Complete);
        assert_eq!(report.blocks[1].inventory, InventoryState::Partial);
        assert_eq!(report.blocks[1].syntax, SyntaxOutcome::Parsed);
        assert!(report.is_incomplete());
        assert_eq!(report.confirmed_findings(), 0);
        assert!(
            report
                .limitations
                .iter()
                .any(|item| item.contains("128 retained occurrences"))
        );
    }

    #[test]
    fn value_object_rollback_releases_evidence_and_preserves_later_nodes() {
        let mut items = Vec::new();
        for index in 0..128 {
            items.push(if index % 2 == 0 {
                r#"{"@type":"datatype","@value":"x"}"#.to_string()
            } else {
                r#"{"@value":"x","@type":"datatype"}"#.to_string()
            });
        }
        items.push(r##"{"@type":"RealNode","@id":"#real"}"##.to_string());
        let report = inspect_html(
            &format!(
                r#"<script type="application/ld+json">[{}]</script>"#,
                items.join(",")
            ),
            true,
        );
        assert_eq!(report.value_objects, 128);
        assert_eq!(report.literal_types_retained, 1);
        assert_eq!(report.literal_ids_retained, 1);
        assert_eq!(report.literal_types_dropped, 0);
        assert_eq!(report.literal_ids_dropped, 0);
        assert_eq!(
            report
                .iter_types()
                .map(|item| item.value.as_ref())
                .collect::<Vec<_>>(),
            vec!["RealNode"]
        );
        assert_eq!(
            report
                .iter_ids()
                .map(|item| item.value.as_ref())
                .collect::<Vec<_>>(),
            vec!["#real"]
        );
        assert_eq!(report.blocks[0].inventory, InventoryState::Complete);
        assert!(!report.is_incomplete());

        let first = StructuredBudget::default();
        let order_a = inspect_html_with_budget(
            r##"<script type="application/ld+json">[{"@type":"datatype","@value":"x"},{"@type":"RealNode"}]</script>"##,
            true,
            &first,
            &AtomicBool::new(false),
        );
        let second = StructuredBudget::default();
        let order_b = inspect_html_with_budget(
            r##"<script type="application/ld+json">[{"@value":"x","@type":"datatype"},{"@type":"RealNode"}]</script>"##,
            true,
            &second,
            &AtomicBool::new(false),
        );
        assert_eq!(order_a.literal_types_retained, 1);
        assert_eq!(order_b.literal_types_retained, 1);
        assert_eq!(
            order_a.retained_bytes, order_b.retained_bytes,
            "both key orders must release the same discarded evidence"
        );
        assert_eq!(
            first.used(),
            second.used(),
            "the budget reservation must be released, not merely hidden from the list"
        );
    }

    #[test]
    fn nested_non_string_type_arrays_validate_without_inventorying_strings() {
        let report = inspect_html(
            r##"<script type="application/ld+json">{"@type":["Valid",["NestedType"],"After"],"@id":["#ok",["nested-id"]]}</script>"##,
            true,
        );
        let block = &report.blocks[0];
        assert_eq!(block.syntax, SyntaxOutcome::Parsed);
        assert_eq!(
            report
                .iter_types()
                .map(|item| item.value.as_ref())
                .collect::<Vec<_>>(),
            vec!["Valid", "After"]
        );
        assert_eq!(
            report
                .iter_ids()
                .map(|item| item.value.as_ref())
                .collect::<Vec<_>>(),
            vec!["#ok"]
        );
        assert_eq!(block.inventory, InventoryState::Partial);
        assert_eq!(report.non_string_types, 1);
        assert_eq!(report.non_string_ids, 1);
        assert_eq!(report.confirmed_findings(), 0);

        let inside_object = inspect_html(
            r#"<script type="application/ld+json">{"@type":[{"x":1,"x":2}]}</script>"#,
            true,
        );
        assert_eq!(inside_object.blocks[0].syntax, SyntaxOutcome::Parsed);
        assert_eq!(inside_object.blocks[0].duplicate_count, 1);
        assert!(inside_object.blocks[0].types.is_empty());
        assert_eq!(inside_object.non_string_types, 1);

        let deep = format!(
            r#"<script type="application/ld+json">{{"@type":{}"x"{}}}</script>"#,
            "[".repeat(70),
            "]".repeat(70)
        );
        let report = inspect_html(&deep, true);
        assert_eq!(report.blocks[0].syntax, SyntaxOutcome::ParserLimit);
        assert!(
            report.blocks[0]
                .error
                .as_ref()
                .is_some_and(|error| error.message.contains("depth bound"))
        );
    }

    #[test]
    fn nested_arrays_at_the_document_root_are_an_explicit_unsupported_shape() {
        let report = inspect_html(r#"<script type="application/ld+json">[[]]</script>"#, true);
        let block = &report.blocks[0];
        assert_eq!(block.syntax, SyntaxOutcome::Parsed);
        assert_eq!(block.root, RootShape::Array);
        assert_eq!(block.inventory, InventoryState::Unsupported);
        assert_eq!(report.confirmed_findings(), 0);
        assert!(
            report
                .limitations
                .iter()
                .any(|item| item.contains("nested array"))
        );

        let mixed = inspect_html(
            r#"<script type="application/ld+json">[{"@type":"Thing"},[]]</script>"#,
            true,
        );
        assert_eq!(mixed.blocks[0].syntax, SyntaxOutcome::Parsed);
        assert_eq!(mixed.blocks[0].inventory, InventoryState::Unsupported);
        assert_eq!(mixed.literal_types_retained, 1);
        assert_eq!(mixed.confirmed_findings(), 0);
    }

    #[test]
    fn non_string_id_advisories_name_id_not_type() {
        let report = inspect_html(
            r#"<script type="application/ld+json">{"@id":42}</script>"#,
            true,
        );
        let block = &report.blocks[0];
        assert_eq!(block.syntax, SyntaxOutcome::Parsed);
        assert_eq!(report.non_string_ids, 1);
        assert_eq!(report.non_string_types, 0);
        assert!(
            report
                .limitations
                .iter()
                .any(|item| item.contains("@id") && !item.contains("@type"))
        );
        assert!(
            !report
                .limitations
                .iter()
                .any(|item| item.contains("non-string @type"))
        );
        assert_eq!(block.inventory, InventoryState::Complete);
        assert_eq!(report.confirmed_findings(), 0);

        let type_value = inspect_html(
            r#"<script type="application/ld+json">{"@type":42}</script>"#,
            true,
        );
        assert_eq!(type_value.non_string_types, 1);
        assert_eq!(type_value.non_string_ids, 0);
    }
}
