//! Real-crawl proof that structured-data inspection uses the selected HTML capture once, makes
//! no extra requests, and keeps syntax, findings and coverage separate.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use calmtoad::crawl::{CrawlOptions, Limits, crawl_site_with};
use calmtoad::indexability::EvidenceStage;
use calmtoad::render::RenderMode;
use calmtoad::structured_data::{
    CaptureCompleteness, InspectionCoverage, InventoryState, RootShape, SyntaxOutcome,
};

const MIXED: &str = r##"<!doctype html><html><head>
<title>Structured fixture page title long enough</title>
<meta name="description" content="A fixture description comfortably long enough for the meta check to pass.">
<script type="application/ld+json" id="valid">
{"@context":"https://schema.org","@type":["Organization","LocalBusiness"],
 "@graph":[{"@type":"Person"}],"name":"Example"}
</script>
<script type=' Application/LD+JSON ; profile="x" ' id="duplicate">
{"@type":"Article","headline":"One","headline":"Two"}
</script>
<script type="application/ld+json">{"@type":</script>
<script type="application/ld+json" src="https://example.test/data.json"></script>
<script type="application/ld+jsonx">{"@type":"NotJsonLd"}</script>
<script type="text/javascript">{"@type":"JavaScript"}</script>
</head><body><h1>Structured fixture</h1></body></html>"##;

fn serve(stream: TcpStream, requests: Arc<Mutex<Vec<String>>>, hits: Arc<AtomicUsize>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    if let Some(path) = request.split_whitespace().nth(1) {
        requests.lock().unwrap().push(path.to_string());
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut stream = stream;
    let (content_type, body) = if request.contains("/robots.txt") {
        ("text/plain", "User-agent: *\nAllow: /\n")
    } else if request.contains("/file") {
        ("text/plain", "not html")
    } else {
        hits.fetch_add(1, Ordering::Relaxed);
        ("text/html; charset=utf-8", MIXED)
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn crawl(path: &str) -> (calmtoad::crawl::CrawlResult, Vec<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let hits = Arc::new(AtomicUsize::new(0));
    let requests_for_server = requests.clone();
    let hits_for_server = hits.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            serve(stream, requests_for_server.clone(), hits_for_server.clone());
        }
    });
    let outcome = crawl_site_with(
        &format!("{base}{path}"),
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            rendering: RenderMode::Html,
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let seen = requests.lock().unwrap().clone();
    (outcome, seen)
}

#[test]
fn html_crawl_inventories_mixed_blocks_without_fetching_src_or_near_misses() {
    let (outcome, requests) = crawl("/");
    let page = outcome
        .pages
        .first()
        .unwrap_or_else(|| panic!("no page: {}", outcome.message));
    let report = &page.structured;
    assert_eq!(report.stage, EvidenceStage::Document);
    assert_eq!(report.capture, CaptureCompleteness::Complete);
    assert_eq!(report.inspection, InspectionCoverage::Complete);
    assert_eq!(
        report.blocks_observed, 4,
        "MIME parameters match; ld+jsonx and JS do not"
    );
    assert_eq!(report.blocks_inspected, 4);
    assert_eq!(report.blocks.len(), 4);
    assert_eq!(
        report.confirmed_findings(),
        2,
        "malformed and duplicate-key blocks"
    );

    let valid = &report.blocks[0];
    assert_eq!(valid.script_id.as_ref(), "valid");
    assert_eq!(valid.declared_type.as_ref(), "application/ld+json");
    assert_eq!(valid.syntax, SyntaxOutcome::Parsed);
    assert_eq!(valid.root, RootShape::Object);
    assert_eq!(valid.inventory, InventoryState::Complete);
    assert_eq!(
        report
            .iter_types()
            .map(|item| item.value.as_ref())
            .collect::<Vec<_>>(),
        vec!["Organization", "LocalBusiness", "Person", "Article"]
    );
    assert!(
        report
            .iter_types()
            .any(|item| item.path.as_ref() == "/@graph/0/@type")
    );

    let duplicate = &report.blocks[1];
    assert_eq!(
        duplicate.declared_type.as_ref(),
        " Application/LD+JSON ; profile=\"x\" "
    );
    assert_eq!(duplicate.duplicate_count, 1);
    assert_eq!(duplicate.duplicates[0].key.as_ref(), "headline");

    assert_eq!(report.blocks[2].syntax, SyntaxOutcome::ParseError);
    assert_eq!(report.blocks[3].syntax, SyntaxOutcome::ExternalSource);
    assert_eq!(
        report.blocks[3].src.as_ref(),
        "https://example.test/data.json"
    );

    assert!(
        !requests.iter().any(|path| path.contains("data.json")),
        "external JSON-LD src must never be fetched: {requests:?}"
    );
}

#[test]
fn a_non_html_response_is_unavailable_evidence_not_an_absence_claim() {
    let (outcome, _) = crawl("/file");
    let page = outcome
        .pages
        .first()
        .unwrap_or_else(|| panic!("no page: {}", outcome.message));
    let report = &page.structured;
    assert_eq!(report.capture, CaptureCompleteness::Unavailable);
    assert_eq!(report.inspection, InspectionCoverage::Unavailable);
    assert!(!report.observed_known);
    assert!(!report.can_assert_absence());
    assert_eq!(report.blocks_observed_label(), "—");
    assert!(report.blocks.is_empty());
}

#[test]
#[ignore = "local structured-data inspection measurement"]
fn structured_inspection_measures_10k_and_50k_corpora() {
    use calmtoad::structured_data::StructuredBudget;
    use std::time::Instant;

    let no_markup = scraper::Html::parse_document("<main><h1>No JSON-LD</h1></main>");
    for count in [10_000_usize, 50_000] {
        let budget = StructuredBudget::default();
        let started = Instant::now();
        let mut reports = Vec::with_capacity(count);
        for _ in 0..count {
            reports.push(
                calmtoad::structured_data::Inspection::new(&budget, None).inspect(
                    &no_markup,
                    EvidenceStage::Document,
                    true,
                ),
            );
        }
        let elapsed = started.elapsed();
        let observed = reports
            .iter()
            .filter(|report| report.can_assert_absence())
            .count();
        eprintln!(
            "structured no-markup {count}: {elapsed:?} total, {:?}/page, budget {} bytes, absence {}",
            elapsed / count as u32,
            budget.used(),
            observed
        );
        assert_eq!(observed, count);
    }

    let dense_html = {
        let blocks = (0..40)
            .map(|index| {
                format!(
                    r#"<script type="application/ld+json">{{"@type":["T{index}A","T{index}B","T{index}C"],"name":"n{index}"}}</script>"#
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("<main>{blocks}</main>")
    };
    let dense = scraper::Html::parse_document(&dense_html);
    let count = 10_000_usize;
    let budget = StructuredBudget::default();
    let started = Instant::now();
    let mut retained = 0_usize;
    let mut full_pages = 0_usize;
    let mut limited_pages = 0_usize;
    let mut observed_total = 0_usize;
    for index in 0..count {
        let report = calmtoad::structured_data::Inspection::new(&budget, None).inspect(
            &dense,
            EvidenceStage::Document,
            true,
        );
        retained += report.retained_bytes;
        observed_total += report.blocks_observed;
        if report.blocks_retained == calmtoad::structured_data::MAX_BLOCKS_PER_PAGE {
            full_pages += 1;
        } else {
            limited_pages += 1;
        }
        if index == 0 {
            assert_eq!(report.blocks_observed, 40);
            assert_eq!(
                report.blocks_retained,
                calmtoad::structured_data::MAX_BLOCKS_PER_PAGE
            );
            assert!(report.is_incomplete());
        }
    }
    let elapsed = started.elapsed();
    eprintln!(
        "structured dense {count}: {elapsed:?} total, {:?}/page, budget {} bytes, retained {} bytes, full pages {full_pages}, limited pages {limited_pages}, observed {observed_total}",
        elapsed / count as u32,
        budget.used(),
        retained
    );
    assert_eq!(
        observed_total,
        count * 40,
        "counting continues after retention stops"
    );
    assert!(limited_pages > 0, "the run budget must become visible");
    assert!(budget.used() <= calmtoad::structured_data::STRUCTURED_DATA_BUDGET);
}

#[test]
#[ignore = "local structured-data limit measurement"]
fn structured_inspection_reports_memory_exhaustion_without_growing_records() {
    use calmtoad::structured_data::StructuredBudget;

    let html = (0..64)
        .map(|index| {
            format!(
                r#"<script type="application/ld+json">{{"@type":"T{index}","pad":"{}"}}</script>"#,
                "x".repeat(8 * 1024)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let doc = scraper::Html::parse_document(&html);
    let budget =
        StructuredBudget::with_used(calmtoad::structured_data::STRUCTURED_DATA_BUDGET - 12 * 1024);
    let started = std::time::Instant::now();
    let report = calmtoad::structured_data::Inspection::new(&budget, None).inspect(
        &doc,
        EvidenceStage::Document,
        true,
    );
    let elapsed = started.elapsed();
    eprintln!(
        "structured exhaustion: {elapsed:?}, observed {}, retained {}, inspected {}, budget {}",
        report.blocks_observed,
        report.blocks.len(),
        report.blocks_inspected,
        budget.used()
    );
    assert_eq!(report.blocks_observed, 64);
    assert!(report.blocks.len() < 64);
    assert!(report.blocks_inspected < 64);
    assert!(report.is_incomplete());
    assert!(
        report
            .limitations
            .iter()
            .any(|item| item.contains("budget"))
    );
}

fn crawl_body(
    body: &'static str,
    budget: Option<Arc<calmtoad::structured_data::StructuredBudget>>,
) -> calmtoad::crawl::CrawlResult {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(AtomicUsize::new(0));
    let seen_for_server = seen.clone();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line == "\r\n" || line == "\n" {
                    break;
                }
            }
            let (content_type, payload) = if request.contains("/robots.txt") {
                ("text/plain", "User-agent: *\nAllow: /\n")
            } else {
                seen_for_server.fetch_add(1, Ordering::Relaxed);
                ("text/html; charset=utf-8", body)
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
        }
    });
    crawl_site_with(
        &format!("{base}/"),
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            rendering: RenderMode::Html,
            structured_budget: budget,
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    )
}

#[test]
fn crawl_keeps_page_audits_for_multibyte_paths_and_adversarial_containers() {
    let long = "a".repeat(1_019);
    let body = Box::leak(
        format!(
            r##"<!doctype html><html><head>
<title>Structured adversarial title long enough</title>
<meta name="description" content="A fixture description comfortably long enough for the meta check to pass.">
<script type="application/ld+json">{{"{long}":{{"😀":{{"@type":"Thing"}}}}}}</script>
<script type="application/ld+json">{{"@type":{{"x":1,"x":2}}}}</script>
<script type="application/ld+json">[[]]</script>
<script type="application/ld+json">{{"@id":42}}</script>
</head><body><h1>Adversarial</h1></body></html>"##
        )
        .into_boxed_str(),
    );
    let outcome = crawl_body(body, None);
    let page = outcome
        .pages
        .first()
        .unwrap_or_else(|| panic!("no page: {}", outcome.message));
    assert!(
        page.error.is_empty(),
        "a long multibyte pointer must not panic the crawl worker: {}",
        page.error
    );
    assert_eq!(page.title, "Structured adversarial title long enough");
    assert_eq!(page.h1s, vec!["Adversarial".to_string()]);
    let report = &page.structured;
    assert_eq!(report.blocks_observed, 4);
    assert_eq!(report.blocks[0].syntax, SyntaxOutcome::Parsed);
    assert!(report.blocks[0].types[0].path_truncated);
    assert!(
        report.blocks[0].types[0]
            .path
            .is_char_boundary(report.blocks[0].types[0].path.len())
    );
    assert_eq!(report.blocks[1].syntax, SyntaxOutcome::Parsed);
    assert_eq!(
        report.blocks[1].duplicate_count, 1,
        "non-string @type containers are traversed with duplicate detection"
    );
    assert_eq!(report.blocks[2].syntax, SyntaxOutcome::Parsed);
    assert_eq!(report.blocks[2].inventory, InventoryState::Unsupported);
    assert_eq!(report.non_string_types, 1);
    assert_eq!(report.non_string_ids, 1);
    assert!(
        report
            .limitations
            .iter()
            .any(|item| item.contains("non-string @id"))
    );
    assert_eq!(report.confirmed_findings(), 1);
}

#[test]
fn the_window_supplied_budget_is_the_crawl_ledger() {
    let budget = Arc::new(calmtoad::structured_data::StructuredBudget::default());
    let outcome = crawl_body(
        r##"<!doctype html><html><head><title>Budget fixture title long enough</title></head>
<body><h1>Budget</h1>
<script type="application/ld+json">{"@type":"Thing","name":"Example"}</script>
</body></html>"##,
        Some(budget.clone()),
    );
    let page = outcome
        .pages
        .first()
        .unwrap_or_else(|| panic!("no page: {}", outcome.message));
    assert_eq!(page.structured.blocks_observed, 1);
    assert!(page.structured.retained_bytes > 0);
    assert!(
        budget.used() >= page.structured.retained_bytes,
        "the crawl must reserve evidence in the window-supplied ledger"
    );
    assert!(budget.used() <= calmtoad::structured_data::STRUCTURED_DATA_BUDGET);
}

#[test]
fn a_pre_exhausted_shared_budget_keeps_the_page_and_reports_limited_coverage() {
    let budget = Arc::new(calmtoad::structured_data::StructuredBudget::with_used(
        calmtoad::structured_data::STRUCTURED_DATA_BUDGET,
    ));
    let outcome = crawl_body(
        r##"<!doctype html><html><head><title>Exhausted fixture title long enough</title></head>
<body><h1>Exhausted</h1>
<script type="application/ld+json">{"@type":"Thing","name":"Example"}</script>
</body></html>"##,
        Some(budget.clone()),
    );
    let page = outcome
        .pages
        .first()
        .unwrap_or_else(|| panic!("no page: {}", outcome.message));
    assert_eq!(page.title, "Exhausted fixture title long enough");
    assert!(page.error.is_empty());
    assert_eq!(page.structured.blocks_observed, 1);
    assert!(page.structured.blocks.is_empty());
    assert!(page.structured.is_incomplete());
    assert_eq!(
        budget.used(),
        calmtoad::structured_data::STRUCTURED_DATA_BUDGET
    );
}
