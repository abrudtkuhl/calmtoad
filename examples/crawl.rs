//! A headless crawl, useful for timing and for checking robots handling without the window.
//!
//! `cargo run --release --example crawl -- https://example.com/`

use std::time::Instant;

use calmtoad::crawl::crawl_site;

fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://example.com/".to_string());
    let start = Instant::now();
    let outcome = crawl_site(&url);
    let elapsed = start.elapsed();
    println!("{} in {elapsed:?}", outcome.message);
    for page in outcome.pages.iter().take(10) {
        println!(
            "{:>3} {:>4}  {}  {}",
            page.status, page.words, page.url, page.title
        );
    }
}
