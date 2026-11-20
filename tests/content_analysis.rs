use std::sync::atomic::AtomicBool;

use calmtoad::content::{self, ContentInput, NearState};
use calmtoad::crawl::{Fetched, Limits, crawl_with};
use calmtoad::indexability::ReadState;

fn body_text(tokens: &[String]) -> String {
    format!(
        "<html><head><meta name=\"robots\" content=\"noindex\"></head><body><main><h1>Article</h1><p>{}</p></main></body></html>",
        tokens.join(" ")
    )
}

#[test]
fn crawl_content_evidence_groups_exact_and_verifies_near_pairs_without_ineligible_pages() {
    let base = (0..80)
        .map(|index| format!("content-token-{index}"))
        .collect::<Vec<_>>();
    let mut similar = base.clone();
    similar[0] = "different-token".into();
    let short = vec!["too".into(), "short".into(), "here".into()];
    let base_html = body_text(&base);
    let similar_html = body_text(&similar);
    let short_html = body_text(&short);
    let outcome = crawl_with(
        "https://example.test/",
        Limits {
            max_pages: 8,
            max_depth: 1,
        },
        move |url| {
            let (status, body, read) = match url {
                "https://example.test/" => (
                    200,
                    "<main><p>Hub words that are not the same article.</p>\
<a href='/a'>A</a><a href='/b'>B</a><a href='/c'>C</a>\
<a href='/short'>Short</a><a href='/missing'>Missing</a>\
<a href='/truncated'>Truncated</a></main>"
                        .into(),
                    ReadState::Complete,
                ),
                "https://example.test/a" => (200, base_html.clone(), ReadState::Complete),
                "https://example.test/b" => (
                    200,
                    "<main><h1>Article</h1><p>".to_string() + &base.join(" ") + "</p></main>",
                    ReadState::Complete,
                ),
                "https://example.test/c" => (200, similar_html.clone(), ReadState::Complete),
                "https://example.test/short" => (200, short_html.clone(), ReadState::Complete),
                "https://example.test/missing" => {
                    (404, "<p>missing</p>".into(), ReadState::Complete)
                }
                "https://example.test/truncated" => (200, base_html.clone(), ReadState::Truncated),
                _ => (200, "<html></html>".into(), ReadState::Complete),
            };
            Fetched {
                status,
                content_type: "text/html".into(),
                body,
                read,
                ..Fetched::default()
            }
        },
    );
    let page = |path: &str| {
        outcome
            .pages
            .iter()
            .find(|page| page.url.ends_with(path))
            .unwrap_or_else(|| panic!("missing page {path}"))
    };
    assert!(page("/a").content.exact_eligible);
    assert!(page("/b").content.exact_eligible);
    assert!(page("/c").content.exact_eligible);
    assert_eq!(page("/short").content.near_state, NearState::TooShort);
    assert_eq!(page("/missing").content.near_state, NearState::NotEligible);
    assert_eq!(
        page("/truncated").content.near_state,
        NearState::NotEligible
    );
    assert!(!page("/missing").content.exact_eligible);
    assert!(!page("/truncated").content.exact_eligible);

    let inputs = outcome
        .pages
        .iter()
        .map(|page| ContentInput {
            url: page.url.clone(),
            content: page.content.clone(),
        })
        .collect::<Vec<_>>();
    let analysis = content::analyze(inputs, 11, &AtomicBool::new(false));
    assert_eq!(analysis.generation, 11);
    assert_eq!(analysis.groups.len(), 1);
    assert_eq!(analysis.groups[0].members.len(), 2);
    assert_eq!(analysis.groups[0].representative, "https://example.test/a");
    assert!(analysis.pairs.iter().any(|pair| {
        (pair.left.ends_with("/a") && pair.right.ends_with("/c"))
            || (pair.left.ends_with("/c") && pair.right.ends_with("/a"))
    }));
    assert!(
        analysis
            .not_compared
            .iter()
            .any(|item| item.url.ends_with("/short"))
    );
    assert!(
        analysis
            .not_compared
            .iter()
            .any(|item| item.url.ends_with("/missing"))
    );
    assert_eq!(analysis.coverage.exact_eligible, 5);
    assert!(analysis.coverage.exact_groups >= 1);
}
