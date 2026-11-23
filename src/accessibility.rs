//! Opt-in accessibility evidence from the pinned, locally bundled axe-core engine.
//!
//! The browser performs the audit in an isolated execution world and projects a bounded payload
//! before CDP serialization. This module owns the limits, host-side validation, retained evidence
//! and the run budget. It never talks to the network and never treats axe's `incomplete` outcome as
//! a confirmed violation.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// The pinned browser engine. Kept here so the embedded bytes and every report agree.
pub const ENGINE_VERSION: &str = "4.14.1";
pub const ENGINE_LICENSE: &str = "MPL-2.0";
pub const ENGINE_SHA256: &str = "29348999450cdf2849a8e70b7912fc2da85a849da39c0d0d5394f51d4a9ed95c";
pub const ENGINE_BYTES_SHA256: &str = ENGINE_SHA256;
pub const ENGINE_SOURCE: &str = include_str!("../assets/axe/4.14.1/axe.min.js");
pub const ENGINE_LICENSE_TEXT: &str = include_str!("../assets/axe/4.14.1/LICENSE");
pub const ENGINE_NOTICES: &str = include_str!("../assets/axe/4.14.1/LICENSE-3RD-PARTY.txt");
pub const ENGINE_SOURCE_URL: &str = "https://github.com/dequelabs/axe-core/tree/v4.14.1";

/// The WCAG A/AA tag union deliberately selects every matching tagged rule, including rules whose
/// default `enabled` value is false. The profile is persisted with each report.
pub const PROFILE_TAGS: [&str; 5] = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
pub const PROFILE_ID: &str = "wcag2a+wcag2aa+wcag21a+wcag21aa+wcag22aa";
pub const SCOPE_NOTE: &str = "Main document and supported open shadow roots only. Frame contents, closed shadow roots, responsive states and interactive journeys are excluded. Automated checks cover only part of WCAG; needs-review results are not passes or confirmed violations.";

/// Retained accessibility evidence, caches, index copies and exports combined.
pub const BUDGET_BYTES: usize = 64 * 1024 * 1024;
/// Retained affected-node occurrences across the whole run.
pub const MAX_RUN_OCCURRENCES: usize = 10_000;
/// Preflight traversal limit, including supported open-shadow descendants.
pub const MAX_PREFLIGHT_ELEMENTS: usize = 20_000;
/// Retained rule/result-kind groups per page.
pub const MAX_RULES_PER_PAGE: usize = 128;
/// Retained affected-node occurrences per page.
pub const MAX_NODES_PER_PAGE: usize = 200;
/// Maximum projected browser-to-Rust payload per page.
pub const MAX_PAYLOAD_BYTES: usize = 512 * 1024;
/// Target selector component cap.
pub const MAX_TARGET_COMPONENT_BYTES: usize = 2 * 1024;
/// Target selector aggregate cap per occurrence.
pub const MAX_TARGET_BYTES: usize = 8 * 1024;
/// Bounded element HTML snippet cap.
pub const MAX_HTML_BYTES: usize = 2 * 1024;
/// Failure/help description cap.
pub const MAX_TEXT_BYTES: usize = 4 * 1024;
/// Identifier cap (rule, tag and check identifiers).
pub const MAX_IDENTIFIER_BYTES: usize = 256;
/// Help URL cap.
pub const MAX_HELP_URL_BYTES: usize = 2 * 1024;
/// Selected-rule inventory cap. This is an inventory, separate from retained result groups.
pub const MAX_SELECTED_RULES: usize = 256;
/// Tags retained per rule.
pub const MAX_TAGS: usize = 32;
/// Audit gets this much time after a successful render capture.
pub const AUDIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
/// Absolute per-job ceiling, including the existing render deadline and the audit allowance.
pub const MAX_AUDIT_JOB: std::time::Duration = std::time::Duration::from_secs(25);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    #[default]
    NotRequested,
    RequiresBrowser,
    NotApplicable,
    Pending,
    Running,
    Completed,
    /// The scan ran but some rules, nodes or payload detail was dropped by a documented bound.
    Partial,
    TimedOut,
    Failed,
    Stopped,
    /// The scan was declined before running, for example by the preflight element limit.
    Limited,
}

impl ExecutionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotRequested => "Not requested",
            Self::RequiresBrowser => "Requires Browser mode",
            Self::NotApplicable => "Not applicable",
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Partial => "Partial",
            Self::TimedOut => "Timed out",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Limited => "Limited",
        }
    }

    /// A completed or partial scan can retain real rule findings. Zero violations are only an
    /// all-clear when the state is exactly `Completed`.
    pub fn has_results(self) -> bool {
        matches!(self, Self::Completed | Self::Partial)
    }

    pub fn coverage_gap(self) -> bool {
        !matches!(self, Self::NotRequested | Self::Completed)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Impact {
    #[default]
    Unknown,
    Minor,
    Moderate,
    Serious,
    Critical,
}

impl Impact {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Minor => "Minor",
            Self::Moderate => "Moderate",
            Self::Serious => "Serious",
            Self::Critical => "Critical",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    #[default]
    Violation,
    NeedsReview,
}

impl ResultKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Violation => "Violation",
            Self::NeedsReview => "Needs review",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn label(self) -> String {
        format!("{}×{}", self.width, self.height)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct OutcomeSummary {
    pub violation_rules: usize,
    pub incomplete_rules: usize,
    pub pass_rules: usize,
    pub inapplicable_rules: usize,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Coverage {
    pub elements_limit: usize,
    pub preflight_elements: Option<usize>,
    pub preflight_limited: bool,
    pub rules_reported: Option<usize>,
    pub rules_retained: usize,
    pub rules_truncated: bool,
    pub nodes_reported: Option<usize>,
    pub nodes_retained: usize,
    pub nodes_truncated: bool,
    pub occurrences_reported: Option<usize>,
    pub occurrences_retained: usize,
    pub payload_bytes: usize,
    pub payload_limited: bool,
    pub budget_limited: bool,
    pub run_limited: bool,
    pub notes: Vec<String>,
}

impl Coverage {
    pub fn has_gap(&self) -> bool {
        self.preflight_limited
            || self.rules_truncated
            || self.nodes_truncated
            || self.payload_limited
            || self.budget_limited
            || self.run_limited
    }

    pub fn retained_label(&self) -> String {
        if self.rules_truncated || self.nodes_truncated {
            format!(
                "{} rule(s), {} occurrence(s) retained",
                self.rules_retained, self.nodes_retained
            )
        } else {
            format!(
                "{} rule(s), {} occurrence(s)",
                self.rules_retained, self.nodes_retained
            )
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct NodeOccurrence {
    pub ordinal: usize,
    pub impact: Impact,
    /// Each inner vector is one selector path. Nested paths are preserved rather than flattened.
    pub target: Vec<Vec<String>>,
    pub target_truncated: bool,
    pub html: String,
    pub html_truncated: bool,
    pub failure: String,
    pub failure_truncated: bool,
}

impl NodeOccurrence {
    pub fn target_label(&self) -> String {
        if self.target.is_empty() {
            return "No selector reported".into();
        }
        self.target
            .iter()
            .map(|path| path.join(" >>> "))
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RuleResult {
    pub rule_id: String,
    pub title: String,
    pub description: String,
    pub impact: Impact,
    pub kind: ResultKind,
    pub tags: Vec<String>,
    pub help_url: String,
    pub nodes_reported: usize,
    pub nodes_truncated: bool,
    /// Rule identity/description/help URL were shortened at a field bound.
    pub fields_truncated: bool,
    /// The retained tag list was shortened at its count or identifier bound.
    pub tags_truncated: bool,
    pub nodes: Vec<NodeOccurrence>,
}

impl RuleResult {
    pub fn nodes_retained(&self) -> usize {
        self.nodes.len()
    }
}

/// The bounded scan payload produced in the browser and validated in the host.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Scan {
    #[serde(skip)]
    pub state: ExecutionState,
    #[serde(skip)]
    pub reason: String,
    #[serde(skip)]
    pub requested_url: String,
    #[serde(skip)]
    pub observed_url: String,
    #[serde(skip)]
    pub frame_id: String,
    #[serde(skip)]
    pub browser: String,
    #[serde(skip)]
    pub elapsed_ms: u64,
    #[serde(skip)]
    pub scanned_at: u64,
    /// The document URL reported by the isolated world at scan time, used for identity guards.
    pub document_url: String,
    pub engine_version: String,
    pub profile: String,
    pub tags: Vec<String>,
    pub viewport: Viewport,
    pub preflight_elements: Option<usize>,
    pub selected_rules: Vec<String>,
    pub selected_rules_truncated: bool,
    /// Rule IDs returned by the engine across violations, incomplete, passes and inapplicable.
    pub returned_rules: Vec<String>,
    pub returned_rules_truncated: bool,
    pub outcome: OutcomeSummary,
    pub coverage: Coverage,
    pub rules: Vec<RuleResult>,
    pub payload_bytes: usize,
}

impl Scan {
    pub fn declined(
        state: ExecutionState,
        reason: impl Into<String>,
        requested_url: impl Into<String>,
        viewport: Viewport,
        browser: impl Into<String>,
    ) -> Self {
        Self {
            state,
            reason: reason.into(),
            requested_url: requested_url.into(),
            observed_url: String::new(),
            frame_id: String::new(),
            browser: browser.into(),
            elapsed_ms: 0,
            scanned_at: now_unix(),
            document_url: String::new(),
            engine_version: ENGINE_VERSION.into(),
            profile: PROFILE_ID.into(),
            tags: PROFILE_TAGS.iter().map(|tag| (*tag).to_string()).collect(),
            viewport,
            preflight_elements: None,
            selected_rules: Vec::new(),
            selected_rules_truncated: false,
            returned_rules: Vec::new(),
            returned_rules_truncated: false,
            outcome: OutcomeSummary::default(),
            coverage: Coverage {
                elements_limit: MAX_PREFLIGHT_ELEMENTS,
                ..Coverage::default()
            },
            rules: Vec::new(),
            payload_bytes: 0,
        }
    }

    pub fn occurrence_count(&self) -> usize {
        self.rules.iter().map(|rule| rule.nodes.len()).sum()
    }

    /// Conservative retained-byte estimate for the run ledger. Strings are charged at their
    /// current length plus a fixed per-structure overhead.
    pub fn retained_bytes(&self) -> usize {
        let mut bytes = 768usize;
        for value in [
            &self.reason,
            &self.requested_url,
            &self.observed_url,
            &self.document_url,
            &self.frame_id,
            &self.browser,
            &self.engine_version,
            &self.profile,
        ] {
            bytes = bytes.saturating_add(value.len().saturating_add(32));
        }
        for tag in &self.tags {
            bytes = bytes.saturating_add(tag.len().saturating_add(16));
        }
        for rule in &self.selected_rules {
            bytes = bytes.saturating_add(rule.len().saturating_add(24));
        }
        for rule in &self.returned_rules {
            bytes = bytes.saturating_add(rule.len().saturating_add(24));
        }
        for note in &self.coverage.notes {
            bytes = bytes.saturating_add(note.len().saturating_add(16));
        }
        for rule in &self.rules {
            bytes = bytes.saturating_add(192);
            for value in [
                &rule.rule_id,
                &rule.title,
                &rule.description,
                &rule.help_url,
            ] {
                bytes = bytes.saturating_add(value.len().saturating_add(24));
            }
            for tag in &rule.tags {
                bytes = bytes.saturating_add(tag.len().saturating_add(16));
            }
            for node in &rule.nodes {
                bytes = bytes.saturating_add(160);
                for path in &node.target {
                    for part in path {
                        bytes = bytes.saturating_add(part.len().saturating_add(24));
                    }
                }
                bytes = bytes.saturating_add(node.html.len().saturating_add(24));
                bytes = bytes.saturating_add(node.failure.len().saturating_add(24));
            }
        }
        bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadError {
    Oversize { bytes: usize },
    Invalid(String),
    WrongEngine(String),
    WrongProfile(String),
    TooManyRules(usize),
    TooManyNodes(usize),
}

impl fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversize { bytes } => {
                write!(
                    formatter,
                    "the accessibility payload is {bytes} bytes; the cap is {MAX_PAYLOAD_BYTES}"
                )
            }
            Self::Invalid(reason) => {
                write!(formatter, "the accessibility payload is invalid: {reason}")
            }
            Self::WrongEngine(version) => write!(
                formatter,
                "the accessibility payload came from axe {version}, not the pinned {ENGINE_VERSION}"
            ),
            Self::WrongProfile(profile) => {
                write!(
                    formatter,
                    "the accessibility payload used profile {profile}, not {PROFILE_ID}"
                )
            }
            Self::TooManyRules(rules) => write!(
                formatter,
                "the accessibility payload retained {rules} rules; the page cap is {MAX_RULES_PER_PAGE}"
            ),
            Self::TooManyNodes(nodes) => write!(
                formatter,
                "the accessibility payload retained {nodes} nodes; the page cap is {MAX_NODES_PER_PAGE}"
            ),
        }
    }
}

impl std::error::Error for PayloadError {}

/// Validate and defensively re-bound a projected payload from the browser.
pub fn decode_payload(bytes: &[u8]) -> Result<Scan, PayloadError> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(PayloadError::Oversize { bytes: bytes.len() });
    }
    let mut scan: Scan =
        serde_json::from_slice(bytes).map_err(|error| PayloadError::Invalid(error.to_string()))?;
    if scan.engine_version != ENGINE_VERSION {
        return Err(PayloadError::WrongEngine(scan.engine_version));
    }
    if scan.profile != PROFILE_ID {
        return Err(PayloadError::WrongProfile(scan.profile));
    }
    if scan.tags.len() != PROFILE_TAGS.len()
        || !scan
            .tags
            .iter()
            .zip(PROFILE_TAGS)
            .all(|(reported, pinned)| reported == pinned)
    {
        return Err(PayloadError::WrongProfile(scan.profile));
    }
    if scan.rules.len() > MAX_RULES_PER_PAGE {
        return Err(PayloadError::TooManyRules(scan.rules.len()));
    }
    let mut nodes = 0usize;
    for rule in &mut scan.rules {
        if rule.tags.len() > MAX_TAGS {
            rule.tags.truncate(MAX_TAGS);
            rule.tags_truncated = true;
        }
        for tag in &mut rule.tags {
            let (value, cut) = truncate_utf8(tag, MAX_IDENTIFIER_BYTES);
            *tag = value;
            rule.tags_truncated |= cut;
        }
        let (id, id_cut) = truncate_utf8(&rule.rule_id, MAX_IDENTIFIER_BYTES);
        rule.rule_id = id;
        let (title, title_cut) = truncate_utf8(&rule.title, MAX_TEXT_BYTES);
        rule.title = title;
        let (description, description_cut) = truncate_utf8(&rule.description, MAX_TEXT_BYTES);
        rule.description = description;
        let (help_url, help_cut) = truncate_utf8(&rule.help_url, MAX_HELP_URL_BYTES);
        rule.help_url = help_url;
        rule.fields_truncated |= id_cut || title_cut || description_cut || help_cut;
        for node in &mut rule.nodes {
            nodes = nodes.saturating_add(1);
            let (html, html_cut) = truncate_utf8(&node.html, MAX_HTML_BYTES);
            node.html = html;
            node.html_truncated |= html_cut;
            let (failure, failure_cut) = truncate_utf8(&node.failure, MAX_TEXT_BYTES);
            node.failure = failure;
            node.failure_truncated |= failure_cut;
            clamp_target(&mut node.target, &mut node.target_truncated);
        }
    }
    if nodes > MAX_NODES_PER_PAGE {
        return Err(PayloadError::TooManyNodes(nodes));
    }
    if scan.selected_rules.len() > MAX_SELECTED_RULES {
        return Err(PayloadError::Invalid(format!(
            "{} selected rules exceed the {MAX_SELECTED_RULES} inventory cap",
            scan.selected_rules.len()
        )));
    }
    for rule in &mut scan.selected_rules {
        *rule = truncate_utf8(rule, MAX_IDENTIFIER_BYTES).0;
    }
    if scan.returned_rules.len() > MAX_SELECTED_RULES {
        scan.returned_rules.truncate(MAX_SELECTED_RULES);
        scan.returned_rules_truncated = true;
    }
    for rule in &mut scan.returned_rules {
        let (value, cut) = truncate_utf8(rule, MAX_IDENTIFIER_BYTES);
        *rule = value;
        scan.returned_rules_truncated |= cut;
    }
    if scan.coverage.notes.len() > 16 {
        scan.coverage.notes.truncate(16);
    }
    for note in &mut scan.coverage.notes {
        *note = truncate_utf8(note, MAX_TEXT_BYTES).0;
    }
    scan.payload_bytes = bytes.len();
    scan.coverage.payload_bytes = bytes.len();
    if matches!(
        scan.state,
        ExecutionState::NotRequested | ExecutionState::Pending | ExecutionState::Running
    ) {
        scan.state = ExecutionState::Completed;
    }
    scan.coverage.rules_retained = scan.rules.len();
    scan.coverage.nodes_retained = nodes;
    scan.coverage.occurrences_retained = nodes;
    if scan.coverage.preflight_elements.is_none() {
        scan.coverage.preflight_elements = scan.preflight_elements;
    }
    scan.state = authoritative_state(&scan);
    if scan.state == ExecutionState::Partial && scan.reason.is_empty() {
        scan.reason =
            "Some accessibility rules or occurrences were dropped by a documented bound".into();
    }
    if scan.state == ExecutionState::Limited && scan.reason.is_empty() {
        scan.reason = "The accessibility audit was declined by a documented bound".into();
    }
    Ok(scan)
}

fn authoritative_state(scan: &Scan) -> ExecutionState {
    match scan.state {
        ExecutionState::Completed | ExecutionState::Partial => {
            if scan.coverage.preflight_limited
                || (scan.coverage.payload_limited && scan.rules.is_empty())
            {
                ExecutionState::Limited
            } else if scan.coverage.has_gap() {
                ExecutionState::Partial
            } else {
                ExecutionState::Completed
            }
        }
        state => state,
    }
}

fn clamp_target(target: &mut Vec<Vec<String>>, truncated: &mut bool) {
    for path in target.iter_mut() {
        for part in path.iter_mut() {
            let (value, cut) = truncate_utf8(part, MAX_TARGET_COMPONENT_BYTES);
            *part = value;
            *truncated |= cut;
        }
    }
    let mut used = 0usize;
    let mut keep_paths = 0usize;
    'paths: for path in target.iter() {
        for part in path {
            let cost = part.len().saturating_add(1);
            if used.saturating_add(cost) > MAX_TARGET_BYTES {
                *truncated = true;
                break 'paths;
            }
            used = used.saturating_add(cost);
        }
        keep_paths += 1;
    }
    if keep_paths < target.len() {
        target.truncate(keep_paths);
        *truncated = true;
    }
}

/// UTF-8-safe truncation to at most `max` bytes.
pub fn truncate_utf8(value: &str, max: usize) -> (String, bool) {
    if value.len() <= max {
        return (value.to_string(), false);
    }
    let mut cut = max.min(value.len());
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    (value[..cut].to_string(), true)
}

/// Conservative JSON string size, including the six-byte `\u00XX` form for control characters.
pub fn json_escaped_len(value: &str) -> usize {
    value
        .chars()
        .map(|ch| match ch {
            '"' | '\\' => 2,
            ch if (ch as u32) < 0x20 => 6,
            ch => ch.len_utf8(),
        })
        .sum()
}

/// Conservative CSV-encoded field size for the shared quoted writer.
pub fn csv_field_len(value: &str) -> usize {
    let quoted = value.chars().filter(|ch| *ch == '"').count();
    let formula = matches!(
        value
            .chars()
            .find(|ch| !ch.is_whitespace() && !ch.is_control() && *ch != '\u{feff}'),
        Some('=' | '+' | '-' | '@')
    );
    2usize
        .saturating_add(usize::from(formula))
        .saturating_add(value.len())
        .saturating_add(quoted)
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Feature-specific run ledger. Never spends the structured-data budget.
#[derive(Debug, Default)]
pub struct Budget {
    used: AtomicUsize,
    occurrences: AtomicUsize,
}

impl Budget {
    pub fn try_reserve(&self, bytes: usize, occurrences: usize) -> bool {
        if occurrences > MAX_RUN_OCCURRENCES {
            return false;
        }
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > BUDGET_BYTES {
                return false;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }

        let mut current = self.occurrences.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(occurrences) else {
                self.release(bytes, 0);
                return false;
            };
            if next > MAX_RUN_OCCURRENCES {
                self.release(bytes, 0);
                return false;
            }
            match self.occurrences.compare_exchange_weak(
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

    pub fn release(&self, bytes: usize, occurrences: usize) {
        if bytes > 0 {
            let _ = self
                .used
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_sub(bytes))
                });
        }
        if occurrences > 0 {
            let _ = self
                .occurrences
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_sub(occurrences))
                });
        }
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    pub fn occurrences(&self) -> usize {
        self.occurrences.load(Ordering::Relaxed)
    }

    pub fn remaining(&self) -> usize {
        BUDGET_BYTES.saturating_sub(self.used())
    }
}

/// A reservation owned by the retained report itself, so page snapshots and cancelled exports
/// cannot release bytes while the evidence is still alive.
#[derive(Debug)]
struct EvidenceLease {
    budget: Arc<Budget>,
    bytes: usize,
    occurrences: usize,
}

impl Drop for EvidenceLease {
    fn drop(&mut self) {
        self.budget.release(self.bytes, self.occurrences);
    }
}

/// Immutable retained evidence for one page. `Page` shares this behind `Arc`.
#[derive(Debug)]
pub struct Report {
    pub scan: Scan,
    pub retained_bytes: usize,
    /// Outcome counts known before the run ledger forced rule-node evidence to be dropped.
    /// They keep page classification truthful even when no node is retained.
    known_violation_occurrences: usize,
    known_needs_review_occurrences: usize,
    known_max_impact: Impact,
    _lease: Option<EvidenceLease>,
}

impl Default for Report {
    fn default() -> Self {
        Self::not_requested()
    }
}

impl PartialEq for Report {
    fn eq(&self, other: &Self) -> bool {
        self.scan == other.scan
            && self.retained_bytes == other.retained_bytes
            && self.known_violation_occurrences == other.known_violation_occurrences
            && self.known_needs_review_occurrences == other.known_needs_review_occurrences
            && self.known_max_impact == other.known_max_impact
    }
}

impl Eq for Report {}

fn known_outcome(scan: &Scan) -> (usize, usize, Impact) {
    let mut violations = 0usize;
    let mut needs_review = 0usize;
    let mut max_impact = Impact::Unknown;
    for rule in &scan.rules {
        if rule.nodes.is_empty() {
            continue;
        }
        match rule.kind {
            ResultKind::Violation => violations = violations.saturating_add(rule.nodes.len()),
            ResultKind::NeedsReview => needs_review = needs_review.saturating_add(rule.nodes.len()),
        }
        max_impact = max_impact.max(rule.impact).max(
            rule.nodes
                .iter()
                .map(|node| node.impact)
                .max()
                .unwrap_or_default(),
        );
    }
    (violations, needs_review, max_impact)
}

impl Report {
    pub fn not_requested() -> Self {
        Self::unleased(Scan {
            state: ExecutionState::NotRequested,
            coverage: Coverage {
                elements_limit: MAX_PREFLIGHT_ELEMENTS,
                ..Coverage::default()
            },
            ..Scan::default()
        })
    }

    pub fn requires_browser(requested_url: impl Into<String>, viewport: Viewport) -> Self {
        Self::unleased(Scan::declined(
            ExecutionState::RequiresBrowser,
            "Accessibility checks require Browser mode; HTML captures were not audited.",
            requested_url,
            viewport,
            "",
        ))
    }

    pub fn declined(
        state: ExecutionState,
        reason: impl Into<String>,
        requested_url: impl Into<String>,
        viewport: Viewport,
        browser: impl Into<String>,
    ) -> Self {
        Self::unleased(Scan::declined(
            state,
            reason,
            requested_url,
            viewport,
            browser,
        ))
    }

    fn unleased(scan: Scan) -> Self {
        let retained_bytes = scan.retained_bytes();
        let (known_violation_occurrences, known_needs_review_occurrences, known_max_impact) =
            known_outcome(&scan);
        Self {
            scan,
            retained_bytes,
            known_violation_occurrences,
            known_needs_review_occurrences,
            known_max_impact,
            _lease: None,
        }
    }

    /// Charge the run ledger for retained findings. If the page cannot fit, keep its truthful
    /// execution/coverage facts and rule/result groups but drop the node evidence and mark the
    /// loss visible. The pre-drop outcome counts remain available for page classification.
    pub fn from_scan(mut scan: Scan, budget: &Arc<Budget>) -> Self {
        let (known_violation_occurrences, known_needs_review_occurrences, known_max_impact) =
            known_outcome(&scan);
        let bytes = scan.retained_bytes();
        let occurrences = scan.occurrence_count();
        if budget.try_reserve(bytes, occurrences) {
            return Self {
                scan,
                retained_bytes: bytes,
                known_violation_occurrences,
                known_needs_review_occurrences,
                known_max_impact,
                _lease: Some(EvidenceLease {
                    budget: budget.clone(),
                    bytes,
                    occurrences,
                }),
            };
        }

        for rule in &mut scan.rules {
            rule.nodes_truncated |= !rule.nodes.is_empty();
            rule.nodes.clear();
        }
        scan.coverage.rules_retained = scan.rules.len();
        scan.coverage.nodes_retained = 0;
        scan.coverage.occurrences_retained = 0;
        scan.coverage.budget_limited = true;
        scan.coverage.run_limited = true;
        scan.coverage.rules_truncated =
            scan.coverage.rules_reported.unwrap_or(0) > scan.rules.len();
        scan.coverage.nodes_truncated =
            scan.coverage.nodes_truncated || scan.coverage.nodes_reported.unwrap_or(0) > 0;
        if matches!(
            scan.state,
            ExecutionState::Completed | ExecutionState::Partial
        ) {
            scan.state = ExecutionState::Limited;
        }
        scan.reason = format!(
            "Accessibility evidence exceeded the {} MiB run budget or the {MAX_RUN_OCCURRENCES}-occurrence run cap; rule summaries remain, occurrence detail was dropped",
            BUDGET_BYTES / (1024 * 1024)
        );
        let mut bytes = scan.retained_bytes();
        let mut lease = budget.try_reserve(bytes, 0).then(|| EvidenceLease {
            budget: budget.clone(),
            bytes,
            occurrences: 0,
        });
        if lease.is_none() {
            // The ledger is already full. Retain only scalar aggregate outcome/coverage facts and
            // fixed engine metadata; no uncharged input-derived vectors or strings remain.
            scan.rules.clear();
            scan.selected_rules.clear();
            scan.returned_rules.clear();
            scan.tags.clear();
            scan.coverage.notes.clear();
            scan.requested_url.clear();
            scan.observed_url.clear();
            scan.document_url.clear();
            scan.frame_id.clear();
            scan.browser.clear();
            scan.coverage.rules_retained = 0;
            scan.coverage.nodes_retained = 0;
            scan.coverage.occurrences_retained = 0;
            scan.coverage.rules_truncated = scan.coverage.rules_reported.unwrap_or(0) > 0;
            scan.coverage.nodes_truncated = scan.coverage.nodes_reported.unwrap_or(0) > 0;
            scan.reason = format!(
                "Accessibility evidence exceeded the {} MiB run budget or the {MAX_RUN_OCCURRENCES}-occurrence run cap; occurrence and rule detail were dropped, aggregate outcome counts remain",
                BUDGET_BYTES / (1024 * 1024)
            );
            bytes = scan.retained_bytes();
            lease = budget.try_reserve(bytes, 0).then(|| EvidenceLease {
                budget: budget.clone(),
                bytes,
                occurrences: 0,
            });
        }
        Self {
            scan,
            retained_bytes: bytes,
            known_violation_occurrences,
            known_needs_review_occurrences,
            known_max_impact,
            _lease: lease,
        }
    }

    pub fn state(&self) -> ExecutionState {
        self.scan.state
    }

    pub fn violation_occurrences(&self) -> usize {
        if self.scan.coverage.budget_limited {
            return self.known_violation_occurrences;
        }
        self.scan
            .rules
            .iter()
            .filter(|rule| rule.kind == ResultKind::Violation)
            .map(RuleResult::nodes_retained)
            .sum()
    }

    pub fn needs_review_occurrences(&self) -> usize {
        if self.scan.coverage.budget_limited {
            return self.known_needs_review_occurrences;
        }
        self.scan
            .rules
            .iter()
            .filter(|rule| rule.kind == ResultKind::NeedsReview)
            .map(RuleResult::nodes_retained)
            .sum()
    }

    pub fn confined_violation_rules(&self) -> usize {
        self.scan
            .rules
            .iter()
            .filter(|rule| {
                rule.kind == ResultKind::Violation
                    && (!rule.nodes.is_empty()
                        || (self.scan.coverage.budget_limited && rule.nodes_reported > 0))
            })
            .count()
    }

    pub fn max_impact(&self) -> Impact {
        if self.scan.coverage.budget_limited {
            return self.known_max_impact;
        }
        self.scan
            .rules
            .iter()
            .filter(|rule| !rule.nodes.is_empty())
            .flat_map(|rule| {
                std::iter::once(rule.impact).chain(rule.nodes.iter().map(|node| node.impact))
            })
            .max()
            .unwrap_or(Impact::Unknown)
    }

    pub fn is_finding(&self) -> bool {
        self.violation_occurrences() > 0
    }

    pub fn is_incomplete(&self) -> bool {
        self.has_coverage_gap() || self.needs_review_occurrences() > 0
    }

    /// Execution/coverage gaps only. Needs-review results are a separate outcome and do not by
    /// themselves mean the scan failed to run.
    pub fn has_coverage_gap(&self) -> bool {
        self.state().coverage_gap() || self.scan.coverage.has_gap()
    }

    /// True only when the scan completed within scope with no hidden coverage loss and found no
    /// automated violations. This is the only state that may say "No automated violations found".
    pub fn zero_violation_all_clear(&self) -> bool {
        self.state() == ExecutionState::Completed
            && !self.scan.coverage.has_gap()
            && self.violation_occurrences() == 0
    }

    pub fn coverage_label(&self) -> String {
        if self.state().coverage_gap() {
            format!(
                "{} · {}",
                self.state().label(),
                self.scan.coverage.retained_label()
            )
        } else {
            self.scan.coverage.retained_label()
        }
    }

    /// Field-wise, case-insensitive search over every retained URL/rule/selector/diagnostic field.
    /// There is no concatenated-prefix truncation.
    pub fn matches(&self, query_lower: &str) -> bool {
        if query_lower.is_empty() {
            return true;
        }
        let scan = &self.scan;
        if contains_lower(&scan.requested_url, query_lower)
            || contains_lower(&scan.observed_url, query_lower)
            || contains_lower(scan.state.label(), query_lower)
            || contains_lower(&scan.reason, query_lower)
            || contains_lower(&scan.browser, query_lower)
            || contains_lower(&scan.engine_version, query_lower)
            || contains_lower(&scan.profile, query_lower)
            || scan.tags.iter().any(|tag| contains_lower(tag, query_lower))
            || scan
                .selected_rules
                .iter()
                .any(|rule| contains_lower(rule, query_lower))
            || scan
                .coverage
                .notes
                .iter()
                .any(|note| contains_lower(note, query_lower))
        {
            return true;
        }
        scan.rules.iter().any(|rule| {
            contains_lower(&rule.rule_id, query_lower)
                || contains_lower(&rule.title, query_lower)
                || contains_lower(&rule.description, query_lower)
                || contains_lower(rule.impact.label(), query_lower)
                || contains_lower(rule.kind.label(), query_lower)
                || contains_lower(&rule.help_url, query_lower)
                || rule.tags.iter().any(|tag| contains_lower(tag, query_lower))
                || rule.nodes.iter().any(|node| {
                    contains_lower(node.impact.label(), query_lower)
                        || contains_lower(&node.html, query_lower)
                        || contains_lower(&node.failure, query_lower)
                        || node
                            .target
                            .iter()
                            .any(|path| path.iter().any(|part| contains_lower(part, query_lower)))
                })
        })
    }
}

fn contains_lower(haystack: &str, needle_lower: &str) -> bool {
    haystack.to_lowercase().contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn scan_json(rules: serde_json::Value) -> Vec<u8> {
        serde_json::json!({
            "engineVersion": ENGINE_VERSION,
            "profile": PROFILE_ID,
            "tags": PROFILE_TAGS,
            "viewport": {"width": 1280, "height": 720},
            "preflightElements": 10,
            "selectedRules": ["image-alt"],
            "selectedRulesTruncated": false,
            "returnedRules": ["image-alt", "label"],
            "returnedRulesTruncated": false,
            "outcome": {
                "violationRules": 1,
                "incompleteRules": 1,
                "passRules": 2,
                "inapplicableRules": 3
            },
            "coverage": {
                "elementsLimit": MAX_PREFLIGHT_ELEMENTS,
                "preflightElements": 10,
                "preflightLimited": false,
                "rulesReported": 2,
                "rulesRetained": 2,
                "rulesTruncated": false,
                "nodesReported": 2,
                "nodesRetained": 2,
                "nodesTruncated": false,
                "occurrencesReported": 2,
                "occurrencesRetained": 2,
                "payloadBytes": 0,
                "payloadLimited": false,
                "budgetLimited": false,
                "runLimited": false,
                "notes": []
            },
            "rules": rules,
            "payloadBytes": 0
        })
        .to_string()
        .into_bytes()
    }

    fn node(impact: &str, html: &str) -> serde_json::Value {
        serde_json::json!({
            "ordinal": 1,
            "impact": impact,
            "target": [["#main", "img.hero"]],
            "targetTruncated": false,
            "html": html,
            "htmlTruncated": false,
            "failure": "Fix any of the following: element has no alt",
            "failureTruncated": false
        })
    }

    fn rule(kind: &str, impact: &str, nodes: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "ruleId": "image-alt",
            "title": "Images must have alternate text",
            "description": "Ensures every image has alternate text",
            "impact": impact,
            "kind": kind,
            "tags": ["wcag2a", "wcag111"],
            "helpUrl": "https://dequeuniversity.com/rules/axe/4.14/image-alt",
            "nodesReported": 1,
            "nodesTruncated": false,
            "nodes": nodes
        })
    }

    #[test]
    fn provenance_matches_the_pinned_engine() {
        assert_eq!(ENGINE_SOURCE.len(), 653_780);
        let digest = Sha256::digest(ENGINE_SOURCE.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(digest, ENGINE_SHA256);
        let header = &ENGINE_SOURCE[..200];
        assert!(header.contains("axe v4.14.1"));
        assert!(header.contains("Mozilla Public"));
        assert!(ENGINE_LICENSE_TEXT.contains("Mozilla Public License"));
        assert!(!ENGINE_NOTICES.trim().is_empty());
        assert!(ENGINE_SOURCE_URL.contains("4.14.1"));
    }

    #[test]
    fn budget_reserves_bytes_and_occurrences_and_releases_on_drop() {
        let budget = Arc::new(Budget::default());
        assert!(budget.try_reserve(100, 2));
        assert_eq!(budget.used(), 100);
        assert_eq!(budget.occurrences(), 2);
        let lease = EvidenceLease {
            budget: budget.clone(),
            bytes: 100,
            occurrences: 2,
        };
        drop(lease);
        assert_eq!(budget.used(), 0);
        assert_eq!(budget.occurrences(), 0);
        assert!(!budget.try_reserve(BUDGET_BYTES + 1, 0));
        assert!(!budget.try_reserve(0, MAX_RUN_OCCURRENCES + 1));
    }

    #[test]
    fn decode_rejects_oversize_wrong_engine_and_structural_overrun() {
        let oversized = vec![b'{'; MAX_PAYLOAD_BYTES + 1];
        assert!(matches!(
            decode_payload(&oversized),
            Err(PayloadError::Oversize { .. })
        ));

        let mut payload = scan_json(serde_json::json!([]));
        let mut value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        value["engineVersion"] = serde_json::json!("4.13.0");
        payload = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            decode_payload(&payload),
            Err(PayloadError::WrongEngine(_))
        ));

        let rules: Vec<_> = (0..=MAX_RULES_PER_PAGE)
            .map(|_| rule("violation", "serious", serde_json::json!([])))
            .collect();
        let mut value: serde_json::Value =
            serde_json::from_slice(&scan_json(serde_json::json!(rules))).unwrap();
        value["rules"] = serde_json::json!(rules);
        let payload = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            decode_payload(&payload),
            Err(PayloadError::TooManyRules(_))
        ));
    }

    #[test]
    fn decode_clamps_long_fields_and_marks_a_partial_scan() {
        let long_html = "a".repeat(MAX_HTML_BYTES + 100);
        let long_target = "b".repeat(MAX_TARGET_COMPONENT_BYTES + 100);
        let mut node = node("critical", &long_html);
        node["target"] = serde_json::json!([[long_target, "#other"]]);
        let payload = scan_json(serde_json::json!([rule(
            "violation",
            "critical",
            serde_json::json!([node])
        )]));
        let scan = decode_payload(&payload).expect("valid bounded payload");
        assert_eq!(scan.state, ExecutionState::Completed);
        assert_eq!(scan.rules.len(), 1);
        assert_eq!(scan.rules[0].nodes.len(), 1);
        assert!(scan.rules[0].nodes[0].html.len() <= MAX_HTML_BYTES);
        assert_eq!(
            scan.rules[0].nodes[0].target[0][0].len(),
            MAX_TARGET_COMPONENT_BYTES
        );
        assert_eq!(scan.selected_rules, vec!["image-alt"]);
        assert_eq!(scan.returned_rules, vec!["image-alt", "label"]);
        assert_eq!(scan.outcome.pass_rules, 2);
        assert_eq!(scan.coverage.nodes_retained, 1);
    }

    #[test]
    fn decode_flags_rule_field_and_tag_truncation() {
        let mut rule = rule("violation", "critical", serde_json::json!([]));
        rule["title"] = serde_json::json!("t".repeat(MAX_TEXT_BYTES + 10));
        rule["tags"] = serde_json::json!((0..MAX_TAGS + 2).map(|_| "wcag2a").collect::<Vec<_>>());
        let payload = scan_json(serde_json::json!([rule]));
        let scan = decode_payload(&payload).expect("valid payload");
        assert!(scan.rules[0].fields_truncated);
        assert!(scan.rules[0].tags_truncated);
    }

    #[test]
    fn report_classifies_violations_needs_review_and_max_impact() {
        let payload = scan_json(serde_json::json!([
            rule(
                "violation",
                "serious",
                serde_json::json!([node("serious", "<img>")])
            ),
            rule(
                "needs_review",
                "minor",
                serde_json::json!([node("minor", "<span>")])
            )
        ]));
        let scan = decode_payload(&payload).unwrap();
        let report = Report::from_scan(scan, &Arc::new(Budget::default()));
        assert_eq!(report.violation_occurrences(), 1);
        assert_eq!(report.needs_review_occurrences(), 1);
        assert_eq!(report.max_impact(), Impact::Serious);
        assert!(report.is_finding());
        assert!(report.is_incomplete());
        assert!(!report.zero_violation_all_clear());
    }

    #[test]
    fn zero_violations_is_all_clear_only_for_completed_unbounded_scans() {
        let payload = scan_json(serde_json::json!([rule(
            "needs_review",
            "minor",
            serde_json::json!([node("minor", "<span>")])
        )]));
        let mut scan = decode_payload(&payload).unwrap();
        scan.state = ExecutionState::Completed;
        let report = Report::from_scan(scan, &Arc::new(Budget::default()));
        assert!(report.zero_violation_all_clear());

        let mut scan = decode_payload(&payload).unwrap();
        scan.coverage.nodes_truncated = true;
        scan.state = ExecutionState::Partial;
        let report = Report::from_scan(scan, &Arc::new(Budget::default()));
        assert!(!report.zero_violation_all_clear());
    }

    #[test]
    fn retained_bytes_accounts_for_the_document_url_identity() {
        let payload = scan_json(serde_json::json!([]));
        let mut short = decode_payload(&payload).unwrap();
        short.document_url = "https://example.test/".into();
        let mut long = short.clone();
        long.document_url = format!("https://example.test/{}", "a".repeat(4_096));
        assert!(
            long.retained_bytes() >= short.retained_bytes() + 4_096,
            "the scan ledger must charge the retained document identity"
        );
    }

    #[test]
    fn retained_report_releases_its_ledger_charge_when_dropped() {
        let payload = scan_json(serde_json::json!([rule(
            "violation",
            "critical",
            serde_json::json!([node("critical", "<img>")])
        )]));
        let scan = decode_payload(&payload).unwrap();
        let budget = Arc::new(Budget::default());
        let report = Report::from_scan(scan, &budget);
        assert!(budget.used() > 0);
        assert_eq!(budget.occurrences(), 1);
        drop(report);
        assert_eq!(budget.used(), 0);
        assert_eq!(budget.occurrences(), 0);
    }

    #[test]
    fn budget_exhaustion_keeps_truthful_summary_and_drops_occurrences() {
        let payload = scan_json(serde_json::json!([
            rule(
                "violation",
                "critical",
                serde_json::json!([node("critical", "<img>")])
            ),
            rule(
                "needs_review",
                "moderate",
                serde_json::json!([node("moderate", "<span>")])
            )
        ]));
        let scan = decode_payload(&payload).unwrap();
        let budget = Arc::new(Budget::default());
        assert!(budget.try_reserve(BUDGET_BYTES, 0));
        let report = Report::from_scan(scan, &budget);
        assert_eq!(report.state(), ExecutionState::Limited);
        assert!(report.scan.coverage.budget_limited);
        assert!(report.scan.coverage.run_limited);
        assert!(
            report.scan.rules.is_empty(),
            "a fully exhausted ledger retains no uncharged rule summaries"
        );
        assert_eq!(report.violation_occurrences(), 1);
        assert_eq!(report.needs_review_occurrences(), 1);
        assert_eq!(report.max_impact(), Impact::Critical);
        assert!(report.is_finding());
        assert!(report.is_incomplete());
        assert!(!report.zero_violation_all_clear());
        assert_eq!(report.scan.outcome.violation_rules, 1);
        assert_eq!(report.scan.outcome.incomplete_rules, 1);
        assert_eq!(report.scan.coverage.rules_retained, 0);
        assert_eq!(report.scan.coverage.nodes_retained, 0);
        assert_eq!(report.scan.coverage.occurrences_retained, 0);
        assert!(report.scan.coverage.nodes_truncated);
    }

    #[test]
    fn budget_exhaustion_with_only_violations_keeps_the_finding_count() {
        let payload = scan_json(serde_json::json!([rule(
            "violation",
            "serious",
            serde_json::json!([
                node("serious", "<img>"),
                node("serious", "<img class=second>")
            ])
        )]));
        let scan = decode_payload(&payload).unwrap();
        let budget = Arc::new(Budget::default());
        assert!(budget.try_reserve(BUDGET_BYTES, 0));
        let report = Report::from_scan(scan, &budget);
        assert_eq!(report.state(), ExecutionState::Limited);
        assert_eq!(report.violation_occurrences(), 2);
        assert_eq!(report.needs_review_occurrences(), 0);
        assert_eq!(report.max_impact(), Impact::Serious);
        assert!(report.is_finding());
        assert!(!report.zero_violation_all_clear());
        assert_eq!(report.confined_violation_rules(), 0);
    }

    #[test]
    fn exhausted_ledger_does_not_retain_uncharged_rule_summary_vectors() {
        let payload = scan_json(serde_json::json!([
            rule(
                "violation",
                "critical",
                serde_json::json!([node("critical", "<img>")])
            ),
            rule(
                "needs_review",
                "moderate",
                serde_json::json!([node("moderate", "<span>")])
            )
        ]));
        let budget = Arc::new(Budget::default());
        assert!(budget.try_reserve(BUDGET_BYTES, 0));
        let mut reports = Vec::new();
        for _ in 0..2 {
            let scan = decode_payload(&payload).unwrap();
            reports.push(Report::from_scan(scan, &budget));
            assert_eq!(budget.used(), BUDGET_BYTES);
            assert!(budget.used() <= BUDGET_BYTES);
        }
        for report in &reports {
            assert!(
                report.scan.rules.is_empty(),
                "an exhausted ledger must not retain uncharged rule summaries"
            );
            assert!(report.scan.selected_rules.is_empty());
            assert!(report.scan.returned_rules.is_empty());
            assert!(report.scan.tags.is_empty());
            assert!(report.scan.coverage.notes.is_empty());
            assert!(report.scan.requested_url.is_empty());
            assert!(report.scan.observed_url.is_empty());
            assert!(report.scan.document_url.is_empty());
            assert!(report.scan.frame_id.is_empty());
            assert!(report.scan.browser.is_empty());
            assert_eq!(report.scan.coverage.rules_retained, 0);
            assert_eq!(report.scan.coverage.nodes_retained, 0);
            assert_eq!(report.scan.coverage.occurrences_retained, 0);
            assert!(report.scan.coverage.rules_truncated);
            assert!(report.scan.coverage.nodes_truncated);
            assert_eq!(report.violation_occurrences(), 1);
            assert_eq!(report.needs_review_occurrences(), 1);
            assert!(report.is_finding());
            assert!(report.is_incomplete());
            assert!(!report.zero_violation_all_clear());
        }
        assert!(budget.used() <= BUDGET_BYTES);
        assert!(budget.occurrences() <= MAX_RUN_OCCURRENCES);
    }

    #[test]
    fn field_search_reaches_selectors_and_diagnostics_without_a_prefix_cut() {
        let payload = scan_json(serde_json::json!([rule(
            "violation",
            "serious",
            serde_json::json!([node("serious", "<img class=tail>")])
        )]));
        let scan = decode_payload(&payload).unwrap();
        let report = Report::from_scan(scan, &Arc::new(Budget::default()));
        assert!(report.matches("img.hero"));
        assert!(report.matches("no alt"));
        assert!(report.matches("wcag111"));
        assert!(report.matches("https://dequeuniversity.com"));
        assert!(!report.matches("not present"));
    }

    #[test]
    fn truncation_is_utf8_safe() {
        let (value, cut) = truncate_utf8("ab😀cd", 3);
        assert!(cut);
        assert_eq!(value, "ab");
        assert!(!truncate_utf8("abc", 3).1);
    }
}
