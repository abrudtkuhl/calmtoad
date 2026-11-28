//! The Overview's cached facts and its ranked review.
//!
//! Everything here is computed by [`build`] from the retained crawl, only when the throttled
//! count refresh runs. The view reads the model; it never rescans pages per frame.

use super::*;

/// Cached counts for the default Overview. Group counts overlap by design and are never summed
/// into a unique-page claim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OverviewCounts {
    pub pages: usize,
    pub response: usize,
    pub indexability: usize,
    pub titles: usize,
    pub descriptions: usize,
    pub headings: usize,
    pub images: usize,
    pub links: usize,
    pub incomplete: usize,
    pub sitemap_incomplete: usize,
    pub sitemap_file_findings: usize,
    pub link_findings: usize,
    pub link_incomplete: usize,
    pub content_same_urls: usize,
    pub content_similar_urls: usize,
    pub structured_urls: usize,
    pub structured_incomplete: usize,
    pub accessibility_urls: usize,
    pub accessibility_review_urls: usize,
    pub accessibility_incomplete: usize,

    // Eligible populations and observed subtypes, so the review can state a truthful
    // denominator and name what is actually wrong.
    pub response_eligible: usize,
    pub indexing_eligible: usize,
    pub titles_eligible: usize,
    pub descriptions_eligible: usize,
    pub headings_eligible: usize,
    pub images_eligible: usize,
    pub links_eligible: usize,
    pub content_partial: usize,
    pub indexability_partial: usize,
    pub response_homepage: usize,
    pub response_server: usize,
    pub response_client: usize,
    pub response_fetch: usize,
    pub response_redirect: usize,
    pub title_missing: usize,
    pub title_duplicate: usize,
    pub title_too_long: usize,
    pub title_too_short: usize,
    pub description_missing: usize,
    pub description_duplicate: usize,
    pub description_too_long: usize,
    pub description_too_short: usize,
    pub heading_missing: usize,
    pub heading_multiple: usize,
    pub heading_duplicate: usize,
    pub images_missing_alt_total: usize,
    pub noindex: usize,
    pub canonical_elsewhere: usize,
    pub canonical_review: usize,
    pub sitemap_total: usize,
    pub sitemap_checked: usize,
    pub sitemap_not_reached: usize,
    pub sitemap_blocked: usize,
    pub sitemap_skipped: usize,
    pub sitemap_checked_incomplete: usize,
    pub sitemap_url_findings: usize,
    pub sitemap_file_total: usize,
    pub structured_eligible: usize,
    pub accessibility_eligible: usize,
    pub content_exact_eligible: usize,
    pub content_near_eligible: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OverviewGroup {
    Response,
    Indexability,
    Titles,
    Descriptions,
    Headings,
    Images,
    Links,
}

impl OverviewGroup {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Response => "overview-response",
            Self::Indexability => "overview-indexability",
            Self::Titles => "overview-titles",
            Self::Descriptions => "overview-descriptions",
            Self::Headings => "overview-headings",
            Self::Images => "overview-images",
            Self::Links => "overview-links",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Response => "Response and fetch",
            Self::Indexability => "Indexing and canonical",
            Self::Titles => "Titles",
            Self::Descriptions => "Meta descriptions",
            Self::Headings => "H1",
            Self::Images => "Image alt",
            Self::Links => "Broken links",
        }
    }

    pub(super) fn help(self) -> &'static str {
        match self {
            Self::Response => "HTTP errors, redirects, and pages that could not be fetched",
            Self::Indexability => "Noindex and canonical findings, with capture gaps kept separate",
            Self::Titles => "Missing, long, short, or duplicate page titles",
            Self::Descriptions => "Missing, long, short, or duplicate meta descriptions",
            Self::Headings => "Missing, multiple, or duplicate H1 headings",
            Self::Images => "Images without alternative text on completed captures",
            Self::Links => "Same-host links whose target failed in this crawl",
        }
    }

    pub(super) fn target(self) -> (Tab, CheckFilter) {
        match self {
            Self::Response => (Tab::Response, CheckFilter::Findings),
            Self::Indexability => (Tab::Internal, CheckFilter::Findings),
            Self::Titles => (Tab::Titles, CheckFilter::Findings),
            Self::Descriptions => (Tab::Meta, CheckFilter::Findings),
            Self::Headings => (Tab::H1, CheckFilter::Findings),
            Self::Images => (Tab::Images, CheckFilter::Findings),
            Self::Links => (Tab::Links, CheckFilter::Findings),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Unit {
    Pages,
    Files,
    Occurrences,
}

impl Unit {
    pub(super) fn label(self, count: usize) -> &'static str {
        match (self, count == 1) {
            (Self::Pages, true) => "page",
            (Self::Pages, false) => "pages",
            (Self::Files, true) => "file",
            (Self::Files, false) => "files",
            (Self::Occurrences, true) => "occurrence",
            (Self::Occurrences, false) => "occurrences",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CheckStatus {
    Findings,
    Clean,
    Partial,
    NotRun,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Signal {
    Red,
    Amber,
    Muted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReviewKind {
    HomepageError,
    ServerError,
    FetchFailure,
    ClientError,
    SitemapFile,
    Noindex,
    Accessibility,
    CanonicalElsewhere,
    CanonicalReview,
    MissingTitle,
    DuplicateTitle,
    MissingMeta,
    DuplicateMeta,
    BrokenLinks,
    MissingH1,
    MultipleH1,
    DuplicateH1,
    Structured,
    ContentSame,
    ContentSimilar,
    LinkFinding,
    TitleTooLong,
    TitleTooShort,
    MetaTooLong,
    MetaTooShort,
    MissingAlt,
    Redirect,
}

impl ReviewKind {
    /// Deterministic impact order. Homepage errors rank first, then other server/fetch/client
    /// failures, sitemap file errors, indexing blocks and accessibility violations; title/meta
    /// findings and lower-confidence advisories fill the remaining slots. Ties break by
    /// affected count, then kind, then title, never by page order.
    pub(super) fn priority(self) -> u8 {
        match self {
            Self::HomepageError => 100,
            Self::ServerError => 95,
            Self::FetchFailure => 93,
            Self::ClientError => 90,
            Self::SitemapFile => 85,
            Self::Noindex => 80,
            Self::Accessibility => 75,
            Self::CanonicalElsewhere => 70,
            Self::CanonicalReview => 68,
            Self::MissingTitle => 65,
            Self::DuplicateTitle => 60,
            Self::MissingMeta => 55,
            Self::DuplicateMeta => 50,
            Self::BrokenLinks => 45,
            Self::MissingH1 => 42,
            Self::MultipleH1 => 40,
            Self::DuplicateH1 => 38,
            Self::Structured => 35,
            Self::ContentSame => 33,
            Self::ContentSimilar => 32,
            Self::LinkFinding => 30,
            Self::TitleTooLong => 25,
            Self::TitleTooShort => 24,
            Self::MetaTooLong => 22,
            Self::MetaTooShort => 21,
            Self::MissingAlt => 20,
            Self::Redirect => 18,
        }
    }

    fn order(self) -> u8 {
        self as u8
    }

    pub(super) fn signal(self) -> Signal {
        match self {
            Self::HomepageError
            | Self::ServerError
            | Self::FetchFailure
            | Self::ClientError
            | Self::Noindex
            | Self::Accessibility => Signal::Red,
            Self::SitemapFile
            | Self::CanonicalElsewhere
            | Self::CanonicalReview
            | Self::MissingTitle
            | Self::DuplicateTitle
            | Self::MissingMeta
            | Self::DuplicateMeta
            | Self::BrokenLinks
            | Self::MissingH1
            | Self::MultipleH1
            | Self::DuplicateH1
            | Self::Structured
            | Self::ContentSame
            | Self::ContentSimilar
            | Self::LinkFinding
            | Self::TitleTooLong
            | Self::TitleTooShort
            | Self::MetaTooLong
            | Self::MetaTooShort
            | Self::MissingAlt
            | Self::Redirect => Signal::Amber,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReviewTarget {
    Response,
    InternalFindings,
    InternalIncomplete,
    Titles,
    Meta,
    H1,
    Images,
    Links,
    SitemapFilesFindings,
    SitemapUrlsIncomplete,
    SitemapUrlsFindings,
    SitemapUrlsAll,
    StructuredFindings,
    StructuredIncomplete,
    AccessibilityFindings,
    AccessibilityIncomplete,
    DuplicateSame,
    DuplicateSimilar,
    DuplicateNotCompared,
    LinkEvidenceFindings,
    LinkEvidenceIncomplete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReviewCandidate {
    pub kind: ReviewKind,
    pub title: String,
    pub why: &'static str,
    pub affected: usize,
    pub eligible: Option<usize>,
    pub unit: Unit,
    pub sample: Option<String>,
    pub signal: Signal,
    pub target: ReviewTarget,
    pub query: Option<&'static str>,
    pub priority: u8,
}

impl ReviewCandidate {
    pub(super) fn count_text(&self) -> String {
        // A confirmed finding can coexist with incomplete coverage, so a small complete
        // denominator must never be printed below a larger affected count.
        match self
            .eligible
            .filter(|eligible| *eligible > 0 && *eligible >= self.affected)
        {
            Some(eligible) => format!(
                "{} of {} {}",
                self.affected,
                eligible,
                self.unit.label(eligible)
            ),
            None => format!("{} {}", self.affected, self.unit.label(self.affected)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CheckRow {
    pub group: OverviewGroup,
    pub status: CheckStatus,
    pub affected: usize,
    pub eligible: usize,
    pub partial: usize,
    pub unit: Unit,
    pub note: &'static str,
}

impl CheckRow {
    pub(super) fn count_text(&self) -> String {
        if self.affected > 0 && self.eligible >= self.affected {
            format!(
                "{} of {} {}",
                self.affected,
                self.eligible,
                self.unit.label(self.eligible)
            )
        } else if self.affected > 0 {
            format!("{} {}", self.affected, self.unit.label(self.affected))
        } else if self.partial > 0 {
            format!(
                "{} {} unchecked",
                self.partial,
                self.unit.label(self.partial)
            )
        } else if self.eligible > 0 {
            format!("{} {}", self.eligible, self.unit.label(self.eligible))
        } else {
            String::new()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OptionalRow {
    pub key: &'static str,
    pub label: &'static str,
    pub status: CheckStatus,
    pub affected: usize,
    pub eligible: usize,
    pub partial: usize,
    pub unit: Unit,
    pub note: String,
    pub target: Option<ReviewTarget>,
}

impl OptionalRow {
    pub(super) fn count_text(&self) -> String {
        if self.affected > 0 && self.eligible >= self.affected && self.eligible > 0 {
            format!(
                "{} of {} {}",
                self.affected,
                self.eligible,
                self.unit.label(self.eligible)
            )
        } else if self.affected > 0 {
            format!("{} {}", self.affected, self.unit.label(self.affected))
        } else if self.partial > 0 {
            format!(
                "{} {} unchecked",
                self.partial,
                self.unit.label(self.partial)
            )
        } else if self.eligible > 0 {
            format!("{} {}", self.eligible, self.unit.label(self.eligible))
        } else {
            String::new()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CoverageRow {
    pub key: &'static str,
    pub label: String,
    pub reason: String,
    pub target: Option<ReviewTarget>,
    pub adjust_limit: bool,
    pub signal: Signal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct OverviewModel {
    /// Every ranked finding, sorted by the deterministic priority below. The view shows the
    /// first three; the rest stay available for tests and future expansion.
    pub review: Vec<ReviewCandidate>,
    pub hidden_review: usize,
    pub checks: Vec<CheckRow>,
    pub optional: Vec<OptionalRow>,
    pub coverage: Vec<CoverageRow>,
    pub clean_checks: usize,
    pub finding_checks: usize,
    pub partial_checks: usize,
    pub not_run_checks: usize,
    pub coverage_complete: bool,
}

#[derive(Default)]
struct Tally {
    affected: usize,
    sample: Option<String>,
}

impl Tally {
    fn add(&mut self, page: &Page) {
        if self.affected == 0 {
            self.sample = Some(page.url.clone());
        }
        self.affected += 1;
    }
}

#[derive(Default)]
struct FieldTally {
    missing: Tally,
    duplicate: Tally,
    too_long: Tally,
    too_short: Tally,
}

impl FieldTally {
    fn add(&mut self, page: &Page, kind: FieldIssueKind) {
        match kind {
            FieldIssueKind::Missing => self.missing.add(page),
            FieldIssueKind::Duplicate
            | FieldIssueKind::DuplicateTooLong
            | FieldIssueKind::DuplicateTooShort => self.duplicate.add(page),
            FieldIssueKind::TooLong => self.too_long.add(page),
            FieldIssueKind::TooShort => self.too_short.add(page),
        }
    }

    fn note(&self) -> &'static str {
        let counts = [
            (self.missing.affected, "Missing"),
            (self.duplicate.affected, "Duplicate"),
            (self.too_long.affected, "Too long"),
            (self.too_short.affected, "Too short"),
        ];
        counts
            .into_iter()
            .max_by_key(|(count, _)| *count)
            .filter(|(count, _)| *count > 0)
            .map_or("", |(_, label)| label)
    }
}

#[derive(Default)]
struct H1Tally {
    missing: Tally,
    multiple: Tally,
    duplicate: Tally,
}

impl H1Tally {
    fn add(&mut self, page: &Page, kind: H1IssueKind) {
        match kind {
            H1IssueKind::Missing => self.missing.add(page),
            H1IssueKind::Multiple => self.multiple.add(page),
            H1IssueKind::Duplicate => self.duplicate.add(page),
        }
    }

    fn note(&self) -> &'static str {
        let counts = [
            (self.missing.affected, "Missing"),
            (self.multiple.affected, "Multiple"),
            (self.duplicate.affected, "Duplicate"),
        ];
        counts
            .into_iter()
            .max_by_key(|(count, _)| *count)
            .filter(|(count, _)| *count > 0)
            .map_or("", |(_, label)| label)
    }
}

/// Build the Overview counts and its ranked model in one pass over the retained crawl.
pub(super) fn build(
    state: &State,
    accessibility_enabled: bool,
) -> (Counts, OverviewCounts, OverviewModel) {
    let counts = counts_with(&state.pages, &state.audit);
    let seed = state
        .run
        .as_ref()
        .map(|run| run.seed.as_str())
        .unwrap_or("");
    let mut oc = OverviewCounts {
        pages: counts.pages,
        response: counts.response,
        titles: counts.title,
        descriptions: counts.meta,
        headings: counts.h1,
        images: counts.images,
        links: counts.links,
        response_eligible: counts.pages,
        indexing_eligible: counts.pages,
        ..OverviewCounts::default()
    };

    let mut title = FieldTally::default();
    let mut description = FieldTally::default();
    let mut heading = H1Tally::default();
    let mut server = Tally::default();
    let mut client = Tally::default();
    let mut fetch = Tally::default();
    let mut redirect = Tally::default();
    let mut homepage = Tally::default();
    let mut noindex = Tally::default();
    let mut canonical_elsewhere = Tally::default();
    let mut canonical_review = Tally::default();
    let mut images = Tally::default();
    let mut links = Tally::default();
    let mut auditable_count = 0_usize;
    let mut pages_with_images = 0_usize;
    let mut content_partial = 0_usize;
    let mut structured_eligible = 0_usize;
    let mut buffer = String::new();

    for page in &state.pages {
        if !(200..=299).contains(&page.status) {
            if page.url == seed && !(300..=399).contains(&page.status) {
                homepage.add(page);
                oc.response_homepage += 1;
            } else {
                match page.status {
                    0 => {
                        fetch.add(page);
                        oc.response_fetch += 1;
                    }
                    300..=399 => {
                        redirect.add(page);
                        oc.response_redirect += 1;
                    }
                    400..=499 => {
                        client.add(page);
                        oc.response_client += 1;
                    }
                    500..=599 => {
                        server.add(page);
                        oc.response_server += 1;
                    }
                    _ => {}
                }
            }
        }

        match page.indexability.label {
            Label::Noindex => {
                noindex.add(page);
                oc.noindex += 1;
            }
            Label::CanonicalElsewhere => {
                canonical_elsewhere.add(page);
                oc.canonical_elsewhere += 1;
            }
            Label::ReviewCanonical => {
                canonical_review.add(page);
                oc.canonical_review += 1;
            }
            _ => {}
        }

        if auditable(page) {
            auditable_count += 1;
            if page.images > 0 {
                pages_with_images += 1;
            }
            if page.images_missing_alt > 0 {
                images.add(page);
                oc.images_missing_alt_total += page.images_missing_alt;
            }
            if state.audit.has_broken_links(page) {
                links.add(page);
            }
            if let Some(kind) = title_issue_kind(page, &state.audit, &mut buffer) {
                title.add(page, kind);
            }
            if let Some(kind) = meta_issue_kind(page, &state.audit, &mut buffer) {
                description.add(page, kind);
            }
            if let Some(kind) = h1_issue_kind(page, &state.audit, &mut buffer) {
                heading.add(page, kind);
            }
            if !page.structured.is_incomplete() {
                structured_eligible += 1;
            }
        } else if page.status == 200
            && page.content_type.to_ascii_lowercase().contains("html")
            && page_incomplete(page)
        {
            content_partial += 1;
        }
    }

    // Indexing and canonical is its own family; response findings stay in the Response row and
    // the Review's response subtypes instead of being counted twice here.
    oc.indexability = oc.noindex + oc.canonical_elsewhere + oc.canonical_review;
    oc.titles_eligible = auditable_count;
    oc.descriptions_eligible = auditable_count;
    oc.headings_eligible = auditable_count;
    oc.links_eligible = auditable_count;
    oc.images_eligible = pages_with_images;
    oc.title_missing = title.missing.affected;
    oc.title_duplicate = title.duplicate.affected;
    oc.title_too_long = title.too_long.affected;
    oc.title_too_short = title.too_short.affected;
    oc.description_missing = description.missing.affected;
    oc.description_duplicate = description.duplicate.affected;
    oc.description_too_long = description.too_long.affected;
    oc.description_too_short = description.too_short.affected;
    oc.heading_missing = heading.missing.affected;
    oc.heading_multiple = heading.multiple.affected;
    oc.heading_duplicate = heading.duplicate.affected;
    oc.content_partial = content_partial;
    oc.indexability_partial = content_partial;
    oc.structured_eligible = structured_eligible;
    oc.incomplete = state
        .pages
        .iter()
        .filter(|page| page_incomplete(page))
        .count();
    oc.sitemap_incomplete = sitemap_incomplete(&state.sitemap);
    oc.sitemap_file_findings = sitemap_file_findings(&state.sitemap);
    oc.sitemap_file_total = state
        .sitemap
        .files
        .iter()
        .filter(|file| file.state != FileState::Absent)
        .count();
    for entry in &state.sitemap.urls {
        oc.sitemap_total += 1;
        match entry.state {
            UrlState::Checked => {
                oc.sitemap_checked += 1;
                if sitemap_url_has_finding(entry) {
                    oc.sitemap_url_findings += 1;
                }
                if sitemap_url_incomplete(entry) {
                    oc.sitemap_checked_incomplete += 1;
                }
            }
            UrlState::NotChecked => oc.sitemap_not_reached += 1,
            UrlState::Blocked => oc.sitemap_blocked += 1,
            UrlState::Skipped => oc.sitemap_skipped += 1,
            UrlState::Pending => {}
        }
    }
    for page in &state.pages {
        for occurrence in page.link_evidence.iter() {
            let check = state
                .link_checks
                .get(&occurrence.key)
                .or(occurrence.check.as_deref());
            if check.is_some_and(LinkCheckResult::is_finding) {
                oc.link_findings += 1;
            }
            if check.is_none_or(|check| check.state.is_incomplete()) {
                oc.link_incomplete += 1;
            }
        }
    }
    if accessibility_enabled {
        oc.accessibility_eligible = state
            .pages
            .iter()
            .filter(|page| !page.accessibility.has_coverage_gap())
            .count();
        oc.accessibility_urls = state
            .pages
            .iter()
            .filter(|page| page.accessibility.is_finding())
            .count();
        oc.accessibility_review_urls = state
            .pages
            .iter()
            .filter(|page| page.accessibility.needs_review_occurrences() > 0)
            .count();
        oc.accessibility_incomplete = state
            .pages
            .iter()
            .filter(|page| page.accessibility.has_coverage_gap())
            .count();
    }
    if let Some(analysis) = state.content_analysis.as_deref() {
        oc.content_same_urls = analysis.same_text_affected();
        oc.content_similar_urls = analysis.similar_affected();
        oc.content_exact_eligible = analysis.coverage.exact_eligible;
        oc.content_near_eligible = analysis.coverage.near_eligible;
    }
    oc.structured_urls = state
        .pages
        .iter()
        .filter(|page| page.structured.has_confirmed_finding())
        .count();
    oc.structured_incomplete = state
        .pages
        .iter()
        .filter(|page| page.structured.is_incomplete())
        .count();

    let model = model_from(
        state,
        &oc,
        counts,
        &title,
        &description,
        &heading,
        &server,
        &client,
        &fetch,
        &redirect,
        &homepage,
        &noindex,
        &canonical_elsewhere,
        &canonical_review,
        &images,
        &links,
        content_partial,
        accessibility_enabled,
    );
    (counts, oc, model)
}

#[allow(clippy::too_many_arguments)]
fn model_from(
    state: &State,
    oc: &OverviewCounts,
    counts: Counts,
    title: &FieldTally,
    description: &FieldTally,
    heading: &H1Tally,
    server: &Tally,
    client: &Tally,
    fetch: &Tally,
    redirect: &Tally,
    homepage: &Tally,
    noindex: &Tally,
    canonical_elsewhere: &Tally,
    canonical_review: &Tally,
    images: &Tally,
    links: &Tally,
    content_partial: usize,
    accessibility_enabled: bool,
) -> OverviewModel {
    let mut review = Vec::new();
    let pages_eligible = counts.pages;
    let auditable_eligible = oc.titles_eligible;

    if let Some(sample) = homepage.sample.as_deref() {
        let status = state
            .pages
            .iter()
            .find(|page| page.url == sample)
            .map_or(0, |page| page.status);
        let (name, why) = if status == 0 {
            (
                "Homepage did not respond".to_string(),
                "The homepage is the first page visitors and search engines see; an error there costs the whole site.",
            )
        } else {
            (
                format!("Homepage returned a {status}"),
                "The homepage is the first page visitors and search engines see; an error there costs the whole site.",
            )
        };
        review.push(candidate(
            ReviewKind::HomepageError,
            name,
            why,
            1,
            Some(pages_eligible),
            Unit::Pages,
            Some(sample.to_string()),
            ReviewTarget::Response,
            None,
        ));
    }
    push_pages(
        &mut review,
        ReviewKind::ServerError,
        "page returned a server error",
        "pages returned server errors",
        "A server error page cannot be indexed or reached.",
        server,
        pages_eligible,
        ReviewTarget::Response,
        Some("Server error"),
    );
    push_pages(
        &mut review,
        ReviewKind::FetchFailure,
        "page did not respond",
        "pages did not respond",
        "The page did not respond, so its content could not be checked.",
        fetch,
        pages_eligible,
        ReviewTarget::Response,
        None,
    );
    push_pages(
        &mut review,
        ReviewKind::ClientError,
        "page returned a client error",
        "pages returned client errors",
        "The URL is broken, so visitors and crawlers hit a dead end.",
        client,
        pages_eligible,
        ReviewTarget::Response,
        Some("Client error"),
    );
    if oc.sitemap_file_findings > 0 {
        review.push(candidate(
            ReviewKind::SitemapFile,
            format!(
                "{} sitemap {} attention",
                oc.sitemap_file_findings,
                if oc.sitemap_file_findings == 1 {
                    "file needs"
                } else {
                    "files need"
                }
            ),
            "A sitemap file error can hide every URL it lists.",
            oc.sitemap_file_findings,
            Some(oc.sitemap_file_total),
            Unit::Files,
            state
                .sitemap
                .files
                .iter()
                .find(|file| sitemap_file_has_finding(file))
                .map(|file| file.url.clone()),
            ReviewTarget::SitemapFilesFindings,
            None,
        ));
    }
    push_pages(
        &mut review,
        ReviewKind::Noindex,
        "page is set to noindex",
        "pages are set to noindex",
        "The page can appear in results, but search engines are told not to index it.",
        noindex,
        pages_eligible,
        ReviewTarget::InternalFindings,
        Some("Noindex"),
    );
    if accessibility_enabled && oc.accessibility_urls > 0 {
        review.push(candidate(
            ReviewKind::Accessibility,
            if oc.accessibility_urls == 1 {
                "1 page has an automated accessibility violation".to_string()
            } else {
                format!(
                    "{} pages have automated accessibility violations",
                    oc.accessibility_urls
                )
            },
            "An axe rule found a barrier that can block keyboard or screen-reader users.",
            oc.accessibility_urls,
            Some(oc.accessibility_eligible),
            Unit::Pages,
            state
                .pages
                .iter()
                .find(|page| page.accessibility.is_finding())
                .map(|page| page.url.clone()),
            ReviewTarget::AccessibilityFindings,
            None,
        ));
    }
    push_pages(
        &mut review,
        ReviewKind::CanonicalElsewhere,
        "page canonicalizes to another URL",
        "pages canonicalize to another URL",
        "Search engines may credit another URL for this page's content.",
        canonical_elsewhere,
        pages_eligible,
        ReviewTarget::InternalFindings,
        Some("Canonical elsewhere"),
    );
    push_pages(
        &mut review,
        ReviewKind::CanonicalReview,
        "page has a canonical to review",
        "pages have a canonical to review",
        "Conflicting or invalid canonical evidence needs a person to decide.",
        canonical_review,
        pages_eligible,
        ReviewTarget::InternalFindings,
        Some("Review canonical"),
    );
    push_field(
        &mut review,
        ReviewKind::MissingTitle,
        "page has no title",
        "pages have no title",
        "Search results show a title the crawler could not read; the page loses its best relevance signal.",
        &title.missing,
        auditable_eligible,
        ReviewTarget::Titles,
        Some("Missing"),
    );
    push_field(
        &mut review,
        ReviewKind::DuplicateTitle,
        "page shares a title",
        "pages share a title",
        "Repeat titles make pages compete for the same query and blur which page should rank.",
        &title.duplicate,
        auditable_eligible,
        ReviewTarget::Titles,
        Some("Duplicate"),
    );
    push_field(
        &mut review,
        ReviewKind::MissingMeta,
        "page has no meta description",
        "pages have no meta description",
        "Search engines write their own snippet, so the page loses control of its click-through copy.",
        &description.missing,
        auditable_eligible,
        ReviewTarget::Meta,
        Some("Missing"),
    );
    push_field(
        &mut review,
        ReviewKind::DuplicateMeta,
        "page shares a meta description",
        "pages share a meta description",
        "Repeated descriptions make results look interchangeable and weaken the click.",
        &description.duplicate,
        auditable_eligible,
        ReviewTarget::Meta,
        Some("Duplicate"),
    );
    push_field(
        &mut review,
        ReviewKind::MissingH1,
        "page has no H1",
        "pages have no H1",
        "The page does not state its main topic in a heading.",
        &heading.missing,
        auditable_eligible,
        ReviewTarget::H1,
        Some("Missing"),
    );
    push_field(
        &mut review,
        ReviewKind::MultipleH1,
        "page has multiple H1s",
        "pages have multiple H1s",
        "Competing top-level headings make the page outline ambiguous.",
        &heading.multiple,
        auditable_eligible,
        ReviewTarget::H1,
        Some("Multiple"),
    );
    push_field(
        &mut review,
        ReviewKind::DuplicateH1,
        "page shares an H1",
        "pages share an H1",
        "Repeated top-level headings make pages harder to tell apart.",
        &heading.duplicate,
        auditable_eligible,
        ReviewTarget::H1,
        Some("Duplicate"),
    );
    push_pages(
        &mut review,
        ReviewKind::BrokenLinks,
        "page contains broken internal links",
        "pages contain broken internal links",
        "A broken same-host link wastes crawl paths and sends visitors to a dead end.",
        links,
        auditable_eligible,
        ReviewTarget::Links,
        None,
    );
    if oc.structured_urls > 0 {
        review.push(candidate(
            ReviewKind::Structured,
            format!(
                "{} {} a JSON-LD syntax or duplicate-key finding",
                oc.structured_urls,
                if oc.structured_urls == 1 {
                    "page has"
                } else {
                    "pages have"
                }
            ),
            "Malformed JSON-LD cannot be parsed, so structured-data consumers ignore it.",
            oc.structured_urls,
            Some(oc.structured_eligible),
            Unit::Pages,
            state
                .pages
                .iter()
                .find(|page| page.structured.has_confirmed_finding())
                .map(|page| page.url.clone()),
            ReviewTarget::StructuredFindings,
            None,
        ));
    }
    if oc.content_same_urls > 0 {
        review.push(candidate(
            ReviewKind::ContentSame,
            if oc.content_same_urls == 1 {
                "1 URL shares the same text".to_string()
            } else {
                format!("{} URLs share the same text", oc.content_same_urls)
            },
            "Pages that repeat the same text compete with each other in search.",
            oc.content_same_urls,
            Some(oc.content_exact_eligible),
            Unit::Pages,
            None,
            ReviewTarget::DuplicateSame,
            None,
        ));
    }
    if oc.content_similar_urls > 0 {
        review.push(candidate(
            ReviewKind::ContentSimilar,
            if oc.content_similar_urls == 1 {
                "1 URL has similar content".to_string()
            } else {
                format!("{} URLs have similar content", oc.content_similar_urls)
            },
            "Near-duplicate pages split ranking signals across interchangeable URLs.",
            oc.content_similar_urls,
            Some(oc.content_near_eligible),
            Unit::Pages,
            None,
            ReviewTarget::DuplicateSimilar,
            None,
        ));
    }
    if state.run.as_ref().is_some_and(|run| run.check_links) && oc.link_findings > 0 {
        review.push(candidate(
            ReviewKind::LinkFinding,
            format!(
                "{} link {} returned a finding",
                oc.link_findings,
                if oc.link_findings == 1 {
                    "occurrence"
                } else {
                    "occurrences"
                }
            ),
            "A checked destination returned an error; the anchor leads somewhere broken.",
            oc.link_findings,
            None,
            Unit::Occurrences,
            None,
            ReviewTarget::LinkEvidenceFindings,
            None,
        ));
    }
    push_field(
        &mut review,
        ReviewKind::TitleTooLong,
        "title is too long",
        "titles are too long",
        "Search results truncate a title that runs past the tool's 60-character range.",
        &title.too_long,
        auditable_eligible,
        ReviewTarget::Titles,
        Some("Too long"),
    );
    push_field(
        &mut review,
        ReviewKind::TitleTooShort,
        "title is too short",
        "titles are too short",
        "A short title leaves relevance and click-through on the table.",
        &title.too_short,
        auditable_eligible,
        ReviewTarget::Titles,
        Some("Too short"),
    );
    push_field(
        &mut review,
        ReviewKind::MetaTooLong,
        "meta description is too long",
        "meta descriptions are too long",
        "Search results cut off a description that runs past the tool's 160-character range.",
        &description.too_long,
        auditable_eligible,
        ReviewTarget::Meta,
        Some("Too long"),
    );
    push_field(
        &mut review,
        ReviewKind::MetaTooShort,
        "meta description is too short",
        "meta descriptions are too short",
        "A short description gives the search engine less to work with.",
        &description.too_short,
        auditable_eligible,
        ReviewTarget::Meta,
        Some("Too short"),
    );
    push_field(
        &mut review,
        ReviewKind::MissingAlt,
        "page has images without alt text",
        "pages have images without alt text",
        "Images without alt text are invisible to screen readers and image search.",
        images,
        oc.images_eligible,
        ReviewTarget::Images,
        None,
    );
    push_field(
        &mut review,
        ReviewKind::Redirect,
        "page redirects",
        "pages redirect",
        "A redirect adds a hop; the destination should be linked directly.",
        redirect,
        pages_eligible,
        ReviewTarget::Response,
        Some("Redirect"),
    );

    review.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| right.affected.cmp(&left.affected))
            .then_with(|| left.kind.order().cmp(&right.kind.order()))
            .then_with(|| left.title.cmp(&right.title))
    });

    let checks = check_rows(
        oc,
        counts,
        title,
        description,
        heading,
        server,
        client,
        fetch,
        homepage,
        noindex,
        canonical_elsewhere,
        canonical_review,
        images,
        links,
        content_partial,
    );
    let mut clean_checks = 0;
    let mut finding_checks = 0;
    let mut partial_checks = 0;
    let mut not_run_checks = 0;
    for row in &checks {
        match row.status {
            CheckStatus::Findings => finding_checks += 1,
            CheckStatus::Clean => clean_checks += 1,
            CheckStatus::Partial => partial_checks += 1,
            CheckStatus::NotRun => not_run_checks += 1,
        }
    }

    let optional = optional_rows(state, oc, accessibility_enabled);
    let mut coverage = coverage_rows(state, oc, content_partial);
    coverage.retain(|row| !row.label.is_empty());
    let coverage_complete = coverage.is_empty()
        && !state.phase.active()
        && counts.pages > 0
        && state.run.is_some()
        && state.sitemap.discovery_done
        && !matches!(
            state.content_phase,
            ContentPhase::Preparing | ContentPhase::Running
        )
        && !state
            .run
            .as_ref()
            .is_some_and(|run| run.check_links && state.link_check_total > state.link_check_done);
    let hidden_review = review.len().saturating_sub(3);

    OverviewModel {
        review,
        hidden_review,
        checks,
        optional,
        coverage,
        clean_checks,
        finding_checks,
        partial_checks,
        not_run_checks,
        coverage_complete,
    }
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    kind: ReviewKind,
    title: String,
    why: &'static str,
    affected: usize,
    eligible: Option<usize>,
    unit: Unit,
    sample: Option<String>,
    target: ReviewTarget,
    query: Option<&'static str>,
) -> ReviewCandidate {
    ReviewCandidate {
        kind,
        title,
        why,
        affected,
        eligible,
        unit,
        sample,
        signal: kind.signal(),
        target,
        query,
        priority: kind.priority(),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_pages(
    out: &mut Vec<ReviewCandidate>,
    kind: ReviewKind,
    singular: &str,
    plural: &str,
    why: &'static str,
    tally: &Tally,
    eligible: usize,
    target: ReviewTarget,
    query: Option<&'static str>,
) {
    if tally.affected == 0 {
        return;
    }
    let phrase = if tally.affected == 1 {
        format!("1 {singular}")
    } else {
        format!("{} {plural}", tally.affected)
    };
    out.push(candidate(
        kind,
        phrase,
        why,
        tally.affected,
        Some(eligible),
        Unit::Pages,
        tally.sample.clone(),
        target,
        query,
    ));
}

#[allow(clippy::too_many_arguments)]
fn push_field(
    out: &mut Vec<ReviewCandidate>,
    kind: ReviewKind,
    singular: &str,
    plural: &str,
    why: &'static str,
    tally: &Tally,
    eligible: usize,
    target: ReviewTarget,
    query: Option<&'static str>,
) {
    if tally.affected == 0 {
        return;
    }
    let phrase = if tally.affected == 1 {
        format!("1 {singular}")
    } else {
        format!("{} {plural}", tally.affected)
    };
    out.push(candidate(
        kind,
        phrase,
        why,
        tally.affected,
        Some(eligible),
        Unit::Pages,
        tally.sample.clone(),
        target,
        query,
    ));
}

#[allow(clippy::too_many_arguments)]
fn check_rows(
    oc: &OverviewCounts,
    counts: Counts,
    title: &FieldTally,
    description: &FieldTally,
    heading: &H1Tally,
    server: &Tally,
    client: &Tally,
    fetch: &Tally,
    homepage: &Tally,
    noindex: &Tally,
    canonical_elsewhere: &Tally,
    canonical_review: &Tally,
    images: &Tally,
    links: &Tally,
    content_partial: usize,
) -> Vec<CheckRow> {
    let response_note = if homepage.affected > 0 {
        "Homepage error"
    } else if server.affected > 0 {
        "Server errors"
    } else if fetch.affected > 0 {
        "Fetch failures"
    } else if client.affected > 0 {
        "Client errors"
    } else if oc.response_redirect > 0 {
        "Redirects"
    } else {
        ""
    };
    let indexing_note = if noindex.affected > 0 {
        "Noindex"
    } else if canonical_elsewhere.affected > 0 {
        "Canonical elsewhere"
    } else if canonical_review.affected > 0 {
        "Review canonical"
    } else {
        ""
    };
    let image_note = if oc.pages > 0 && oc.images_eligible == 0 {
        "No images"
    } else if images.affected > 0 {
        "Missing alt"
    } else {
        ""
    };
    let link_note = if links.affected > 0 {
        "Broken links"
    } else {
        ""
    };

    let rows = vec![
        CheckRow {
            group: OverviewGroup::Response,
            status: check_status(counts.response, counts.pages, 0),
            affected: counts.response,
            eligible: counts.pages,
            partial: 0,
            unit: Unit::Pages,
            note: response_note,
        },
        CheckRow {
            group: OverviewGroup::Indexability,
            status: check_status(oc.indexability, counts.pages, content_partial),
            affected: oc.indexability,
            eligible: counts.pages,
            partial: content_partial,
            unit: Unit::Pages,
            note: indexing_note,
        },
        CheckRow {
            group: OverviewGroup::Titles,
            status: check_status(counts.title, oc.titles_eligible, content_partial),
            affected: counts.title,
            eligible: oc.titles_eligible,
            partial: content_partial,
            unit: Unit::Pages,
            note: title.note(),
        },
        CheckRow {
            group: OverviewGroup::Descriptions,
            status: check_status(counts.meta, oc.descriptions_eligible, content_partial),
            affected: counts.meta,
            eligible: oc.descriptions_eligible,
            partial: content_partial,
            unit: Unit::Pages,
            note: description.note(),
        },
        CheckRow {
            group: OverviewGroup::Headings,
            status: check_status(counts.h1, oc.headings_eligible, content_partial),
            affected: counts.h1,
            eligible: oc.headings_eligible,
            partial: content_partial,
            unit: Unit::Pages,
            note: heading.note(),
        },
        CheckRow {
            group: OverviewGroup::Images,
            status: if oc.pages > 0 && oc.images_eligible == 0 {
                CheckStatus::Clean
            } else {
                check_status(counts.images, oc.images_eligible, content_partial)
            },
            affected: counts.images,
            eligible: oc.images_eligible,
            partial: content_partial,
            unit: Unit::Pages,
            note: image_note,
        },
        CheckRow {
            group: OverviewGroup::Links,
            status: check_status(counts.links, oc.links_eligible, content_partial),
            affected: counts.links,
            eligible: oc.links_eligible,
            partial: content_partial,
            unit: Unit::Pages,
            note: link_note,
        },
    ];
    rows
}

fn check_status(affected: usize, eligible: usize, partial: usize) -> CheckStatus {
    if affected > 0 {
        CheckStatus::Findings
    } else if partial > 0 {
        CheckStatus::Partial
    } else if eligible == 0 {
        CheckStatus::NotRun
    } else {
        CheckStatus::Clean
    }
}

fn optional_rows(
    state: &State,
    oc: &OverviewCounts,
    accessibility_enabled: bool,
) -> Vec<OptionalRow> {
    let mut rows = Vec::new();
    let sitemap_files = OptionalRow {
        key: "overview-sitemap-files",
        label: "Sitemap files",
        status: check_status(oc.sitemap_file_findings, oc.sitemap_file_total, 0),
        affected: oc.sitemap_file_findings,
        eligible: oc.sitemap_file_total,
        partial: 0,
        unit: Unit::Files,
        note: if oc.sitemap_file_findings > 0 {
            "Could not read".into()
        } else if oc.sitemap_file_total > 0 {
            "Readable".into()
        } else {
            "No sitemap files".into()
        },
        target: (oc.sitemap_file_findings > 0).then_some(ReviewTarget::SitemapFilesFindings),
    };
    rows.push(sitemap_files);

    let sitemap_urls = OptionalRow {
        key: "overview-sitemap-urls",
        label: "Sitemap URLs",
        status: check_status(
            oc.sitemap_url_findings,
            oc.sitemap_checked,
            oc.sitemap_checked_incomplete,
        ),
        affected: oc.sitemap_url_findings,
        eligible: oc.sitemap_checked,
        partial: oc.sitemap_checked_incomplete,
        unit: Unit::Pages,
        note: "Also counted in page checks".into(),
        target: (oc.sitemap_url_findings > 0).then_some(ReviewTarget::SitemapUrlsFindings),
    };
    rows.push(sitemap_urls);

    let structured = OptionalRow {
        key: "overview-structured",
        label: "JSON-LD",
        status: check_status(
            oc.structured_urls,
            oc.structured_eligible,
            oc.structured_incomplete,
        ),
        affected: oc.structured_urls,
        eligible: oc.structured_eligible,
        partial: oc.structured_incomplete,
        unit: Unit::Pages,
        note: if oc.structured_urls > 0 {
            "Syntax or duplicate key".into()
        } else if oc.structured_incomplete > 0 {
            "Inspection incomplete".into()
        } else if oc.structured_eligible > 0 {
            "No findings".into()
        } else {
            "Not run".into()
        },
        target: (oc.structured_urls > 0).then_some(ReviewTarget::StructuredFindings),
    };
    rows.push(structured);

    let accessibility = if accessibility_enabled {
        let note = if oc.accessibility_review_urls > 0 {
            format!("{} need review", oc.accessibility_review_urls)
        } else if oc.accessibility_urls > 0 {
            "Violations".into()
        } else if oc.accessibility_incomplete > 0 {
            "Coverage incomplete".into()
        } else {
            "No automated violations".into()
        };
        OptionalRow {
            key: "overview-accessibility",
            label: "Accessibility",
            status: check_status(
                oc.accessibility_urls,
                oc.accessibility_eligible,
                oc.accessibility_incomplete,
            ),
            affected: oc.accessibility_urls,
            eligible: oc.accessibility_eligible,
            partial: oc.accessibility_incomplete,
            unit: Unit::Pages,
            note,
            target: (oc.accessibility_urls > 0).then_some(ReviewTarget::AccessibilityFindings),
        }
    } else {
        OptionalRow {
            key: "overview-accessibility",
            label: "Accessibility",
            status: CheckStatus::NotRun,
            affected: 0,
            eligible: 0,
            partial: 0,
            unit: Unit::Pages,
            note: if state
                .run
                .as_ref()
                .is_some_and(|run| run.rendering == RenderMode::Browser)
            {
                "Off in this run".into()
            } else {
                "Requires Browser capture".into()
            },
            target: None,
        }
    };
    rows.push(accessibility);

    let content_status = |affected: usize, eligible: usize, note: &str| match state.content_phase {
        ContentPhase::Ready => {
            if affected > 0 {
                (CheckStatus::Findings, "Review pairs".to_string())
            } else if eligible > 0 {
                (CheckStatus::Clean, "No pairs".to_string())
            } else {
                (CheckStatus::NotRun, "Not enough text".to_string())
            }
        }
        ContentPhase::Preparing | ContentPhase::Running => {
            (CheckStatus::Partial, "Running".to_string())
        }
        ContentPhase::Cancelled | ContentPhase::Failed => {
            (CheckStatus::Partial, "Did not finish".to_string())
        }
        ContentPhase::Idle => (CheckStatus::NotRun, note.to_string()),
    };
    let (same_status, same_note) =
        content_status(oc.content_same_urls, oc.content_exact_eligible, "Not run");
    rows.push(OptionalRow {
        key: "overview-content-same",
        label: "Same-text pages",
        status: same_status,
        affected: oc.content_same_urls,
        eligible: oc.content_exact_eligible,
        partial: 0,
        unit: Unit::Pages,
        note: same_note,
        target: (oc.content_same_urls > 0).then_some(ReviewTarget::DuplicateSame),
    });
    let (similar_status, similar_note) =
        content_status(oc.content_similar_urls, oc.content_near_eligible, "Not run");
    rows.push(OptionalRow {
        key: "overview-content-similar",
        label: "Similar pages",
        status: similar_status,
        affected: oc.content_similar_urls,
        eligible: oc.content_near_eligible,
        partial: 0,
        unit: Unit::Pages,
        note: similar_note,
        target: (oc.content_similar_urls > 0).then_some(ReviewTarget::DuplicateSimilar),
    });

    let link_checks = if state.run.as_ref().is_some_and(|run| run.check_links) {
        let total = state.link_check_total;
        let status = if oc.link_findings > 0 {
            CheckStatus::Findings
        } else if oc.link_incomplete > 0 {
            CheckStatus::Partial
        } else if total == 0 {
            CheckStatus::NotRun
        } else {
            CheckStatus::Clean
        };
        OptionalRow {
            key: "overview-optional-link-checks",
            label: "Link checks",
            status,
            affected: oc.link_findings,
            eligible: 0,
            partial: oc.link_incomplete,
            unit: Unit::Occurrences,
            note: if oc.link_incomplete > 0 {
                format!("{} unchecked", oc.link_incomplete)
            } else if total > 0 {
                format!("{} destinations", total)
            } else {
                "No destinations".into()
            },
            target: if oc.link_findings > 0 {
                Some(ReviewTarget::LinkEvidenceFindings)
            } else if oc.link_incomplete > 0 {
                Some(ReviewTarget::LinkEvidenceIncomplete)
            } else {
                None
            },
        }
    } else {
        OptionalRow {
            key: "overview-optional-link-checks",
            label: "Link checks",
            status: CheckStatus::NotRun,
            affected: 0,
            eligible: 0,
            partial: 0,
            unit: Unit::Occurrences,
            note: "Off in this run".into(),
            target: None,
        }
    };
    rows.push(link_checks);
    rows
}

fn coverage_rows(state: &State, oc: &OverviewCounts, content_partial: usize) -> Vec<CoverageRow> {
    let mut rows = Vec::new();
    let page_limit = state
        .run
        .as_ref()
        .map(|run| run.limits.max_pages)
        .unwrap_or(state.max_pages);
    let reason = match state.phase {
        RunPhase::Limited => "page limit",
        RunPhase::Stopped => "crawl stopped",
        RunPhase::Failed | RunPhase::Blocked => "crawl ended early",
        _ if state.phase.active() => "crawl still running",
        _ => "not reached",
    };
    if oc.sitemap_not_reached > 0 {
        rows.push(CoverageRow {
            key: "overview-sitemap-not-reached",
            label: if oc.sitemap_total > 0 {
                format!(
                    "{} of {} sitemap URLs not reached",
                    oc.sitemap_not_reached, oc.sitemap_total
                )
            } else {
                format!("{} sitemap URLs not reached", oc.sitemap_not_reached)
            },
            reason: reason.into(),
            target: Some(ReviewTarget::SitemapUrlsIncomplete),
            adjust_limit: matches!(state.phase, RunPhase::Limited),
            signal: Signal::Amber,
        });
    } else if matches!(state.phase, RunPhase::Limited) {
        rows.push(CoverageRow {
            key: "overview-page-limit",
            label: format!("Crawl stopped at the {page_limit}-page limit"),
            reason: "later URLs were not reached".into(),
            target: None,
            adjust_limit: true,
            signal: Signal::Amber,
        });
    }
    if oc.sitemap_blocked > 0 {
        rows.push(CoverageRow {
            key: "overview-sitemap-blocked",
            label: format!(
                "{} sitemap {} blocked by robots.txt",
                oc.sitemap_blocked,
                if oc.sitemap_blocked == 1 {
                    "URL"
                } else {
                    "URLs"
                }
            ),
            reason: "robots.txt".into(),
            target: Some(ReviewTarget::SitemapUrlsIncomplete),
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    if oc.sitemap_skipped > 0 {
        rows.push(CoverageRow {
            key: "overview-sitemap-skipped",
            label: format!(
                "{} sitemap {} skipped",
                oc.sitemap_skipped,
                if oc.sitemap_skipped == 1 {
                    "URL"
                } else {
                    "URLs"
                }
            ),
            reason: "out of scope or invalid".into(),
            target: Some(ReviewTarget::SitemapUrlsIncomplete),
            adjust_limit: false,
            signal: Signal::Muted,
        });
    }
    if oc.sitemap_checked_incomplete > 0 {
        rows.push(CoverageRow {
            key: "overview-sitemap-incomplete",
            label: format!(
                "{} checked sitemap {} incomplete evidence",
                oc.sitemap_checked_incomplete,
                if oc.sitemap_checked_incomplete == 1 {
                    "URL has"
                } else {
                    "URLs have"
                }
            ),
            reason: "capture did not finish".into(),
            target: Some(ReviewTarget::SitemapUrlsIncomplete),
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    if content_partial > 0 {
        rows.push(CoverageRow {
            key: "overview-incomplete",
            label: format!(
                "{} {} did not finish capture or read",
                content_partial,
                if content_partial == 1 {
                    "page"
                } else {
                    "pages"
                }
            ),
            reason: "content checks withheld".into(),
            target: Some(ReviewTarget::InternalIncomplete),
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    if oc.structured_incomplete > 0 && !state.phase.active() {
        rows.push(CoverageRow {
            key: "overview-structured-incomplete",
            label: format!(
                "{} {} incomplete JSON-LD coverage",
                oc.structured_incomplete,
                if oc.structured_incomplete == 1 {
                    "URL has"
                } else {
                    "URLs have"
                }
            ),
            reason: "inspection did not finish".into(),
            target: Some(ReviewTarget::StructuredIncomplete),
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    if oc.accessibility_incomplete > 0 && !state.phase.active() {
        rows.push(CoverageRow {
            key: "overview-accessibility-incomplete",
            label: format!(
                "{} {} incomplete accessibility coverage",
                oc.accessibility_incomplete,
                if oc.accessibility_incomplete == 1 {
                    "URL has"
                } else {
                    "URLs have"
                }
            ),
            reason: "audit did not finish".into(),
            target: Some(ReviewTarget::AccessibilityIncomplete),
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    if matches!(state.phase, RunPhase::Stopped) && oc.sitemap_not_reached == 0 {
        rows.push(CoverageRow {
            key: "overview-stopped",
            label: "Crawl stopped before it finished".into(),
            reason: "later URLs were not reached".into(),
            target: None,
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    if matches!(state.phase, RunPhase::Failed | RunPhase::Blocked) {
        rows.push(CoverageRow {
            key: "overview-failed",
            label: "The crawl ended before its checks finished".into(),
            reason: "results are partial".into(),
            target: None,
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    if let Some(partial) = &state.sitemap.partial {
        rows.push(CoverageRow {
            key: "overview-sitemap-partial",
            label: "Sitemap discovery stopped early".into(),
            reason: partial.clone(),
            target: Some(ReviewTarget::SitemapUrlsAll),
            adjust_limit: false,
            signal: Signal::Amber,
        });
    }
    match state.content_phase {
        ContentPhase::Cancelled | ContentPhase::Failed => rows.push(CoverageRow {
            key: "overview-content-incomplete",
            label: "Content comparison did not finish".into(),
            reason: "duplicate-content coverage is partial".into(),
            target: Some(ReviewTarget::DuplicateNotCompared),
            adjust_limit: false,
            signal: Signal::Amber,
        }),
        ContentPhase::Ready
            if state
                .content_analysis
                .as_deref()
                .is_some_and(|analysis| !analysis.coverage.limited_reasons.is_empty()) =>
        {
            rows.push(CoverageRow {
                key: "overview-content-limited",
                label: "Content comparison hit a limit".into(),
                reason: state
                    .content_analysis
                    .as_deref()
                    .and_then(|analysis| analysis.coverage.limited_reasons.first())
                    .cloned()
                    .unwrap_or_else(|| "documented bound".into()),
                target: Some(ReviewTarget::DuplicateNotCompared),
                adjust_limit: false,
                signal: Signal::Amber,
            });
        }
        _ => {}
    }
    if state.run.as_ref().is_some_and(|run| run.check_links)
        && !state.phase.active()
        && state.link_check_total > state.link_check_done
    {
        let remaining = state.link_check_total - state.link_check_done;
        rows.push(CoverageRow {
            key: "overview-link-checks",
            label: format!(
                "{} of {} link destinations not checked",
                remaining, state.link_check_total
            ),
            reason: "optional checks did not finish".into(),
            target: Some(ReviewTarget::LinkEvidenceIncomplete),
            adjust_limit: false,
            signal: Signal::Muted,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(url: &str, title: &str, meta: &str, h1: &str) -> Page {
        let mut page = crawl::sample_pages()
            .into_iter()
            .nth(3)
            .expect("clean sample page");
        page.url = url.into();
        page.title = title.into();
        page.meta_description = meta.into();
        page.h1s = vec![h1.into()];
        page.canonical = url.into();
        page.links = Arc::new(Vec::new());
        page.images = 0;
        page.images_missing_alt = 0;
        page.indexability = super::super::scene_complete_assessment(&page);
        page
    }

    fn title(n: usize) -> String {
        format!("Page {n} of the calm crawl guide")
    }

    fn meta(n: usize) -> String {
        format!(
            "Page {n} explains how the crawl reads titles, links, images and responses without guessing."
        )
    }

    fn capped_fixture() -> State {
        let mut state = init();
        let mut pages = Vec::new();
        for n in 1..=40 {
            let url = if n == 1 {
                "https://example.com/".to_string()
            } else {
                format!("https://example.com/page-{n}")
            };
            let page_title = if n <= 11 { String::new() } else { title(n) };
            let page_meta = if (12..=13).contains(&n) {
                String::new()
            } else {
                meta(n)
            };
            pages.push(page(
                &url,
                &page_title,
                &page_meta,
                &format!("Page {n}: what this crawl found"),
            ));
        }
        install_sample_content(&mut pages);
        state.pages = pages;
        state.audit = Arc::new(Audit::from_pages(&state.pages));
        state.run = Some(RunContext {
            seed: "https://example.com/".into(),
            host: "example.com".into(),
            limits: Limits {
                max_pages: 40,
                max_depth: 3,
            },
            rendering: RenderMode::Html,
            sitemaps: true,
            check_links: false,
            accessibility: false,
            started_utc: String::new(),
            extraction_rules: Vec::new(),
            list_total: 0,
        });
        state.phase = RunPhase::Limited;
        state.max_pages = 40;
        state.sitemap.discovery_done = true;
        for n in 1..=40 {
            state.sitemap.insert_url(SitemapUrl {
                url: format!("https://example.com/page-{n}"),
                state: UrlState::Checked,
                ..SitemapUrl::default()
            });
        }
        for n in 1..=81 {
            state.sitemap.insert_url(SitemapUrl {
                url: format!("https://example.com/later-{n}"),
                state: UrlState::NotChecked,
                ..SitemapUrl::default()
            });
        }
        state
    }

    #[test]
    fn capped_fixture_has_truthful_counts_and_denominators() {
        let state = capped_fixture();
        let (_, counts, model) = build(&state, false);
        assert_eq!(counts.pages, 40);
        assert_eq!(counts.titles, 11);
        assert_eq!(counts.descriptions, 2);
        assert_eq!(counts.titles_eligible, 40);
        assert_eq!(counts.sitemap_not_reached, 81);
        assert_eq!(counts.sitemap_total, 121);
        assert_eq!(model.review[0].kind, ReviewKind::MissingTitle);
        assert_eq!(model.review[0].count_text(), "11 of 40 pages");
        assert_eq!(model.review[1].kind, ReviewKind::MissingMeta);
        assert_eq!(model.review[1].count_text(), "2 of 40 pages");
        assert_eq!(
            model.coverage[0].label,
            "81 of 121 sitemap URLs not reached"
        );
        assert_eq!(model.coverage[0].reason, "page limit");
        assert!(model.coverage[0].adjust_limit);
    }

    #[test]
    fn homepage_server_error_outranks_title_and_meta_findings() {
        let mut state = capped_fixture();
        state.phase = RunPhase::Completed;
        if let Some(page) = state.pages.first_mut() {
            page.status = 500;
            page.indexability = super::super::scene_complete_assessment(page);
        }
        state.audit = Arc::new(Audit::from_pages(&state.pages));
        let (_, counts, model) = build(&state, false);
        assert_eq!(counts.response, 1);
        assert_eq!(model.review[0].kind, ReviewKind::HomepageError);
        assert_eq!(model.review[0].title, "Homepage returned a 500");
        assert_eq!(model.review[1].kind, ReviewKind::MissingTitle);
        assert_eq!(model.review[2].kind, ReviewKind::MissingMeta);
        assert!(model.review[0].priority > model.review[1].priority);
        assert!(model.review[1].priority > model.review[2].priority);
    }

    #[test]
    fn clean_crawl_distinguishes_clean_from_not_run() {
        let mut state = capped_fixture();
        state.phase = RunPhase::Completed;
        for (index, page) in state.pages.iter_mut().enumerate() {
            let n = index + 1;
            page.title = title(n);
            page.meta_description = meta(n);
            page.images = 1;
            page.images_missing_alt = 0;
        }
        state.audit = Arc::new(Audit::from_pages(&state.pages));
        state.sitemap.urls.clear();
        state.content_phase = ContentPhase::Ready;
        let (_, counts, model) = build(&state, false);
        assert_eq!(counts.titles, 0);
        assert_eq!(counts.descriptions, 0);
        assert!(model.review.is_empty());
        assert_eq!(model.finding_checks, 0);
        assert!(model.clean_checks >= 7);
        let accessibility = model
            .optional
            .iter()
            .find(|row| row.label == "Accessibility")
            .expect("accessibility row");
        assert_eq!(accessibility.status, CheckStatus::NotRun);
        let link_checks = model
            .optional
            .iter()
            .find(|row| row.label == "Link checks")
            .expect("link checks row");
        assert_eq!(link_checks.status, CheckStatus::NotRun);
    }

    #[test]
    fn coverage_labels_agree_with_a_single_sitemap_url() {
        let mut state = capped_fixture();
        state.sitemap.insert_url(SitemapUrl {
            url: "https://example.com/blocked".into(),
            state: UrlState::Blocked,
            ..SitemapUrl::default()
        });
        state.sitemap.insert_url(SitemapUrl {
            url: "https://example.com/skipped".into(),
            state: UrlState::Skipped,
            ..SitemapUrl::default()
        });
        let (_, _, model) = build(&state, false);
        assert!(
            model
                .coverage
                .iter()
                .any(|row| row.label == "1 sitemap URL blocked by robots.txt")
        );
        assert!(
            model
                .coverage
                .iter()
                .any(|row| row.label == "1 sitemap URL skipped")
        );
    }

    #[test]
    fn affected_counts_never_print_a_smaller_denominator() {
        let candidate = candidate(
            ReviewKind::Structured,
            "3 pages have a JSON-LD finding".into(),
            "Malformed JSON-LD cannot be parsed.",
            3,
            Some(2),
            Unit::Pages,
            None,
            ReviewTarget::StructuredFindings,
            None,
        );
        assert_eq!(candidate.count_text(), "3 pages");

        let check = CheckRow {
            group: OverviewGroup::Titles,
            status: CheckStatus::Findings,
            affected: 3,
            eligible: 2,
            partial: 1,
            unit: Unit::Pages,
            note: "Missing",
        };
        assert_eq!(check.count_text(), "3 pages");

        let optional = OptionalRow {
            key: "overview-structured",
            label: "JSON-LD",
            status: CheckStatus::Findings,
            affected: 3,
            eligible: 2,
            partial: 1,
            unit: Unit::Pages,
            note: String::new(),
            target: Some(ReviewTarget::StructuredFindings),
        };
        assert_eq!(optional.count_text(), "3 pages");
    }

    #[test]
    fn coverage_separates_not_reached_from_sitemap_findings() {
        let mut state = capped_fixture();
        state.sitemap.files.push(SitemapFile {
            url: "https://example.com/broken-sitemap.xml".into(),
            state: FileState::Error,
            status: 404,
            issue: "Not found".into(),
            ..SitemapFile::default()
        });
        let mut listed = SitemapUrl {
            url: "https://example.com/listed-with-finding".into(),
            state: UrlState::Checked,
            status: 500,
            content_type: "text/html".into(),
            ..SitemapUrl::default()
        };
        listed
            .assessment
            .findings
            .push(calmtoad::indexability::Finding {
                code: FindingCode::HttpError,
                severity: Severity::Issue,
                text: "500 Server error".into(),
                guidance: String::new(),
            });
        listed.assessment.label = Label::HttpError;
        state.sitemap.insert_url(listed);
        let (_, counts, model) = build(&state, false);
        assert_eq!(counts.sitemap_file_findings, 1);
        assert_eq!(counts.sitemap_url_findings, 1);
        assert!(
            model
                .coverage
                .iter()
                .all(|row| row.key != "overview-sitemap-files"),
            "file findings are observed issues, not coverage gaps"
        );
        assert!(
            model
                .optional
                .iter()
                .any(|row| row.label == "Sitemap files" && row.status == CheckStatus::Findings)
        );
        assert!(
            model
                .coverage
                .iter()
                .any(|row| row.key == "overview-sitemap-not-reached")
        );
    }
}
