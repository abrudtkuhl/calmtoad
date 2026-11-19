use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use calmtoad::crawl::{CrawlOptions, Limits, crawl_site_with};
use calmtoad::links::LinkCheckState;
use calmtoad::render::RenderMode;

struct Fixture {
    base: String,
    external: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("0.0.0.0:0").expect("bind fixture");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let shared = hits.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let hits = shared.clone();
                thread::spawn(move || serve(stream, hits));
            }
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            external: format!("http://localhost:{port}"),
            hits,
        }
    }

    fn hits(&self, host: &str, path: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .get(&format!("{host}{path}"))
            .copied()
            .unwrap_or(0)
    }
}

fn serve(stream: TcpStream, hits: Arc<Mutex<HashMap<String, usize>>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut parts = request.split_whitespace();
    let _method = parts.next();
    let path = parts.next().unwrap_or("/").to_string();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line == "\n" {
            break;
        }
    }
    // The Host header is in the drained headers; this fixture uses path-only counts for the
    // external host and seed host, which is enough to prove no duplicate requests.
    hits.lock()
        .unwrap()
        .entry(path.clone())
        .and_modify(|n| *n += 1)
        .or_insert(1);
    let mut stream = stream;
    let body = match path.as_str() {
        "/" => "<!doctype html><html><head><base href=\"/docs/\"></head><body>\
<a href=\"/pricing\">Pricing</a>\
<a href=\"/pricing\">Pricing again</a>\
<a href=\"guide\">Guide</a>\
<a href=\"#section\">Section</a>\
<a href=\"mailto:team@example.com\">Mail</a>\
<a href=\"/blocked\">Blocked</a>\
<a href=\"/file.pdf\">PDF</a>\
<a href=\"http://localhost:PORT/external\">External</a>\
<a href=\"http://localhost:PORT/redirect\">Redirect</a>\
<a href=\"http://localhost:PORT/loop\">Loop</a>\
<a href=\"http://localhost:PORT/rate\">Rate</a>\
<a href=\"http://localhost:PORT/forbidden\">Forbidden</a>\
<a href=\"http://user:pass@localhost:PORT/secret\">Credentials</a>\
<a href=\"http://localhost:PORT/external-blocked\">Blocked external</a>\
<a href=\"/logo-link\"><img src=\"/logo.png\" alt=\"Logo\"></a>\
</body></html>"
            .replace("PORT", &stream.local_addr().unwrap().port().to_string()),
        "/robots.txt" => "User-agent: *\nDisallow: /blocked\nDisallow: /external-blocked\nAllow: /\n".into(),
        "/pricing" => "<!doctype html><html><head><title>Pricing page title long enough</title></head><body><h1>Pricing</h1><a href=\"/deeper\">Deeper</a></body></html>".into(),
        "/docs/guide" => "<!doctype html><html><head><title>Guide page title long enough</title></head><body><h1>Guide</h1></body></html>".into(),
        "/file.pdf" => "%PDF-1.4\n".into(),
        "/external" => "not found".into(),
        "/redirect" => String::new(),
        "/final" => "final".into(),
        "/loop" => String::new(),
        "/rate" => "rate".into(),
        "/forbidden" => "forbidden".into(),
        _ => "not found".into(),
    };
    let location = match path.as_str() {
        "/redirect" => Some("/final"),
        "/loop" => Some("/loop"),
        _ => None,
    };
    let status = match path.as_str() {
        "/external" | "/missing" => 404,
        "/redirect" | "/loop" => 302,
        "/rate" => 429,
        "/forbidden" => 403,
        _ => 200,
    };
    let content_type = if path.ends_with(".pdf") {
        "application/pdf"
    } else if path.ends_with(".txt") {
        "text/plain"
    } else {
        "text/html; charset=utf-8"
    };
    let location_header = location
        .map(|target| format!("Location: {target}\r\n"))
        .unwrap_or_default();
    let retry_after = if path == "/rate" {
        "Retry-After: 7\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {status} Status\r\nContent-Type: {content_type}\r\n{location_header}{retry_after}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

#[test]
fn evidence_keeps_repeated_anchors_skips_and_checks_only_observed_destinations() {
    let fixture = Fixture::start();
    let external = fixture.external.clone();
    let outcome = crawl_site_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 5,
            max_depth: 1,
        },
        CrawlOptions {
            rendering: RenderMode::Html,
            check_links: true,
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert!(!outcome.pages.is_empty(), "{}", outcome.message);
    let home = outcome
        .pages
        .iter()
        .find(|page| page.url == format!("{}/", fixture.base))
        .expect("home page");
    let pricing: Vec<_> = home
        .link_evidence
        .iter()
        .filter(|link| link.resolved.ends_with("/pricing"))
        .collect();
    assert_eq!(
        pricing.len(),
        2,
        "duplicate anchors stay as separate evidence"
    );
    assert_ne!(pricing[0].order, pricing[1].order);
    assert!(
        home.link_evidence
            .iter()
            .any(|link| link.resolved.contains("/docs/guide"))
    );
    assert!(
        home.link_evidence
            .iter()
            .any(|link| link.check_state() == LinkCheckState::Fragment)
    );
    assert!(
        home.link_evidence
            .iter()
            .any(|link| link.check_state() == LinkCheckState::Invalid)
    );
    assert!(home.link_evidence.iter().any(|link| {
        link.check_state() == LinkCheckState::Invalid
            && link.state_reason().contains("Embedded credentials")
    }));
    assert!(
        home.link_evidence
            .iter()
            .any(|link| link.check_state() == LinkCheckState::Blocked)
    );
    let deeper = outcome
        .pages
        .iter()
        .flat_map(|page| page.link_evidence.iter())
        .find(|link| link.resolved.ends_with("/deeper"))
        .expect("depth-limited evidence");
    assert_eq!(deeper.check_state(), LinkCheckState::Limited);
    assert!(home.link_evidence.iter().any(|link| link.anchor.is_empty()));
    assert_eq!(fixture.hits("", "/file.pdf"), 1);
    assert_eq!(fixture.hits("", "/external"), 1);
    let pdf = home
        .link_evidence
        .iter()
        .find(|link| link.resolved.ends_with("/file.pdf"))
        .expect("pdf evidence");
    assert_eq!(pdf.check_state(), LinkCheckState::Checked);
    let ext = home
        .link_evidence
        .iter()
        .find(|link| link.resolved.starts_with(&external))
        .expect("external evidence");
    assert_eq!(ext.check_state(), LinkCheckState::Checked);
    assert!(
        ext.is_finding(),
        "the 404 external destination is a finding"
    );
    let redirect = home
        .link_evidence
        .iter()
        .find(|link| link.resolved.ends_with("/redirect"))
        .expect("redirect evidence");
    assert_eq!(redirect.check_state(), LinkCheckState::Checked);
    assert_eq!(redirect.redirect_chain().len(), 1);
    assert!(redirect.final_url().ends_with("/final"));
    let loop_link = home
        .link_evidence
        .iter()
        .find(|link| link.resolved.ends_with("/loop"))
        .expect("loop evidence");
    assert_eq!(loop_link.check_state(), LinkCheckState::Limited);
    assert!(loop_link.state_reason().contains("loop"));
    let rate = home
        .link_evidence
        .iter()
        .find(|link| link.resolved.ends_with("/rate"))
        .expect("rate evidence");
    assert_eq!(rate.check_state(), LinkCheckState::RateLimited);
    assert!(
        rate.state_reason().contains("Retry-After: 7"),
        "rate limiting keeps the advisory header without retrying: {}",
        rate.state_reason()
    );
    let forbidden = home
        .link_evidence
        .iter()
        .find(|link| link.resolved.ends_with("/forbidden"))
        .expect("forbidden evidence");
    assert_eq!(forbidden.check_state(), LinkCheckState::Restricted);
    let blocked_external = home
        .link_evidence
        .iter()
        .find(|link| link.resolved.ends_with("/external-blocked"))
        .expect("robots-blocked external evidence");
    assert_eq!(blocked_external.check_state(), LinkCheckState::Blocked);
    assert!(
        fixture.hits("", "/external") <= 1,
        "no recursive external discovery"
    );
    assert_eq!(fixture.hits("", "/pricing"), 1, "page evidence is reused");
    assert_eq!(
        fixture.hits("", "/final"),
        1,
        "a followed redirect is requested once"
    );
    assert!(
        outcome
            .pages
            .iter()
            .flat_map(|page| page.link_evidence.iter())
            .all(|link| link.resolved.len() <= calmtoad::links::MAX_URL_BYTES)
    );
}

#[test]
fn checking_off_preserves_existing_network_behavior_and_marks_external_unchecked() {
    let fixture = Fixture::start();
    let outcome = crawl_site_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 2,
            max_depth: 1,
        },
        CrawlOptions {
            rendering: RenderMode::Html,
            check_links: false,
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let home = outcome
        .pages
        .iter()
        .find(|page| page.url == format!("{}/", fixture.base))
        .expect("home page");
    assert_eq!(fixture.hits("", "/file.pdf"), 0);
    assert_eq!(fixture.hits("", "/external"), 0);
    assert!(home.link_evidence.iter().any(|link| {
        link.resolved.contains("/file.pdf")
            && link
                .state_reason()
                .contains("Document download not checked")
    }));
    assert!(home.link_evidence.iter().any(|link| {
        link.resolved.contains("localhost") && link.state_reason().contains("checking is off")
    }));
}
