//! Evidence-backed indexability diagnostics.
//!
//! The crawl observes directives, canonical declarations and response facts. This module keeps
//! that evidence bounded, turns it into one deterministic assessment, and exposes the same
//! labels, reasons and counts to every view. It never claims that Google has indexed a URL or
//! that Google will choose the declared canonical.
//!
//! Two concepts stay separate:
//!
//! 1. Whether the crawl visited a URL at all. That belongs to the crawl and sitemap state.
//! 2. What the crawl observed when it did fetch an HTML page, plus any header evidence on a
//!    non-HTML response. That is the [`Assessment`] built here.

use sha2::{Digest, Sha256};
use url::Url;

use crate::crawl::url_key;

/// Most directive or canonical declarations retained for one page.
pub const MAX_EVIDENCE_ITEMS: usize = 16;
/// Longest raw directive or canonical href retained for one page.
pub const MAX_EVIDENCE_CHARS: usize = 512;

/// How much of a response body the crawler could read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadState {
    /// The body was read to its end within the crawl limit.
    #[default]
    Complete,
    /// The body reached the crawl's byte limit before it ended.
    Truncated,
    /// The body could not be read.
    Failed,
    /// No body was requested, for example on an error or non-HTML response.
    NotRead,
}

impl ReadState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Complete => "Complete",
            Self::Truncated => "Truncated at the body limit",
            Self::Failed => "Could not be read",
            Self::NotRead => "Not read",
        }
    }
}

/// Where a robots directive was declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceSource {
    Meta,
    Header,
}

/// One observed robots meta tag or X-Robots-Tag header line, with its scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RobotsDirective {
    pub source: EvidenceSource,
    /// `None` means the generic rule. `Some("googlebot")` is Google's crawler.
    pub agent: Option<String>,
    /// The bounded raw directive text.
    pub raw: String,
    /// `noindex` or `none` is present in this directive.
    pub noindex: bool,
    pub nofollow: bool,
    /// Either the raw directive or its agent name exceeded the evidence cap.
    pub truncated: bool,
}

impl RobotsDirective {
    /// Generic directives and Googlebot's Web Search rules are the only ones applied to the
    /// primary assessment. Other agent names are retained as scoped evidence.
    pub fn applies_to_google(&self) -> bool {
        self.agent
            .as_deref()
            .is_none_or(|agent| agent.eq_ignore_ascii_case("googlebot"))
    }

    pub fn source_label(&self) -> String {
        match (self.source, self.agent.as_deref()) {
            (EvidenceSource::Meta, None) => "Meta robots".to_string(),
            (EvidenceSource::Meta, Some(agent)) => format!("{} meta", title_case(agent)),
            (EvidenceSource::Header, None) => "X-Robots-Tag".to_string(),
            (EvidenceSource::Header, Some(agent)) => {
                format!("X-Robots-Tag ({})", title_case(agent))
            }
        }
    }

    pub fn finding_text(&self) -> String {
        format!("{}: {}", self.source_label(), self.raw)
    }
}

/// The validity of one HTML `<link rel=canonical>` declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalState {
    Valid,
    Empty,
    Invalid,
}

impl CanonicalState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Valid => "Valid",
            Self::Empty => "Empty href",
            Self::Invalid => "Invalid href",
        }
    }
}

/// One observed HTML canonical declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalDeclaration {
    pub raw: String,
    /// The normalized absolute target when the declaration is valid.
    pub resolved: Option<String>,
    /// Digest of the full normalized target, retained when the display value is clipped.
    pub resolved_hash: Option<[u8; 32]>,
    pub state: CanonicalState,
    /// Either the source href or normalized target exceeded the evidence cap.
    pub truncated: bool,
}

impl CanonicalDeclaration {
    pub fn matches_url(&self, url: &str) -> bool {
        if self.state != CanonicalState::Valid {
            return false;
        }
        let Ok(url) = Url::parse(url) else {
            return false;
        };
        self.resolved_hash == Some(target_hash(&url_key(url)))
    }

    pub fn display_target(&self) -> String {
        match (&self.state, &self.resolved) {
            (CanonicalState::Valid, Some(resolved)) => resolved.clone(),
            (CanonicalState::Valid, None) => self.raw.clone(),
            (_, _) => self.state.label().to_string(),
        }
    }
}

/// Effective restrictions observed after the retained evidence cap.
///
/// Only the facts that change an assessment are kept, so a page with thousands of
/// declarations cannot grow memory or silently drop a known block. Raw declarations beyond
/// the cap stay visible only as this bounded summary plus the incompleteness notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OmittedEvidence {
    /// An applicable generic or Googlebot directive contained `noindex` or `none`.
    pub applicable_noindex: bool,
    /// An applicable generic or Googlebot directive contained `nofollow` or `none`.
    pub applicable_nofollow: bool,
    /// A restriction scoped to another agent was omitted.
    pub scoped: bool,
}

impl OmittedEvidence {
    fn observe(&mut self, agent: Option<&str>, noindex: bool, nofollow: bool) {
        if !noindex && !nofollow {
            return;
        }
        let applies = agent.is_none_or(|agent| agent.eq_ignore_ascii_case("googlebot"));
        if applies {
            self.applicable_noindex |= noindex;
            self.applicable_nofollow |= nofollow;
        } else {
            self.scoped = true;
        }
    }
}

/// Bounded evidence collected for one fetched page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageEvidence {
    pub directives: Vec<RobotsDirective>,
    pub canonicals: Vec<CanonicalDeclaration>,
    pub read: ReadState,
    /// More evidence was present than the per-page cap allowed.
    pub truncated: bool,
    /// Effective restrictions from evidence beyond the cap, kept without retaining the raw
    /// declarations. This is what prevents a header or late meta rule from disappearing
    /// behind earlier, benign declarations.
    pub omitted: OmittedEvidence,
}

impl PageEvidence {
    pub fn bound(&mut self) {
        for directive in &mut self.directives {
            let (raw, raw_cut) = truncate(&directive.raw, MAX_EVIDENCE_CHARS);
            directive.raw = raw;
            let mut cut = raw_cut;
            if let Some(agent) = directive.agent.as_mut() {
                let (bounded_agent, agent_cut) = truncate(agent, MAX_EVIDENCE_CHARS);
                *agent = bounded_agent;
                cut |= agent_cut;
            }
            directive.truncated |= cut;
            self.truncated |= cut || directive.truncated;
        }
        for declaration in &mut self.canonicals {
            let (raw, raw_cut) = truncate(&declaration.raw, MAX_EVIDENCE_CHARS);
            declaration.raw = raw;
            let mut cut = raw_cut;
            if let Some(resolved) = declaration.resolved.as_mut() {
                if declaration.resolved_hash.is_none() {
                    declaration.resolved_hash = Some(target_hash(resolved));
                }
                let (bounded_target, target_cut) = truncate(resolved, MAX_EVIDENCE_CHARS);
                *resolved = bounded_target;
                cut |= target_cut;
            }
            declaration.truncated |= cut;
            self.truncated |= cut || declaration.truncated;
        }
        if self.directives.len() > MAX_EVIDENCE_ITEMS {
            for directive in &self.directives[MAX_EVIDENCE_ITEMS..] {
                self.omitted.observe(
                    directive.agent.as_deref(),
                    directive.noindex,
                    directive.nofollow,
                );
            }
            self.directives.truncate(MAX_EVIDENCE_ITEMS);
            self.truncated = true;
        }
        if self.canonicals.len() > MAX_EVIDENCE_ITEMS {
            self.canonicals.truncate(MAX_EVIDENCE_ITEMS);
            self.truncated = true;
        }
    }
}

/// How strongly a finding asks the reader to act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A directive or response fact that can prevent indexing.
    Issue,
    /// A known signal worth review, or an assessment that could not finish.
    Advisory,
    /// Context that does not by itself need action.
    Info,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Issue => "Issue",
            Self::Advisory => "Advisory",
            Self::Info => "Info",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindingCode {
    FetchFailed,
    BodyNotRead,
    BodyTruncated,
    EvidenceTruncated,
    HttpError,
    Redirect,
    RedirectWithoutTarget,
    NotModified,
    UnsupportedStatus,
    Noindex,
    Nofollow,
    ScopedDirective,
    CanonicalMissing,
    CanonicalSelf,
    CanonicalRepeated,
    CanonicalElsewhere,
    CanonicalEmpty,
    CanonicalInvalid,
    CanonicalConflict,
    NonHtml,
    NoBlockFound,
}

/// One supporting reason shown in the drawer and exported to CSV.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub code: FindingCode,
    pub severity: Severity,
    pub text: String,
    /// Conditional next step. Empty when there is nothing useful to suggest.
    pub guidance: String,
}

impl Finding {
    fn new(
        code: FindingCode,
        severity: Severity,
        text: impl Into<String>,
        guidance: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            text: text.into(),
            guidance: guidance.into(),
        }
    }
}

/// The single primary label shown in lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Label {
    FetchFailed,
    HttpError,
    Redirect,
    Noindex,
    ReviewCanonical,
    CanonicalElsewhere,
    #[default]
    NotAssessed,
    NoBlockFound,
}

impl Label {
    pub fn text(self) -> &'static str {
        match self {
            Self::FetchFailed => "Fetch failed",
            Self::HttpError => "HTTP error",
            Self::Redirect => "Redirect",
            Self::Noindex => "Noindex",
            Self::ReviewCanonical => "Review canonical",
            Self::CanonicalElsewhere => "Canonical elsewhere",
            Self::NotAssessed => "Not assessed",
            Self::NoBlockFound => "No block found",
        }
    }

    /// A known observation, as opposed to an unknown or a clean supported check.
    pub fn is_known(self) -> bool {
        matches!(
            self,
            Self::FetchFailed
                | Self::HttpError
                | Self::Redirect
                | Self::Noindex
                | Self::ReviewCanonical
                | Self::CanonicalElsewhere
        )
    }
}

/// The complete observed indexability picture for one fetched page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Assessment {
    pub page_url: String,
    pub label: Label,
    pub findings: Vec<Finding>,
    pub evidence: PageEvidence,
    /// Cached severity summary so filters and counts do not rescan findings per repaint.
    pub attention: bool,
    pub confirmed_issue: bool,
    /// Cached canonical summary so the Canonical count does not rebuild target lists.
    pub canonical_attention: bool,
}

impl Assessment {
    /// `Page.noindex` compatibility derivative: an applicable generic or Googlebot rule,
    /// including one seen beyond the retained evidence cap.
    pub fn has_noindex(&self) -> bool {
        self.evidence.omitted.applicable_noindex
            || self
                .evidence
                .directives
                .iter()
                .any(|directive| directive.noindex && directive.applies_to_google())
    }

    pub fn first_valid_canonical(&self) -> Option<&str> {
        self.evidence
            .canonicals
            .iter()
            .find(|declaration| declaration.state == CanonicalState::Valid)
            .and_then(|declaration| declaration.resolved.as_deref())
    }

    /// Findings worth showing under Issues only. No block found stays clean.
    pub fn needs_attention(&self) -> bool {
        self.attention
    }

    /// A directive or response fact, excluding advisories and unknowns.
    pub fn has_confirmed_issue(&self) -> bool {
        self.confirmed_issue
    }

    /// Short row/CSV reasons. Known findings and advisories are kept. When only context
    /// remains, the first context finding explains the unknown label; a clean result stays
    /// empty. Context never makes a row an issue on its own.
    pub fn reason_text(&self) -> String {
        let mut parts: Vec<&str> = self
            .findings
            .iter()
            .filter(|finding| finding.severity != Severity::Info)
            .map(|finding| finding.text.as_str())
            .collect();
        if parts.is_empty() && self.label != Label::NoBlockFound {
            match self
                .findings
                .iter()
                .find(|finding| !finding.text.is_empty())
            {
                Some(finding) => parts.push(finding.text.as_str()),
                None => parts.push(self.label.text()),
            }
        }
        parts.join(" · ")
    }

    /// Every finding, including context, in deterministic collection order.
    pub fn all_reasons_text(&self) -> String {
        self.findings
            .iter()
            .map(|finding| finding.text.as_str())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Raw directives and canonical declarations, in collection order.
    pub fn evidence_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for directive in &self.evidence.directives {
            parts.push(directive.finding_text());
        }
        for declaration in &self.evidence.canonicals {
            let evidence = match declaration.state {
                CanonicalState::Valid => format!(
                    "canonical {} → {}",
                    declaration.raw,
                    declaration.resolved.as_deref().unwrap_or("")
                ),
                CanonicalState::Empty => "canonical (empty href)".to_string(),
                CanonicalState::Invalid => {
                    format!("canonical {} (invalid)", declaration.raw)
                }
            };
            parts.push(if declaration.truncated {
                format!("{evidence} (truncated)")
            } else {
                evidence
            });
        }
        if self.evidence.omitted.applicable_noindex {
            parts.push("An applicable noindex was among directives not retained".to_string());
        }
        if self.evidence.omitted.applicable_nofollow {
            parts.push("An applicable nofollow was among directives not retained".to_string());
        }
        if self.evidence.omitted.scoped {
            parts.push("Scoped restrictions were among directives not retained".to_string());
        }
        if self.evidence.truncated {
            parts.push("More evidence was present than was captured".to_string());
        }
        if parts.is_empty() {
            "None observed".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// The Canonical tab's wording, derived from the same declarations as the assessment.
    pub fn canonical_issue_text(&self) -> String {
        if self.evidence.canonicals.is_empty() {
            return "Missing".to_string();
        }
        if self.canonical_review_required() {
            return "Review declaration".to_string();
        }
        match self.unique_canonical_targets().first() {
            Some(target)
                if !self
                    .evidence
                    .canonicals
                    .iter()
                    .find(|declaration| declaration.state == CanonicalState::Valid)
                    .is_some_and(|declaration| declaration.matches_url(&self.page_url)) =>
            {
                format!("Points to {}", short_path(target))
            }
            _ => String::new(),
        }
    }

    /// Missing, malformed or competing canonicals, or a valid canonical elsewhere.
    pub fn canonical_needs_attention(&self) -> bool {
        self.canonical_attention
    }

    pub fn read_label(&self) -> &'static str {
        self.evidence.read.label()
    }

    fn canonical_review_required(&self) -> bool {
        self.evidence
            .canonicals
            .iter()
            .any(|declaration| declaration.state != CanonicalState::Valid)
            || self.unique_canonical_targets().len() > 1
    }

    /// Every distinct valid canonical target, in declaration order.
    pub fn unique_canonical_targets(&self) -> Vec<&str> {
        let mut hashes: Vec<[u8; 32]> = Vec::new();
        let mut targets: Vec<&str> = Vec::new();
        for declaration in &self.evidence.canonicals {
            if declaration.state != CanonicalState::Valid {
                continue;
            }
            let Some(target) = declaration.resolved.as_deref() else {
                continue;
            };
            let Some(hash) = declaration.resolved_hash else {
                continue;
            };
            if !hashes.contains(&hash) {
                hashes.push(hash);
                targets.push(target);
            }
        }
        targets
    }
}

/// Build the one assessment every view reads.
pub fn assess(
    page_url: &str,
    status: u16,
    content_type: &str,
    redirect_to: &str,
    error: &str,
    evidence: PageEvidence,
) -> Assessment {
    let mut assessment = Assessment {
        page_url: page_url.to_string(),
        evidence,
        ..Assessment::default()
    };
    assessment.evidence.bound();

    let applicable_noindex = noindex_findings(&assessment.evidence);
    let canonical = canonical_findings(&assessment);
    let mut findings: Vec<Finding> = Vec::new();

    let label = if !error.trim().is_empty() {
        findings.push(Finding::new(
            FindingCode::FetchFailed,
            Severity::Issue,
            error.trim(),
            "The URL could not be read, so no indexability signal was confirmed.",
        ));
        Label::FetchFailed
    } else if assessment.evidence.read == ReadState::Failed {
        findings.push(Finding::new(
            FindingCode::FetchFailed,
            Severity::Issue,
            "The response body could not be read",
            "The URL could not be fully read, so no indexability signal was confirmed.",
        ));
        // Keep any declarations that were recovered before the read failed.
        if !assessment.evidence.canonicals.is_empty() {
            findings.extend(canonical.findings.clone());
        }
        Label::FetchFailed
    } else if status == 0 {
        findings.push(Finding::new(
            FindingCode::FetchFailed,
            Severity::Issue,
            "No response",
            "The URL did not answer, so no indexability signal was confirmed.",
        ));
        Label::FetchFailed
    } else if (400..=599).contains(&status) {
        let guidance = if matches!(status, 429 | 503) {
            "A rate limit or temporary server failure is not proof that the URL was removed. Check again later."
        } else {
            "If this URL should be available, check the server response before looking for a content rule."
        };
        findings.push(Finding::new(
            FindingCode::HttpError,
            Severity::Issue,
            status_text(status),
            guidance,
        ));
        Label::HttpError
    } else if (300..=399).contains(&status) {
        match status {
            304 => {
                findings.push(Finding::new(
                    FindingCode::NotModified,
                    Severity::Info,
                    "HTTP 304 · no content to assess",
                    "A conditional response did not include a body, so this crawl could not assess it.",
                ));
                Label::NotAssessed
            }
            301 | 302 | 303 | 307 | 308 if !redirect_to.is_empty() => {
                findings.push(Finding::new(
                    FindingCode::Redirect,
                    Severity::Advisory,
                    format!("Redirect to {redirect_to}"),
                    "Redirects can be intentional. This crawl followed the target when it stayed in scope.",
                ));
                Label::Redirect
            }
            301 | 302 | 303 | 307 | 308 => {
                findings.push(Finding::new(
                    FindingCode::RedirectWithoutTarget,
                    Severity::Advisory,
                    format!("HTTP {status} · no target location"),
                    "The response did not name a usable target, so the destination was not assessed.",
                ));
                Label::NotAssessed
            }
            _ => {
                findings.push(Finding::new(
                    FindingCode::UnsupportedStatus,
                    Severity::Info,
                    format!("HTTP {status} · not a content response"),
                    "This response status does not carry a page body to assess.",
                ));
                Label::NotAssessed
            }
        }
    } else if status != 200 {
        findings.push(Finding::new(
            FindingCode::UnsupportedStatus,
            Severity::Info,
            format!("HTTP {status} · not assessed as a page"),
            "Only a 200 HTML response is checked as a page in this version.",
        ));
        Label::NotAssessed
    } else if !is_html(content_type) {
        let blocked = !applicable_noindex.is_empty();
        if blocked {
            findings.extend(applicable_noindex.clone());
        } else {
            // A successful non-HTML response is outside this HTML check, not an SEO defect.
            // Keep the context visible without inflating Issues only or confirmed errors.
            findings.push(Finding::new(
                FindingCode::NonHtml,
                Severity::Info,
                format!("Non-HTML content ({})", mime(content_type)),
                "This crawl checks HTML indexability only. Non-HTML content can still be indexed.",
            ));
        }
        if assessment.evidence.truncated {
            findings.push(evidence_truncated_finding());
        }
        if blocked {
            Label::Noindex
        } else {
            Label::NotAssessed
        }
    } else {
        let mut label = if applicable_noindex.is_empty() {
            Label::NoBlockFound
        } else {
            Label::Noindex
        };
        findings.extend(applicable_noindex.clone());
        findings.extend(canonical.findings.clone());
        if label != Label::Noindex {
            if canonical.review {
                label = Label::ReviewCanonical;
            } else if canonical.elsewhere {
                label = Label::CanonicalElsewhere;
            }
        }
        let incomplete_read = match assessment.evidence.read {
            ReadState::Truncated => Some(Finding::new(
                FindingCode::BodyTruncated,
                Severity::Advisory,
                "The response body was truncated at the crawl's body limit",
                "Later directives or canonicals may not have been seen. The assessment is incomplete.",
            )),
            ReadState::NotRead => Some(Finding::new(
                FindingCode::BodyNotRead,
                Severity::Advisory,
                "The HTML response body was not read",
                "The assessment is incomplete because no HTML body was available to inspect.",
            )),
            ReadState::Complete | ReadState::Failed => None,
        };
        if let Some(finding) = incomplete_read {
            findings.push(finding);
            if label == Label::NoBlockFound {
                label = Label::NotAssessed;
            }
        }
        if assessment.evidence.truncated {
            findings.push(evidence_truncated_finding());
            if label == Label::NoBlockFound {
                label = Label::NotAssessed;
            }
        }
        if label == Label::NoBlockFound {
            findings.push(Finding::new(
                FindingCode::NoBlockFound,
                Severity::Info,
                "No noindex or canonical concern found in the supported checks",
                "This is not confirmation that Google has indexed the URL.",
            ));
        }
        label
    };

    // Keep header evidence visible even when a response fact wins the primary label.
    if label != Label::Noindex {
        findings.extend(applicable_noindex);
    }

    let applicable_nofollow = assessment
        .evidence
        .directives
        .iter()
        .any(|directive| directive.nofollow && directive.applies_to_google());
    if applicable_nofollow
        && !assessment
            .evidence
            .directives
            .iter()
            .any(|directive| directive.noindex && directive.applies_to_google())
    {
        findings.push(Finding::new(
            FindingCode::Nofollow,
            Severity::Info,
            "nofollow is present; it does not by itself prevent indexing",
            "",
        ));
    }

    for directive in &assessment.evidence.directives {
        if directive.applies_to_google() {
            continue;
        }
        if directive.noindex || directive.nofollow {
            findings.push(Finding::new(
                FindingCode::ScopedDirective,
                Severity::Info,
                format!(
                    "{}: {} (scoped to {}, not applied)",
                    directive.source_label(),
                    directive.raw,
                    directive.agent.as_deref().unwrap_or("another agent")
                ),
                "",
            ));
        }
    }

    assessment.label = label;
    assessment.findings = findings;
    assessment.attention = assessment
        .findings
        .iter()
        .any(|finding| finding.severity != Severity::Info);
    assessment.confirmed_issue = assessment
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Issue);
    assessment.canonical_attention =
        assessment.evidence.canonicals.is_empty() || canonical.review || canonical.elsewhere;
    assessment
}

struct CanonicalFindings {
    review: bool,
    elsewhere: bool,
    findings: Vec<Finding>,
}

fn canonical_findings(assessment: &Assessment) -> CanonicalFindings {
    let declarations = &assessment.evidence.canonicals;
    let mut findings = Vec::new();
    let mut review = false;
    let mut elsewhere = false;

    if declarations.is_empty() {
        findings.push(Finding::new(
            FindingCode::CanonicalMissing,
            Severity::Info,
            "No HTML canonical declared",
            "A missing canonical is informational; it is not proof that the URL cannot be indexed.",
        ));
        return CanonicalFindings {
            review,
            elsewhere,
            findings,
        };
    }

    let empty = declarations
        .iter()
        .filter(|declaration| declaration.state == CanonicalState::Empty)
        .count();
    let invalid = declarations
        .iter()
        .filter(|declaration| declaration.state == CanonicalState::Invalid)
        .count();
    if empty > 0 {
        review = true;
        findings.push(Finding::new(
            FindingCode::CanonicalEmpty,
            Severity::Advisory,
            format!(
                "{} canonical {} with an empty href",
                empty,
                if empty == 1 {
                    "declaration"
                } else {
                    "declarations"
                }
            ),
            "Make the canonical target an absolute or resolvable URL.",
        ));
    }
    if invalid > 0 {
        review = true;
        findings.push(Finding::new(
            FindingCode::CanonicalInvalid,
            Severity::Advisory,
            format!(
                "{} canonical {} could not be resolved",
                invalid,
                if invalid == 1 {
                    "declaration"
                } else {
                    "declarations"
                }
            ),
            "Make the canonical target an absolute or resolvable HTTP(S) URL.",
        ));
    }

    let targets = assessment.unique_canonical_targets();
    if targets.len() > 1 {
        review = true;
        findings.push(Finding::new(
            FindingCode::CanonicalConflict,
            Severity::Advisory,
            format!("Conflicting canonical targets: {}", targets.join(" · ")),
            "Leave one unambiguous canonical target so the preference is clear.",
        ));
    } else if let Some(target) = targets.first() {
        let declared = declarations
            .iter()
            .filter(|declaration| declaration.state == CanonicalState::Valid)
            .count();
        if declared > 1 {
            findings.push(Finding::new(
                FindingCode::CanonicalRepeated,
                Severity::Info,
                format!("The same canonical target is declared {declared} times"),
                "",
            ));
        }
        if declarations
            .iter()
            .find(|declaration| declaration.state == CanonicalState::Valid)
            .is_some_and(|declaration| declaration.matches_url(&assessment.page_url))
        {
            findings.push(Finding::new(
                FindingCode::CanonicalSelf,
                Severity::Info,
                "Self canonical",
                "",
            ));
        } else {
            elsewhere = true;
            findings.push(Finding::new(
                FindingCode::CanonicalElsewhere,
                Severity::Advisory,
                format!("Canonical points to {}", short_path(target)),
                "A canonical is a preference, not a guaranteed indexing block. Change it only if this URL should be canonical.",
            ));
        }
    }

    CanonicalFindings {
        review,
        elsewhere,
        findings,
    }
}

fn evidence_truncated_finding() -> Finding {
    Finding::new(
        FindingCode::EvidenceTruncated,
        Severity::Advisory,
        "More directive or canonical evidence was present than could be captured",
        "Some declarations were not inspected, so the assessment is incomplete.",
    )
}

fn noindex_findings(evidence: &PageEvidence) -> Vec<Finding> {
    let mut findings: Vec<Finding> = evidence
        .directives
        .iter()
        .filter(|directive| directive.noindex && directive.applies_to_google())
        .map(|directive| {
            Finding::new(
                FindingCode::Noindex,
                Severity::Issue,
                directive.finding_text(),
                "If this page should appear in search, remove the applicable noindex rule.",
            )
        })
        .collect();
    if evidence.omitted.applicable_noindex {
        findings.push(Finding::new(
            FindingCode::Noindex,
            Severity::Issue,
            "An applicable noindex was present among directives not retained in the evidence list",
            "If this page should appear in search, remove the applicable noindex rule.",
        ));
    }
    findings
}

fn status_text(status: u16) -> String {
    match status {
        400..=499 => format!("HTTP {status} Client error"),
        500..=599 => format!("HTTP {status} Server error"),
        status => format!("HTTP {status}"),
    }
}

fn is_html(content_type: &str) -> bool {
    content_type.to_ascii_lowercase().contains("html")
}

fn mime(content_type: &str) -> String {
    let mime = content_type.split(';').next().unwrap_or("").trim();
    if mime.is_empty() {
        "unknown type".to_string()
    } else {
        mime.to_string()
    }
}

fn short_path(url: &str) -> String {
    Url::parse(url)
        .map(|url| url.path().to_string())
        .unwrap_or_else(|_| url.to_string())
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Parse a `<meta name="robots">` or agent-specific robots meta tag.
pub fn parse_meta_directive(name: &str, content: &str) -> Option<RobotsDirective> {
    let name = name.trim();
    let content = content.trim();
    let generic = name.eq_ignore_ascii_case("robots");
    if !generic && !name.to_ascii_lowercase().contains("bot") {
        return None;
    }
    if content.is_empty() {
        return None;
    }
    let (raw, raw_truncated) = truncate(content, MAX_EVIDENCE_CHARS);
    let (agent, agent_truncated) = if generic {
        (None, false)
    } else {
        let (agent, truncated) = truncate(name, MAX_EVIDENCE_CHARS);
        (Some(agent), truncated)
    };
    let tokens = directive_tokens(content);
    Some(RobotsDirective {
        source: EvidenceSource::Meta,
        agent,
        noindex: tokens
            .iter()
            .any(|token| token == "noindex" || token == "none"),
        nofollow: tokens
            .iter()
            .any(|token| token == "nofollow" || token == "none"),
        truncated: raw_truncated || agent_truncated,
        raw,
    })
}

/// Parse every `X-Robots-Tag` header value, keeping agent scopes separate.
///
/// A scoped prefix applies to the following comma-separated directives in the same header
/// line. Directive names such as `unavailable_after` and `max-snippet` are not agent names.
///
/// At most [`MAX_EVIDENCE_ITEMS`] raw directives are retained. Any restriction past the cap
/// is folded into [`OmittedEvidence`] so a known block is never lost just because earlier
/// directives filled the list.
pub fn parse_x_robots_tag(values: &[&str]) -> (Vec<RobotsDirective>, bool, OmittedEvidence) {
    let mut directives = Vec::new();
    let mut truncated = false;
    let mut omitted = OmittedEvidence::default();
    for value in values {
        let mut agent: Option<&str> = None;
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let mut text = part;
            if let Some((prefix, rest)) = part.split_once(':') {
                let prefix = prefix.trim();
                if !is_directive_name(prefix) && is_agent_token(prefix) {
                    agent = Some(prefix);
                    text = rest.trim();
                }
            }
            if text.is_empty() {
                continue;
            }
            let tokens = directive_tokens(text);
            let noindex = tokens
                .iter()
                .any(|token| token == "noindex" || token == "none");
            let nofollow = tokens
                .iter()
                .any(|token| token == "nofollow" || token == "none");
            if directives.len() >= MAX_EVIDENCE_ITEMS {
                truncated = true;
                omitted.observe(agent, noindex, nofollow);
                continue;
            }
            let (raw, cut) = truncate(text, MAX_EVIDENCE_CHARS);
            let (agent, agent_cut) = agent
                .map(|agent| truncate(agent, MAX_EVIDENCE_CHARS))
                .map_or((None, false), |(agent, cut)| (Some(agent), cut));
            truncated |= cut || agent_cut;
            directives.push(RobotsDirective {
                source: EvidenceSource::Header,
                agent,
                noindex,
                nofollow,
                truncated: cut || agent_cut,
                raw,
            });
        }
    }
    (directives, truncated, omitted)
}

/// Resolve one canonical href against the page's base URL.
pub fn canonical_declaration(raw: Option<&str>, base: &Url) -> CanonicalDeclaration {
    let raw_value = raw.unwrap_or("").trim();
    if raw_value.is_empty() {
        return CanonicalDeclaration {
            raw: String::new(),
            resolved: None,
            resolved_hash: None,
            state: CanonicalState::Empty,
            truncated: false,
        };
    }
    let (raw, raw_truncated) = truncate(raw_value, MAX_EVIDENCE_CHARS);
    match base.join(raw_value) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => {
            let target = url_key(url);
            let resolved_hash = Some(target_hash(&target));
            let (resolved, target_truncated) = truncate(&target, MAX_EVIDENCE_CHARS);
            CanonicalDeclaration {
                raw,
                resolved: Some(resolved),
                resolved_hash,
                state: CanonicalState::Valid,
                truncated: raw_truncated || target_truncated,
            }
        }
        _ => CanonicalDeclaration {
            raw,
            resolved: None,
            resolved_hash: None,
            state: CanonicalState::Invalid,
            truncated: raw_truncated,
        },
    }
}

fn directive_tokens(content: &str) -> Vec<String> {
    content
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .map(|token| token.trim().trim_end_matches(':').to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_directive_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "noindex"
            | "nofollow"
            | "none"
            | "all"
            | "index"
            | "noarchive"
            | "nosnippet"
            | "notranslate"
            | "noimageindex"
            | "indexifembedded"
            | "max-snippet"
            | "max-image-preview"
            | "max-video-preview"
            | "unavailable_after"
            | "nositelinkssearchbox"
    )
}

fn is_agent_token(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(char::is_whitespace)
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn truncate(value: &str, limit: usize) -> (String, bool) {
    if value.chars().count() <= limit {
        return (value.to_string(), false);
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    (truncated, true)
}

fn target_hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "https://example.com/page";

    fn evidence() -> PageEvidence {
        PageEvidence {
            read: ReadState::Complete,
            ..PageEvidence::default()
        }
    }

    fn meta(agent: Option<&str>, raw: &str) -> RobotsDirective {
        let tokens = directive_tokens(raw);
        RobotsDirective {
            source: EvidenceSource::Meta,
            agent: agent.map(str::to_string),
            raw: raw.to_string(),
            noindex: tokens
                .iter()
                .any(|token| token == "noindex" || token == "none"),
            nofollow: tokens
                .iter()
                .any(|token| token == "nofollow" || token == "none"),
            truncated: false,
        }
    }

    fn canonical(raw: &str, target: &str) -> CanonicalDeclaration {
        CanonicalDeclaration {
            raw: raw.to_string(),
            resolved: Some(target.to_string()),
            resolved_hash: Some(target_hash(target)),
            state: CanonicalState::Valid,
            truncated: false,
        }
    }

    fn html200(evidence: PageEvidence) -> Assessment {
        assess(PAGE, 200, "text/html; charset=utf-8", "", "", evidence)
    }

    #[test]
    fn clean_html_200_finds_no_block_and_keeps_missing_canonical_informational() {
        let assessment = html200(evidence());
        assert_eq!(assessment.label, Label::NoBlockFound);
        assert!(!assessment.needs_attention());
        assert!(!assessment.has_confirmed_issue());
        assert_eq!(assessment.reason_text(), "");
        assert_eq!(assessment.canonical_issue_text(), "Missing");
        assert!(
            assessment
                .findings
                .iter()
                .any(|finding| finding.code == FindingCode::CanonicalMissing
                    && finding.severity == Severity::Info)
        );
    }

    #[test]
    fn canonical_declarations_distinguish_self_missing_elsewhere_and_malformed() {
        let self_canonical = html200(PageEvidence {
            canonicals: vec![canonical("/page", PAGE)],
            ..evidence()
        });
        assert_eq!(self_canonical.label, Label::NoBlockFound);
        assert_eq!(self_canonical.canonical_issue_text(), "");

        let elsewhere = html200(PageEvidence {
            canonicals: vec![canonical("/other", "https://example.com/other")],
            ..evidence()
        });
        assert_eq!(elsewhere.label, Label::CanonicalElsewhere);
        assert!(elsewhere.needs_attention());
        assert!(!elsewhere.has_confirmed_issue());
        assert_eq!(elsewhere.canonical_issue_text(), "Points to /other");

        let empty = html200(PageEvidence {
            canonicals: vec![CanonicalDeclaration {
                raw: String::new(),
                resolved: None,
                resolved_hash: None,
                state: CanonicalState::Empty,
                truncated: false,
            }],
            ..evidence()
        });
        assert_eq!(empty.label, Label::ReviewCanonical);
        assert_eq!(empty.canonical_issue_text(), "Review declaration");

        let invalid = html200(PageEvidence {
            canonicals: vec![CanonicalDeclaration {
                raw: "mailto:x@example.com".into(),
                resolved: None,
                resolved_hash: None,
                state: CanonicalState::Invalid,
                truncated: false,
            }],
            ..evidence()
        });
        assert_eq!(invalid.label, Label::ReviewCanonical);

        let conflict = html200(PageEvidence {
            canonicals: vec![
                canonical("/one", "https://example.com/one"),
                canonical("/two", "https://example.com/two"),
            ],
            ..evidence()
        });
        assert_eq!(conflict.label, Label::ReviewCanonical);
        assert!(
            conflict
                .findings
                .iter()
                .any(|finding| finding.code == FindingCode::CanonicalConflict)
        );

        let repeated = html200(PageEvidence {
            canonicals: vec![
                canonical("/other", "https://example.com/other"),
                canonical("/other", "https://example.com/other"),
            ],
            ..evidence()
        });
        assert_eq!(repeated.label, Label::CanonicalElsewhere);
        assert!(
            repeated
                .findings
                .iter()
                .any(|finding| finding.code == FindingCode::CanonicalRepeated)
        );
    }

    #[test]
    fn relative_and_query_canonicals_are_resolved_without_losing_the_query() {
        let base = Url::parse("https://example.com/docs/page?x=1").unwrap();
        let relative = canonical_declaration(Some("../target?keep=1"), &base);
        assert_eq!(relative.state, CanonicalState::Valid);
        assert_eq!(
            relative.resolved.as_deref(),
            Some("https://example.com/target?keep=1")
        );
        assert_eq!(
            canonical_declaration(Some(""), &base).state,
            CanonicalState::Empty
        );
        assert_eq!(
            canonical_declaration(None, &base).state,
            CanonicalState::Empty
        );
        assert_eq!(
            canonical_declaration(Some("javascript:void(0)"), &base).state,
            CanonicalState::Invalid
        );
    }

    #[test]
    fn oversized_evidence_is_bounded_without_losing_its_effective_signal() {
        let long_meta = format!("{} noindex", "index, ".repeat(100));
        let directive = parse_meta_directive("robots", &long_meta).unwrap();
        assert!(directive.truncated);
        assert!(directive.raw.chars().count() <= MAX_EVIDENCE_CHARS);
        assert!(directive.noindex);

        let mut long_href = String::from("/");
        long_href.push_str(&"a".repeat(MAX_EVIDENCE_CHARS * 2));
        let base = Url::parse(PAGE).unwrap();
        let declaration = canonical_declaration(Some(&long_href), &base);
        assert!(declaration.truncated);
        assert!(declaration.raw.chars().count() <= MAX_EVIDENCE_CHARS);
        assert!(declaration.resolved.as_ref().unwrap().chars().count() <= MAX_EVIDENCE_CHARS);
        assert!(declaration.matches_url(&format!("https://example.com{long_href}")));

        let assessment = html200(PageEvidence {
            directives: vec![directive],
            canonicals: vec![declaration],
            ..evidence()
        });
        assert_eq!(assessment.label, Label::Noindex);
        assert!(assessment.evidence.truncated);
        assert!(assessment.evidence_text().contains("truncated"));
        assert!(assessment.needs_attention());
    }

    #[test]
    fn a_long_self_canonical_stays_self_after_its_display_target_is_clipped() {
        let mut page = String::from("https://example.com/");
        page.push_str(&"long/".repeat(150));
        page.push_str("end");
        let base = Url::parse(&page).unwrap();
        let declaration = canonical_declaration(Some(&page), &base);
        assert!(declaration.truncated);
        assert!(declaration.matches_url(&page));

        let assessment = assess(
            &page,
            200,
            "text/html",
            "",
            "",
            PageEvidence {
                canonicals: vec![declaration],
                ..evidence()
            },
        );
        assert_eq!(assessment.label, Label::NotAssessed);
        assert_eq!(assessment.canonical_issue_text(), "");
        assert!(assessment.evidence.truncated);
    }

    #[test]
    fn generic_and_googlebot_noindex_precedence_holds_in_both_tag_orders() {
        for directives in [
            vec![meta(None, "noindex"), meta(Some("googlebot"), "index")],
            vec![meta(Some("googlebot"), "index"), meta(None, "noindex")],
        ] {
            let assessment = html200(PageEvidence {
                directives,
                ..evidence()
            });
            assert_eq!(assessment.label, Label::Noindex);
            assert!(assessment.has_noindex());
            assert!(assessment.has_confirmed_issue());
        }

        let googlebot_only = html200(PageEvidence {
            directives: vec![meta(Some("googlebot"), "noindex")],
            ..evidence()
        });
        assert_eq!(googlebot_only.label, Label::Noindex);

        let other_agent = html200(PageEvidence {
            directives: vec![meta(Some("bingbot"), "noindex")],
            ..evidence()
        });
        assert_eq!(other_agent.label, Label::NoBlockFound);
        assert!(!other_agent.has_noindex());
        assert!(
            other_agent
                .findings
                .iter()
                .any(|finding| finding.code == FindingCode::ScopedDirective)
        );
    }

    #[test]
    fn none_implies_noindex_and_nofollow_while_all_does_not_cancel_noindex() {
        let none = html200(PageEvidence {
            directives: vec![meta(None, "none")],
            ..evidence()
        });
        assert_eq!(none.label, Label::Noindex);
        assert!(
            none.findings
                .iter()
                .any(|finding| finding.code == FindingCode::Nofollow
                    || finding.code == FindingCode::Noindex)
        );

        let conflicting = html200(PageEvidence {
            directives: vec![meta(None, "noindex, all")],
            ..evidence()
        });
        assert_eq!(conflicting.label, Label::Noindex);

        let nofollow_only = html200(PageEvidence {
            directives: vec![meta(None, "nofollow")],
            ..evidence()
        });
        assert_eq!(nofollow_only.label, Label::NoBlockFound);
        assert!(!nofollow_only.has_confirmed_issue());
        assert!(
            nofollow_only
                .findings
                .iter()
                .any(|finding| finding.code == FindingCode::Nofollow)
        );
    }

    #[test]
    fn mixed_case_whitespace_and_index_conflicts_keep_restrictions() {
        let conflicting = html200(PageEvidence {
            directives: vec![meta(None, "INDEX ,  NoIndex ")],
            ..evidence()
        });
        assert_eq!(conflicting.label, Label::Noindex);
        assert!(conflicting.has_noindex());

        let later_index = html200(PageEvidence {
            directives: vec![meta(None, "NOINDEX"), meta(Some("googlebot"), "all, index")],
            ..evidence()
        });
        assert_eq!(later_index.label, Label::Noindex);

        let (parsed, _, _) = parse_x_robots_tag(&[" GoogleBot :  NOINDEX , NoFollow "]);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].agent.as_deref(), Some("GoogleBot"));
        assert!(parsed[0].noindex);
        assert!(parsed[1].nofollow && !parsed[1].noindex);
    }

    #[test]
    fn noindex_substrings_and_unrelated_directives_do_not_apply() {
        let substring = html200(PageEvidence {
            directives: vec![meta(None, "noindexifembedded")],
            ..evidence()
        });
        assert_eq!(substring.label, Label::NoBlockFound);
        assert!(!substring.has_noindex());

        let max_snippet = html200(PageEvidence {
            directives: vec![meta(None, "max-snippet: -1")],
            ..evidence()
        });
        assert_eq!(max_snippet.label, Label::NoBlockFound);
        assert!(!max_snippet.has_noindex());
    }

    #[test]
    fn x_robots_tag_scopes_agents_and_ignores_directive_colons() {
        let (generic, truncated, _) = parse_x_robots_tag(&["noindex, nofollow"]);
        assert!(!truncated);
        assert_eq!(generic.len(), 2);
        assert_eq!(generic[0].agent, None);
        assert!(generic[0].noindex);
        assert!(generic[1].nofollow);

        let (scoped, _, _) = parse_x_robots_tag(&["googlebot: noindex, nofollow"]);
        assert_eq!(scoped.len(), 2);
        assert_eq!(scoped[0].agent.as_deref(), Some("googlebot"));
        assert_eq!(scoped[1].agent.as_deref(), Some("googlebot"));
        assert!(
            scoped
                .iter()
                .all(|directive| directive.noindex || directive.nofollow)
        );

        let (other, _, _) = parse_x_robots_tag(&["bingbot: noindex"]);
        assert_eq!(other[0].agent.as_deref(), Some("bingbot"));
        assert!(!other[0].applies_to_google());

        let (value, _, _) = parse_x_robots_tag(&["unavailable_after: 25 Jun 2010 15:00:00 PST"]);
        assert_eq!(value[0].agent, None);
        assert!(!value[0].noindex);

        let (multiple, _, _) = parse_x_robots_tag(&["googlebot: noindex", "nofollow"]);
        assert_eq!(multiple[0].agent.as_deref(), Some("googlebot"));
        assert_eq!(multiple[1].agent, None);
    }

    #[test]
    fn non_html_200_is_not_assessed_unless_a_header_noindex_applies() {
        let plain = assess(
            "https://example.com/report.pdf",
            200,
            "application/pdf",
            "",
            "",
            evidence(),
        );
        assert_eq!(plain.label, Label::NotAssessed);
        // A successful non-HTML response is outside this HTML check, not an SEO defect.
        assert!(!plain.needs_attention());
        assert!(!plain.has_confirmed_issue());
        assert_eq!(plain.reason_text(), "Non-HTML content (application/pdf)");
        assert!(
            plain
                .findings
                .iter()
                .any(|finding| finding.code == FindingCode::NonHtml
                    && finding.severity == Severity::Info)
        );

        let blocked = assess(
            "https://example.com/report.pdf",
            200,
            "application/pdf",
            "",
            "",
            PageEvidence {
                directives: vec![RobotsDirective {
                    source: EvidenceSource::Header,
                    agent: None,
                    raw: "noindex".into(),
                    noindex: true,
                    nofollow: false,
                    truncated: false,
                }],
                ..evidence()
            },
        );
        assert_eq!(blocked.label, Label::Noindex);
        assert!(blocked.has_confirmed_issue());
    }

    #[test]
    fn response_states_have_explicit_labels() {
        let redirect = assess(
            "https://example.com/old",
            301,
            "text/html",
            "https://example.com/new",
            "",
            evidence(),
        );
        assert_eq!(redirect.label, Label::Redirect);
        assert!(redirect.reason_text().contains("Redirect to"));

        let no_target = assess(
            "https://example.com/old",
            302,
            "text/html",
            "",
            "",
            evidence(),
        );
        assert_eq!(no_target.label, Label::NotAssessed);
        assert!(no_target.reason_text().contains("302"));

        let not_modified = assess("https://example.com/page", 304, "", "", "", evidence());
        assert_eq!(not_modified.label, Label::NotAssessed);
        assert!(!not_modified.needs_attention());
        assert!(not_modified.reason_text().contains("304"));

        for status in [401, 403, 404, 410, 429, 503] {
            let assessment = assess(
                "https://example.com/page",
                status,
                "text/html",
                "",
                "",
                evidence(),
            );
            assert_eq!(assessment.label, Label::HttpError, "{status}");
            assert!(assessment.has_confirmed_issue(), "{status}");
            assert!(
                assessment.reason_text().contains(&status.to_string()),
                "{status}: {}",
                assessment.reason_text()
            );
        }

        let failed = assess(
            "https://example.com/page",
            200,
            "text/html",
            "",
            "",
            PageEvidence {
                read: ReadState::Failed,
                ..evidence()
            },
        );
        assert_eq!(failed.label, Label::FetchFailed);

        let truncated = assess(
            "https://example.com/page",
            200,
            "text/html",
            "",
            "",
            PageEvidence {
                read: ReadState::Truncated,
                ..evidence()
            },
        );
        assert_eq!(truncated.label, Label::NotAssessed);
        assert!(truncated.reason_text().contains("truncated"));

        let unread = assess(
            "https://example.com/page",
            200,
            "text/html",
            "",
            "",
            PageEvidence {
                read: ReadState::NotRead,
                ..evidence()
            },
        );
        assert_eq!(unread.label, Label::NotAssessed);
        assert!(unread.needs_attention());
        assert!(unread.reason_text().contains("was not read"));

        let capped = html200(PageEvidence {
            directives: (0..MAX_EVIDENCE_ITEMS + 1)
                .map(|_| meta(None, "noindex"))
                .collect(),
            truncated: true,
            ..evidence()
        });
        assert_eq!(capped.label, Label::Noindex);
        assert!(
            capped
                .findings
                .iter()
                .any(|finding| finding.code == FindingCode::EvidenceTruncated)
        );
    }

    #[test]
    fn evidence_cap_keeps_applicable_restrictions_from_omitted_entries() {
        let index = meta(None, "index");
        let noindex_header = RobotsDirective {
            source: EvidenceSource::Header,
            agent: None,
            raw: "noindex".into(),
            noindex: true,
            nofollow: false,
            truncated: false,
        };

        // Sixteen benign meta tags followed by a blocking header: the header is past the
        // retained cap, but its effective restriction must still win.
        let mut directives: Vec<RobotsDirective> =
            (0..MAX_EVIDENCE_ITEMS).map(|_| index.clone()).collect();
        directives.push(noindex_header.clone());
        let meta_first = html200(PageEvidence {
            directives,
            ..evidence()
        });
        assert_eq!(meta_first.label, Label::Noindex);
        assert!(meta_first.has_noindex());
        assert!(meta_first.has_confirmed_issue());
        assert!(meta_first.needs_attention());
        assert!(meta_first.evidence.truncated);
        assert!(meta_first.evidence.omitted.applicable_noindex);
        assert!(
            meta_first
                .evidence_text()
                .contains("among directives not retained")
        );

        // Header first keeps the raw declaration; the omitted meta tail is benign.
        let mut directives = vec![noindex_header];
        directives.extend((0..MAX_EVIDENCE_ITEMS).map(|_| index.clone()));
        let header_first = html200(PageEvidence {
            directives,
            ..evidence()
        });
        assert_eq!(header_first.label, Label::Noindex);
        assert!(header_first.has_noindex());
        assert!(header_first.evidence.truncated);
        assert_eq!(header_first.evidence.omitted, OmittedEvidence::default());

        // A same-source overflow folds a late noindex into the aggregate even though its
        // raw declaration is not retained.
        let mut values: Vec<String> = (0..MAX_EVIDENCE_ITEMS)
            .map(|_| "index".to_string())
            .collect();
        values.push("noindex".to_string());
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let (parsed, truncated, omitted) = parse_x_robots_tag(&refs);
        assert_eq!(parsed.len(), MAX_EVIDENCE_ITEMS);
        assert!(truncated);
        assert!(omitted.applicable_noindex);
        let overflow = html200(PageEvidence {
            directives: parsed,
            truncated,
            omitted,
            ..evidence()
        });
        assert_eq!(overflow.label, Label::Noindex);
        assert!(overflow.has_noindex());
    }

    #[test]
    fn reason_and_evidence_text_are_deterministic() {
        let assessment = assess(
            PAGE,
            200,
            "text/html",
            "",
            "",
            PageEvidence {
                directives: vec![
                    meta(None, "noindex"),
                    RobotsDirective {
                        source: EvidenceSource::Header,
                        agent: Some("googlebot".into()),
                        raw: "noindex".into(),
                        noindex: true,
                        nofollow: false,
                        truncated: false,
                    },
                ],
                canonicals: vec![canonical("/other", "https://example.com/other")],
                ..evidence()
            },
        );
        assert_eq!(
            assessment.reason_text(),
            "Meta robots: noindex · X-Robots-Tag (Googlebot): noindex · Canonical points to /other"
        );
        assert!(
            assessment
                .evidence_text()
                .contains("canonical /other → https://example.com/other")
        );
    }
}
