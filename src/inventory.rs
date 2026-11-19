//! Complete per-URL content inventory projection.
//!
//! This is intentionally a projection over retained crawl facts, not a second crawler. It
//! repeats run context on every row so one CSV is self-contained, and it keeps unknown values
//! empty rather than turning them into zero or a finding.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crawl::{
    Audit, Page, auditable, canonical_issue, h1_issue, image_issue, links_issue, meta_issue,
    response_issue, title_issue,
};
use crate::extract::ExtractionRule;
use crate::links::LinkCheckResult;

pub const HEADERS: &[&str] = &[
    "URL",
    "STATUS",
    "ERROR",
    "CONTENT TYPE",
    "CAPTURE",
    "CAPTURE COMPLETE",
    "CAPTURE REASON",
    "TITLE",
    "META DESCRIPTION",
    "H1S",
    "WORDS",
    "DEPTH",
    "INBOUND",
    "OUTBOUND",
    "CANONICAL",
    "INDEXABILITY",
    "REASONS",
    "TITLE FINDING",
    "META FINDING",
    "H1 FINDING",
    "RESPONSE FINDING",
    "CANONICAL FINDING",
    "IMAGE FINDING",
    "LINKS FINDING",
    "LINK OCCURRENCES",
    "LINK CHECKED",
    "LINK FINDINGS",
    "LINK EVIDENCE LIMITED",
    "RUN SEED",
    "RUN STARTED UTC",
    "RUN PAGE LIMIT",
    "RUN DEPTH LIMIT",
    "RUN SITEMAPS",
    "RUN CHECK LINKS",
    "RUN OUTCOME",
    "RUN CAPTURED URLS",
    "RUN INCOMPLETE CHECKS",
    "RUN SITEMAP URL CHECKS INCOMPLETE",
    "RUN SITEMAP FILE FINDINGS",
    "RUN LINK EVIDENCE LIMITED",
    "RUN SNAPSHOT",
    "RUN EXTRACTION RULES",
];

#[derive(Clone, Debug, Default)]
pub struct InventoryRun {
    pub seed: String,
    pub started_utc: String,
    pub page_limit: usize,
    pub depth_limit: u8,
    pub sitemaps: bool,
    pub check_links: bool,
    pub outcome: String,
    pub captured_urls: usize,
    pub incomplete: usize,
    pub sitemap_incomplete: usize,
    pub sitemap_file_findings: usize,
    pub link_evidence_limited: bool,
    /// When and how the exported snapshot was taken, separate from run completion.
    pub snapshot: String,
    pub extraction_rules: Vec<ExtractionRule>,
}

pub struct InventorySummary<'a> {
    pub run: &'a InventoryRun,
    pub link_checks: &'a HashMap<String, LinkCheckResult>,
}

pub fn rows(
    pages: &[Page],
    audit: &Audit,
    indices: &[usize],
    summary: &InventorySummary<'_>,
) -> Vec<Vec<String>> {
    indices
        .iter()
        .filter_map(|index| pages.get(*index))
        .map(|page| row(page, audit, summary))
        .collect()
}

fn row(page: &Page, audit: &Audit, summary: &InventorySummary<'_>) -> Vec<String> {
    let known = auditable(page);
    let capture = &page.indexability.capture;
    let render_reason = capture
        .render
        .as_ref()
        .map(|render| {
            if render.reason.trim().is_empty() {
                render.state.label().to_string()
            } else {
                render.reason.trim().to_string()
            }
        })
        .unwrap_or_default();
    let link_stats = link_stats(page, summary.link_checks);
    let mut row = vec![
        page.url.clone(),
        if page.status == 0 {
            String::new()
        } else {
            page.status.to_string()
        },
        page.error.clone(),
        page.content_type.clone(),
        capture.mode.label().to_string(),
        if capture.is_complete() { "yes" } else { "no" }.into(),
        render_reason,
        if known {
            page.title.clone()
        } else {
            String::new()
        },
        if known {
            page.meta_description.clone()
        } else {
            String::new()
        },
        if known {
            serde_json::to_string(&page.h1s).unwrap_or_default()
        } else {
            String::new()
        },
        if known {
            page.words.to_string()
        } else {
            String::new()
        },
        page.depth.to_string(),
        audit.inbound(page).to_string(),
        page.links.len().to_string(),
        page.canonical.clone(),
        page.indexability.label.text().to_string(),
        page.indexability.reason_text(),
        finding(known, title_issue(page, audit)),
        finding(known, meta_issue(page, audit)),
        finding(known, h1_issue(page, audit)),
        finding(true, response_issue(page)),
        finding(known, canonical_issue(page)),
        finding(known, image_issue(page)),
        finding(true, links_issue(page, audit)),
        link_stats.occurrences.to_string(),
        link_stats.checked.to_string(),
        link_stats.findings.to_string(),
        if page.link_evidence_limit.is_empty() {
            String::new()
        } else {
            page.link_evidence_limit.clone()
        },
        summary.run.seed.clone(),
        summary.run.started_utc.clone(),
        summary.run.page_limit.to_string(),
        summary.run.depth_limit.to_string(),
        if summary.run.sitemaps { "on" } else { "off" }.into(),
        if summary.run.check_links { "on" } else { "off" }.into(),
        summary.run.outcome.clone(),
        summary.run.captured_urls.to_string(),
        summary.run.incomplete.to_string(),
        summary.run.sitemap_incomplete.to_string(),
        summary.run.sitemap_file_findings.to_string(),
        if summary.run.link_evidence_limited {
            "yes"
        } else {
            "no"
        }
        .into(),
        summary.run.snapshot.clone(),
        serde_json::to_string(&summary.run.extraction_rules).unwrap_or_default(),
    ];
    for rule in &summary.run.extraction_rules {
        let outcome = page
            .extraction
            .iter()
            .find(|outcome| outcome.rule_id == rule.id);
        row.push(rule.name.clone());
        row.push(
            outcome
                .map(|outcome| serde_json::to_string(&outcome.values).unwrap_or_default())
                .unwrap_or_default(),
        );
        row.push(
            outcome
                .map(|outcome| outcome.state.label().to_string())
                .unwrap_or_else(|| "Not evaluated".into()),
        );
        row.push(
            outcome
                .and_then(|outcome| outcome.stage)
                .map(|stage| stage.label().to_string())
                .unwrap_or_default(),
        );
        row.push(
            outcome
                .map(|outcome| outcome.reason.clone())
                .unwrap_or_default(),
        );
    }
    row.shrink_to_fit();
    row
}

fn finding(known: bool, issue: String) -> String {
    if known { issue } else { String::new() }
}

#[derive(Default)]
struct LinkStats {
    occurrences: usize,
    checked: usize,
    findings: usize,
}

fn link_stats(page: &Page, checks: &HashMap<String, LinkCheckResult>) -> LinkStats {
    let mut stats = LinkStats::default();
    for occurrence in page.link_evidence.iter() {
        stats.occurrences += 1;
        let check = checks.get(&occurrence.key).or(occurrence.check.as_deref());
        if let Some(check) = check {
            if !check.state.is_incomplete() {
                stats.checked += 1;
            }
            if check.is_finding() {
                stats.findings += 1;
            }
        }
    }
    stats
}

pub fn headers_with_extraction(rules: &[ExtractionRule]) -> Vec<String> {
    let mut headers = HEADERS
        .iter()
        .map(|header| header.to_string())
        .collect::<Vec<_>>();
    for rule in rules {
        let id = &rule.id;
        headers.push(format!("EXTRACT {id} NAME"));
        headers.push(format!("EXTRACT {id} VALUES"));
        headers.push(format!("EXTRACT {id} STATE"));
        headers.push(format!("EXTRACT {id} STAGE"));
        headers.push(format!("EXTRACT {id} REASON"));
    }
    headers
}

pub fn utc_now() -> String {
    format_utc(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default(),
    )
}

/// Small UTC formatter for a self-contained CSV timestamp, without a date dependency.
pub fn format_utc(seconds: u64) -> String {
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_is_iso_8601() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn mixed_pages_keep_null_zero_and_run_context_distinct() {
        let pages = crate::crawl::indexability_sample_pages();
        let audit = Audit::from_pages(&pages);
        let run = InventoryRun {
            seed: "https://example.com/".into(),
            started_utc: "2026-11-17T00:00:00Z".into(),
            page_limit: 40,
            depth_limit: 3,
            sitemaps: true,
            check_links: true,
            outcome: "9 pages on example.com".into(),
            captured_urls: pages.len(),
            incomplete: 0,
            sitemap_incomplete: 2,
            sitemap_file_findings: 1,
            link_evidence_limited: false,
            snapshot: "complete at test".into(),
            extraction_rules: Vec::new(),
        };
        let checks = HashMap::new();
        let summary = InventorySummary {
            run: &run,
            link_checks: &checks,
        };
        let indices = (0..pages.len()).collect::<Vec<_>>();
        let rows = rows(&pages, &audit, &indices, &summary);
        assert_eq!(rows.len(), pages.len());
        assert!(rows.iter().all(|row| row.len() == HEADERS.len()));
        let pdf = rows
            .iter()
            .find(|row| row[0].ends_with("/report-2026.pdf"))
            .expect("pdf row");
        assert_eq!(pdf[1], "200");
        assert_eq!(pdf[7], "", "a PDF has no title finding or invented title");
        assert_eq!(pdf[10], "", "unmeasured words stay empty, not zero");
        assert_eq!(pdf[29], "2026-11-17T00:00:00Z");
        assert_eq!(pdf[32], "on");
        assert_eq!(pdf[33], "on");
        assert_eq!(pdf[37], "2");
        let home = rows
            .iter()
            .find(|row| row[0] == "https://example.com/")
            .unwrap();
        assert_eq!(home[10], "86");
        assert_eq!(home[12], audit.inbound(&pages[0]).to_string());
        assert!(
            home[9].starts_with('['),
            "H1s are a JSON array: {}",
            home[9]
        );
        assert_eq!(
            home.last().map(String::as_str),
            Some("[]"),
            "no extraction rules means an empty definition"
        );
    }

    #[test]
    #[ignore = "local 10k/50k responsiveness measurement"]
    fn synthetic_inventory_10k_and_50k_stay_bounded() {
        let template = crate::crawl::sample_pages().remove(0);
        for count in [10_000usize, 50_000] {
            let pages = (0..count)
                .map(|index| {
                    let mut page = template.clone();
                    page.url = format!("https://example.com/page-{index}");
                    std::sync::Arc::make_mut(&mut page.link_evidence).clear();
                    std::sync::Arc::make_mut(&mut page.extraction).clear();
                    page
                })
                .collect::<Vec<_>>();
            let audit = Audit::from_pages(&pages);
            let run = InventoryRun {
                seed: "https://example.com/".into(),
                started_utc: "2026-11-17T00:00:00Z".into(),
                page_limit: count,
                depth_limit: 3,
                sitemaps: true,
                check_links: false,
                outcome: format!("{count} pages"),
                captured_urls: count,
                incomplete: 0,
                sitemap_incomplete: 0,
                sitemap_file_findings: 0,
                link_evidence_limited: false,
                snapshot: "in progress at test".into(),
                extraction_rules: Vec::new(),
            };
            let checks = HashMap::new();
            let summary = InventorySummary {
                run: &run,
                link_checks: &checks,
            };
            let indices = (0..count).collect::<Vec<_>>();
            let started = std::time::Instant::now();
            let snapshot_copy = pages.clone();
            let copied = started.elapsed();
            let started = std::time::Instant::now();
            let audit_copy = audit.clone();
            let audit_copied = started.elapsed();
            assert_eq!(snapshot_copy.len(), count);
            drop(audit_copy);
            let started = std::time::Instant::now();
            let rows = rows(&pages, &audit, &indices, &summary);
            let projected = started.elapsed();
            let refs = HEADERS.to_vec();
            let started = std::time::Instant::now();
            let csv = crate::export::to_csv(&refs, &rows);
            eprintln!(
                "{count} rows: snapshot clone {copied:?}, audit clone {audit_copied:?}, project {projected:?}, csv {:?}, bytes {}",
                started.elapsed(),
                csv.len()
            );
            assert_eq!(rows.len(), count);
        }
    }

    #[test]
    fn writes_a_representative_inventory_csv() {
        let mut pages = crate::crawl::indexability_sample_pages();
        pages[0].extraction = std::sync::Arc::new(vec![crate::extract::ExtractionOutcome {
            rule_id: "price".into(),
            state: crate::extract::ExtractionState::Matched,
            values: vec!["$49".into()],
            stage: Some(crate::indexability::EvidenceStage::Document),
            reason: String::new(),
            limited: false,
        }]);
        let audit = Audit::from_pages(&pages);
        let rule = ExtractionRule {
            id: "price".into(),
            name: "Price".into(),
            selector: ".price".into(),
            output: crate::extract::ExtractionOutput::Text,
            attribute: String::new(),
            mode: crate::extract::MatchMode::First,
        };
        let run = InventoryRun {
            seed: "https://example.com/".into(),
            started_utc: "2026-11-17T00:00:00Z".into(),
            page_limit: 40,
            depth_limit: 3,
            sitemaps: true,
            check_links: true,
            outcome: "17 pages on example.com".into(),
            captured_urls: pages.len(),
            incomplete: 2,
            sitemap_incomplete: 2,
            sitemap_file_findings: 1,
            link_evidence_limited: false,
            snapshot: "complete at test".into(),
            extraction_rules: vec![rule],
        };
        let checks = HashMap::new();
        let summary = InventorySummary {
            run: &run,
            link_checks: &checks,
        };
        let mut indices = (0..pages.len()).collect::<Vec<_>>();
        indices.sort_by(|left, right| pages[*left].url.cmp(&pages[*right].url));
        let rows = rows(&pages, &audit, &indices, &summary);
        let headers = headers_with_extraction(&run.extraction_rules);
        let refs = headers.iter().map(String::as_str).collect::<Vec<_>>();
        let csv = crate::export::to_csv(&refs, &rows);
        let dir = tempfile::TempDir::new().expect("unique inventory directory");
        let path = dir.path().join("inventory-sample.csv");
        std::fs::write(&path, csv.as_bytes()).expect("write sample");
        if let Ok(artifact) = std::env::var("CALMTOAD_INVENTORY_ARTIFACT") {
            std::fs::write(artifact, csv.as_bytes()).expect("write requested artifact");
        }
        assert_eq!(
            std::fs::read_to_string(&path).expect("read sample"),
            csv,
            "the fixture is isolated in its own directory"
        );
    }

    #[test]
    fn csv_export_preserves_unicode_newlines_and_formula_protection() {
        let csv = crate::export::to_csv(
            &["TITLE", "H1S"],
            &[vec![
                "=SUM(A1:A2)".into(),
                "line one\nline two — 日本語".into(),
            ]],
        );
        assert!(csv.contains("'=SUM(A1:A2)"));
        assert!(csv.contains("line one\nline two — 日本語"));
    }

    #[test]
    fn headers_are_stable_and_extraction_columns_append() {
        assert_eq!(HEADERS[0], "URL");
        assert_eq!(HEADERS.last(), Some(&"RUN EXTRACTION RULES"));
        let rules = vec![ExtractionRule {
            id: "rule-1".into(),
            name: "Price".into(),
            selector: ".price".into(),
            output: crate::extract::ExtractionOutput::Text,
            attribute: String::new(),
            mode: crate::extract::MatchMode::First,
        }];
        let headers = headers_with_extraction(&rules);
        assert_eq!(headers.len(), HEADERS.len() + 5);
        assert_eq!(headers[HEADERS.len()], "EXTRACT rule-1 NAME");
    }
}
