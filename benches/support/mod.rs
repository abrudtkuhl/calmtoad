//! Synthetic pages shared by `benches/crawl.rs` and `examples/profile.rs`.
//!
//! The fetcher builds each page from its URL, so the workload is deterministic and never
//! touches the network.

#![allow(dead_code)]

use calmtoad::crawl::{Fetched, Limits, Outcome, crawl_with};

pub fn page_body(n: usize) -> String {
    let links: String = (1..=7)
        .map(|k| format!("<a href=\"/p{}\">next</a>", n * 7 + k))
        .collect();
    format!(
        r#"<!doctype html><html><head>
<title>Benchmark page {n} title that is comfortably long</title>
<meta name="description" content="A benchmark description that is long enough to look like a real one on a page.">
<link rel="canonical" href="/p{n}">
</head><body><h1>Benchmark page {n}</h1>
<p>{}</p>
<img src="/a.png" alt="A"><img src="/b.png">
{links}</body></html>"#,
        "word ".repeat(40)
    )
}

/// A body closer to a real article: many paragraphs, links and images to traverse.
pub fn large_body() -> String {
    let mut body = String::from(
        r#"<!doctype html><html><head>
<title>Large benchmark page with a comfortably long title</title>
<meta name="description" content="A benchmark description that is long enough to look like a real one on a page.">
<link rel="canonical" href="/large">
</head><body><h1>Large benchmark page</h1>"#,
    );
    for i in 0..400 {
        body.push_str(&format!(
            "<p id=\"p{i}\">{}<a href=\"/p{i}\">next</a><img src=\"/a{i}.png\" alt=\"A\"><img src=\"/b{i}.png\"></p>",
            "word ".repeat(20)
        ));
    }
    body.push_str("</body></html>");
    body
}

pub fn synthetic_fetch(url: &str) -> Fetched {
    if url.ends_with("/robots.txt") {
        return Fetched {
            status: 404,
            ..Fetched::default()
        };
    }
    let n = url
        .rsplit('/')
        .next()
        .and_then(|part| part.strip_prefix('p'))
        .and_then(|part| part.parse::<usize>().ok())
        .unwrap_or(0);
    Fetched {
        status: 200,
        content_type: "text/html; charset=utf-8".into(),
        body: page_body(n),
        ..Fetched::default()
    }
}

pub fn crawl_300() -> Outcome {
    crawl_with(
        "https://bench.test/",
        Limits {
            max_pages: 300,
            max_depth: 4,
        },
        synthetic_fetch,
    )
}
