//! Maintained regressions adapted from the independent review's five focused probes.
//!
//! These intentionally exercise the real crawl/extraction seams rather than parser helpers.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use calmtoad::crawl::{CrawlEvent, CrawlOptions, Fetched, Limits, crawl_site_with, crawl_with};
use calmtoad::extract::{
    self, CompiledRules, ExtractionOutput, ExtractionRule, ExtractionState, MatchMode,
};
use calmtoad::indexability::{EvidenceStage, ReadState};
use calmtoad::links::LinkCheckState;

#[derive(Clone, Copy)]
enum ReviewMode {
    RobotsFailure,
    Redirect,
    SlowRobots,
    RobotsCap,
    Retry,
    RetryDate,
    HugeRetry,
    Dedup,
    IntermediateDedup,
}

struct ReviewFixture {
    seed: String,
    hits: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
}

impl ReviewFixture {
    fn start(mode: ReviewMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(Mutex::new(HashMap::<String, Vec<Instant>>::new()));
        let shared = hits.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let hits = shared.clone();
                thread::spawn(move || serve_review(stream, hits, port, mode));
            }
        });
        Self {
            seed: format!("http://127.0.0.1:{port}/"),
            hits,
        }
    }

    fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).map_or(0, Vec::len)
    }

    fn starts(&self, path: &str) -> Vec<Instant> {
        self.hits
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_default()
    }
}

fn serve_review(
    stream: TcpStream,
    hits: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    port: u16,
    mode: ReviewMode,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut parts = request.split_whitespace();
    let _method = parts.next();
    let path = parts.next().unwrap_or("/").to_string();
    let mut host = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line == "\n" {
            break;
        }
        if line.to_ascii_lowercase().starts_with("host:") {
            host = line
                .split_once(':')
                .map(|(_, value)| value.trim().to_string())
                .unwrap_or_default();
        }
    }
    hits.lock()
        .unwrap()
        .entry(path.clone())
        .or_default()
        .push(Instant::now());
    let external = host.starts_with("localhost");
    let (status, body) = match (path.as_str(), mode) {
        ("/", ReviewMode::Redirect) => {
            (200, format!("<a href='http://localhost:{port}/go'>Go</a>"))
        }
        ("/", ReviewMode::SlowRobots) => (
            200,
            format!("<a href='http://localhost:{port}/slow-target'>Slow</a>"),
        ),
        ("/", ReviewMode::RobotsCap) => (
            200,
            format!("<a href='http://localhost:{port}/blocked'>Blocked</a>"),
        ),
        (
            "/",
            ReviewMode::RobotsFailure
            | ReviewMode::Retry
            | ReviewMode::RetryDate
            | ReviewMode::HugeRetry
            | ReviewMode::Dedup
            | ReviewMode::IntermediateDedup,
        ) => (
            200,
            format!(
                "<a href='http://localhost:{port}/a'>A</a><a href='http://localhost:{port}/b'>B</a>"
            ),
        ),
        ("/robots.txt", ReviewMode::RobotsFailure) if external => (500, "unavailable".into()),
        ("/robots.txt", ReviewMode::SlowRobots) if external => (200, String::new()),
        ("/robots.txt", ReviewMode::RobotsCap) if external => (
            200,
            format!(
                "User-agent: *\n#{}\nDisallow: /blocked\n",
                "x".repeat(262_144)
            ),
        ),
        ("/robots.txt", _) => (200, "User-agent: *\nAllow: /\n".into()),
        ("/go", ReviewMode::Redirect) => (302, String::new()),
        ("/a", ReviewMode::Retry | ReviewMode::RetryDate | ReviewMode::HugeRetry) => {
            (429, "Rate limited".into())
        }
        ("/a" | "/b", ReviewMode::Dedup | ReviewMode::IntermediateDedup) => (302, String::new()),
        ("/common", ReviewMode::IntermediateDedup) => (302, String::new()),
        ("/never", _) => (200, "<html>never</html>".into()),
        ("/blocked", _) => (200, "<html>blocked</html>".into()),
        ("/a" | "/b" | "/common" | "/slow-target", _) => (200, "<html>Target</html>".into()),
        _ => (200, "<html>Target</html>".into()),
    };
    let headers = match (path.as_str(), mode) {
        ("/go", ReviewMode::Redirect) => {
            format!("Location: http://127.0.0.1:{port}/never\r\n")
        }
        ("/a" | "/b", ReviewMode::Dedup | ReviewMode::IntermediateDedup) => {
            "Location: /common\r\n".into()
        }
        ("/common", ReviewMode::IntermediateDedup) => "Location: /terminal\r\n".into(),
        ("/a", ReviewMode::Retry) => "Retry-After: 7\r\n".into(),
        ("/a", ReviewMode::RetryDate) => format!(
            "Retry-After: {}\r\n",
            httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(7))
        ),
        ("/a", ReviewMode::HugeRetry) => "Retry-After: 18446744073709551615\r\n".into(),
        _ => String::new(),
    };
    let mut stream = stream;
    if matches!(mode, ReviewMode::SlowRobots) && external && path == "/robots.txt" {
        // Headers arrive, then the body trickles forever. A read-inactivity timeout alone would
        // never fire; cancellation must interrupt the chunked read.
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.flush();
        for _ in 0..2_000 {
            if stream.write_all(b"U").is_err() {
                return;
            }
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(50));
        }
        return;
    }
    let response = format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: text/html\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn no_stop() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn review_robots_failure_stays_unknown_for_the_whole_origin() {
    let fixture = ReviewFixture::start(ReviewMode::RobotsFailure);
    let outcome = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    let states = outcome.pages[0]
        .link_evidence
        .iter()
        .map(|link| link.check_state())
        .collect::<Vec<_>>();
    assert!(
        states.iter().all(|state| *state == LinkCheckState::Blocked),
        "same robots failure gave {states:?}; hits {:?}",
        fixture.hits.lock().unwrap()
    );
    assert_eq!(fixture.hits("/a"), 0);
    assert_eq!(fixture.hits("/b"), 0);
}

#[test]
fn review_external_redirect_respects_internal_page_budget() {
    let fixture = ReviewFixture::start(ReviewMode::Redirect);
    let _ = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    assert_eq!(
        fixture.hits("/never"),
        0,
        "optional redirect fetched an internal page outside depth/page budget"
    );
}

#[test]
fn review_slow_robots_honors_cancellation_promptly() {
    let fixture = ReviewFixture::start(ReviewMode::SlowRobots);
    let cancel = no_stop();
    let stop = cancel.clone();
    let started = Instant::now();
    let outcome = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 2,
            max_depth: 1,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        cancel,
        move |event| {
            if matches!(event, CrawlEvent::LinkCheckStarted(_)) {
                let stop = stop.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(150));
                    stop.store(true, Ordering::Relaxed);
                });
            }
        },
    );
    assert_eq!(
        outcome.outcome,
        calmtoad::crawl::CrawlOutcome::Stopped,
        "Stop during the final optional check must stop the run: {}",
        outcome.message
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "slow robots delayed cancellation: {:?}",
        started.elapsed()
    );
    assert!(
        outcome
            .pages
            .iter()
            .flat_map(|page| page.link_evidence.iter())
            .any(|link| {
                link.state_reason()
                    .contains("Stopped during optional check")
                    || link.state_reason().contains("deadline")
            }),
        "cancellation was not recorded as a limited check: {}",
        outcome.message
    );
}

#[test]
fn review_oversized_robots_is_not_complete_permission() {
    let fixture = ReviewFixture::start(ReviewMode::RobotsCap);
    let _ = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    assert_eq!(
        fixture.hits("/blocked"),
        0,
        "a truncated robots policy was treated as complete permission"
    );
}

#[test]
fn review_retry_after_delays_the_next_origin_request() {
    let fixture = ReviewFixture::start(ReviewMode::Retry);
    let outcome = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    let a = fixture.starts("/a");
    let b = fixture.starts("/b");
    assert_eq!(a.len(), 1, "the rate-limited destination is requested once");
    assert_eq!(b.len(), 1, "the next destination is requested once");
    let gap = b[0].duration_since(a[0]);
    assert!(
        gap >= Duration::from_secs(7),
        "next same-origin request started after {gap:?}, Retry-After was 7 seconds: {}",
        outcome.message
    );
}

#[test]
fn review_http_date_retry_after_delays_the_next_origin_request() {
    let fixture = ReviewFixture::start(ReviewMode::RetryDate);
    let _ = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    let a = fixture.starts("/a");
    let b = fixture.starts("/b");
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    // HTTP dates have one-second precision; a seven-second date still leaves over five
    // seconds of cooldown even when the response crosses a wall-clock second boundary.
    assert!(b[0].duration_since(a[0]) >= Duration::from_secs(5));
}

#[test]
fn review_unrepresentable_retry_after_never_panics_or_requests_the_next_target() {
    let fixture = ReviewFixture::start(ReviewMode::HugeRetry);
    let outcome = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    assert_eq!(fixture.hits("/a"), 1);
    assert_eq!(fixture.hits("/b"), 0);
    let b = outcome
        .pages
        .iter()
        .flat_map(|page| page.link_evidence.iter())
        .find(|link| link.resolved.ends_with("/b"))
        .expect("second destination evidence");
    assert_eq!(b.check_state(), LinkCheckState::Limited);
    assert!(b.state_reason().contains("Retry-After"));
}

#[test]
fn review_shared_intermediate_redirects_are_requested_once_with_correct_source_chains() {
    let fixture = ReviewFixture::start(ReviewMode::IntermediateDedup);
    let outcome = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    for path in ["/a", "/b", "/common", "/terminal"] {
        assert_eq!(fixture.hits(path), 1, "{path} was not deduplicated");
    }
    for suffix in ["/a", "/b"] {
        let link = outcome
            .pages
            .iter()
            .flat_map(|page| page.link_evidence.iter())
            .find(|link| link.resolved.ends_with(suffix))
            .expect("redirect source evidence");
        assert_eq!(link.redirect_chain().len(), 2);
        assert!(link.redirect_chain()[0].ends_with("/common"));
        assert!(link.redirect_chain()[1].ends_with("/terminal"));
        assert!(link.final_url().ends_with("/terminal"));
    }
}

#[test]
fn review_redirect_destinations_are_checked_once() {
    let fixture = ReviewFixture::start(ReviewMode::Dedup);
    let outcome = crawl_site_with(
        &fixture.seed,
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            check_links: true,
            ..CrawlOptions::default()
        },
        no_stop(),
        |_| {},
    );
    assert_eq!(
        fixture.hits("/common"),
        1,
        "two redirects fetched their common destination repeatedly"
    );
    for suffix in ["/a", "/b"] {
        let link = outcome
            .pages
            .iter()
            .flat_map(|page| page.link_evidence.iter())
            .find(|link| link.resolved.ends_with(suffix))
            .expect("redirect source evidence");
        assert_eq!(link.redirect_chain().len(), 1);
        assert!(link.redirect_chain()[0].ends_with("/common"));
        assert!(link.final_url().ends_with("/common"));
    }
}

#[test]
fn review_evidence_cap_does_not_cut_discovery() {
    let html = format!(
        "{}<a href='/tail'>Tail</a>",
        "<a href='/'>Home</a>".repeat(calmtoad::links::MAX_PAGE_OCCURRENCES)
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
                html.clone()
            },
            read: ReadState::Complete,
            ..Fetched::default()
        },
    );
    assert!(
        out.pages.iter().any(|page| page.url.ends_with("/tail")),
        "optional evidence cap removed ordinary page discovery: {}",
        out.message
    );
}

fn all_attribute_rule() -> ExtractionRule {
    ExtractionRule {
        id: "r1".into(),
        name: "Custom field".into(),
        selector: "a".into(),
        output: ExtractionOutput::Attribute,
        attribute: "data-owner".into(),
        mode: MatchMode::All,
    }
}

#[test]
fn review_truncation_is_not_reported_as_complete() {
    let compiled = CompiledRules::compile(&[ExtractionRule {
        id: "r1".into(),
        name: "Custom field".into(),
        selector: "a".into(),
        output: ExtractionOutput::Text,
        attribute: String::new(),
        mode: MatchMode::All,
    }])
    .expect("compiled");
    let mut budget = extract::MAX_RUN_VALUE_BYTES;
    let html = format!("<a>{}</a>", "x".repeat(5_000));
    let outcomes = extract::evaluate(
        &compiled,
        Some(&html),
        EvidenceStage::Document,
        true,
        false,
        &mut budget,
    );
    assert!(
        outcomes[0].limited,
        "truncated value exported as {:?}, length {}",
        outcomes[0].state,
        outcomes[0].values[0].len()
    );
    assert_ne!(outcomes[0].state, ExtractionState::Matched);
}

#[test]
fn review_all_matches_preserve_later_attributes() {
    let compiled = CompiledRules::compile(&[all_attribute_rule()]).expect("compiled");
    let mut budget = extract::MAX_RUN_VALUE_BYTES;
    let outcomes = extract::evaluate(
        &compiled,
        Some("<a data-owner='first'></a><a></a><a data-owner='last'></a>"),
        EvidenceStage::Document,
        true,
        false,
        &mut budget,
    );
    assert_eq!(
        outcomes[0].values,
        vec!["first", "last"],
        "missing attribute stopped All and reported {:?}",
        outcomes[0].state
    );
    assert_ne!(outcomes[0].state, ExtractionState::Matched);
}
