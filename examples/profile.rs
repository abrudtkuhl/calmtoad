//! A repeatable mixed workload for profiling.
//!
//! ```sh
//! cargo flamegraph --example profile
//! cargo flamegraph --example profile -- parse
//! cargo flamegraph --example profile -- audit --seconds 5
//! ```
//!
//! Modes: `all` (default), `crawl`, `parse`, `audit`, `push`, `counts`. `--seconds` sets how
//! long each phase runs. The workload is synthetic and never touches the network.

#[path = "../benches/support/mod.rs"]
mod support;

use std::hint::black_box;
use std::time::{Duration, Instant};

use calmtoad::crawl::{Audit, Limits, counts_with, crawl_with, parse_stats};

fn main() {
    let mut mode = String::from("all");
    let mut seconds = 2.0_f64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seconds" => {
                seconds = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(seconds);
            }
            other if !other.starts_with('-') => mode = other.to_string(),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let seconds = seconds.max(0.1);

    let outcome = support::crawl_300();
    let pages = outcome.pages;
    let audit = Audit::from_pages(&pages);
    let typical = support::page_body(1);
    let large = support::large_body();

    let phases: &[&str] = match mode.as_str() {
        "all" => &["crawl", "parse", "audit", "push", "counts"],
        "crawl" => &["crawl"],
        "parse" => &["parse"],
        "audit" => &["audit"],
        "push" => &["push"],
        "counts" => &["counts"],
        other => {
            eprintln!("unknown mode: {other} (use all, crawl, parse, audit, push or counts)");
            std::process::exit(2);
        }
    };

    for phase in phases {
        match *phase {
            "crawl" => run_for("crawl", seconds, || {
                black_box(crawl_with(
                    "https://bench.test/",
                    Limits {
                        max_pages: 300,
                        max_depth: 4,
                    },
                    support::synthetic_fetch,
                ));
            }),
            "parse" => run_for("parse", seconds, || {
                black_box(parse_stats(black_box(&typical)));
                black_box(parse_stats(black_box(&large)));
            }),
            "audit" => run_for("audit", seconds, || {
                black_box(Audit::from_pages(black_box(pages.as_slice())));
            }),
            "push" => run_for("push", seconds, || {
                let mut audit = Audit::default();
                for page in &pages {
                    audit.push(black_box(page));
                }
                black_box(audit);
            }),
            "counts" => run_for("counts", seconds, || {
                black_box(counts_with(black_box(pages.as_slice()), black_box(&audit)));
            }),
            _ => unreachable!(),
        }
    }
}

fn run_for<F: FnMut()>(label: &str, seconds: f64, mut work: F) {
    eprintln!("profiling {label} for {seconds:.1}s");
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut iterations = 0_u64;
    while Instant::now() < deadline {
        work();
        iterations += 1;
    }
    eprintln!("{label}: {iterations} iterations");
}
