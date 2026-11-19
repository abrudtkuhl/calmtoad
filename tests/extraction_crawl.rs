use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

use calmtoad::crawl::{CrawlOptions, Limits, crawl_site_with};
use calmtoad::extract::{ExtractionOutput, ExtractionRule, ExtractionState, MatchMode};
use calmtoad::render::RenderMode;

const HTML: &str = "<!doctype html><html><head><title>Extraction fixture title long enough</title></head>\
<body><h1>Hello <b>nested</b></h1><a class=\"primary\" href=\"/one\">One</a>\
<a class=\"primary\" href=\"/two\">Two</a><p class=\"price\" data-x=\"\">$49</p>\
<script>document.body.appendChild(document.createTextNode('ignore me'));</script>\
</body></html>";

fn serve(stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut stream = stream;
    let body = if request.contains("/robots.txt") {
        "User-agent: *\nAllow: /\n"
    } else {
        HTML
    };
    let content_type = if request.contains("/robots.txt") {
        "text/plain"
    } else {
        "text/html; charset=utf-8"
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn rule(
    id: &str,
    name: &str,
    selector: &str,
    output: ExtractionOutput,
    attribute: &str,
    mode: MatchMode,
) -> ExtractionRule {
    ExtractionRule {
        id: id.into(),
        name: name.into(),
        selector: selector.into(),
        output,
        attribute: attribute.into(),
        mode,
    }
}

#[test]
fn html_crawl_evaluates_text_attribute_first_all_empty_and_no_match() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            serve(stream);
        }
    });
    let rules = vec![
        rule(
            "heading",
            "Heading",
            "h1",
            ExtractionOutput::Text,
            "",
            MatchMode::First,
        ),
        rule(
            "links",
            "Links",
            "a.primary",
            ExtractionOutput::Attribute,
            "href",
            MatchMode::All,
        ),
        rule(
            "empty",
            "Empty",
            ".price",
            ExtractionOutput::Attribute,
            "data-x",
            MatchMode::First,
        ),
        rule(
            "missing",
            "Missing",
            "section",
            ExtractionOutput::Text,
            "",
            MatchMode::First,
        ),
    ];
    let outcome = crawl_site_with(
        &format!("{base}/"),
        Limits {
            max_pages: 1,
            max_depth: 0,
        },
        CrawlOptions {
            rendering: RenderMode::Html,
            extraction_rules: rules,
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let page = outcome
        .pages
        .first()
        .unwrap_or_else(|| panic!("no page: {}", outcome.message));
    assert_eq!(page.extraction.len(), 4);
    assert_eq!(page.extraction[0].state, ExtractionState::Matched);
    assert_eq!(page.extraction[0].values, vec!["Hello nested"]);
    assert_eq!(page.extraction[1].values, vec!["/one", "/two"]);
    assert_eq!(page.extraction[2].values, vec![""]);
    assert_eq!(page.extraction[3].state, ExtractionState::NoMatch);
    assert!(
        page.extraction
            .iter()
            .all(|outcome| outcome.stage == Some(calmtoad::indexability::EvidenceStage::Document))
    );
}
