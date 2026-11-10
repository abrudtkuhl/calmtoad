//! Dependency-free performance harness: `cargo bench --bench crawl`.
//!
//! Scenarios cover the costs that repeat while a crawl is live: fetching and parsing pages,
//! rebuilding the audit the window repaints from, adding pages to it incrementally, counting
//! issues, and parsing one document on its own. Each scenario reports the median time per
//! logical operation.
//!
//! ```sh
//! cargo bench --bench crawl
//! cargo bench --bench crawl -- --json
//! cargo bench --bench crawl -- --save-baseline benches/perf-baseline.json
//! cargo bench --bench crawl -- --compare benches/perf-baseline.json --threshold 15
//! ```
//!
//! The comparison is wall-clock based, so it belongs on a stable machine or CI runner. Use a
//! generous threshold when the baseline and the run are on different hardware.

mod support;

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use calmtoad::crawl::{Audit, Limits, counts_with, crawl_with, parse_stats};

const DEFAULT_SAMPLES: usize = 9;
const DEFAULT_WARMUP: usize = 1;
const DEFAULT_THRESHOLD: f64 = 15.0;

struct Options {
    json: bool,
    list: bool,
    samples: usize,
    warmup: usize,
    threshold: f64,
    save_baseline: Option<String>,
    compare: Option<String>,
}

impl Options {
    fn parse() -> Self {
        let mut options = Self {
            json: false,
            list: false,
            samples: DEFAULT_SAMPLES,
            warmup: DEFAULT_WARMUP,
            threshold: DEFAULT_THRESHOLD,
            save_baseline: None,
            compare: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--json" => options.json = true,
                "--list" => options.list = true,
                // Cargo passes this to every bench target; it is not ours.
                "--bench" | "--test" => {}
                "--samples" => options.samples = parse_value(&mut args, &arg),
                "--warmup" => options.warmup = parse_value(&mut args, &arg),
                "--threshold" => options.threshold = parse_value(&mut args, &arg),
                "--save-baseline" => options.save_baseline = Some(value(&mut args, &arg)),
                "--compare" => options.compare = Some(value(&mut args, &arg)),
                "--help" | "-h" => {
                    help();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("unknown argument: {other}");
                    help();
                    std::process::exit(2);
                }
            }
        }
        options.samples = options.samples.max(1);
        options
    }
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next().unwrap_or_else(|| {
        eprintln!("{flag} needs a value");
        std::process::exit(2);
    })
}

fn parse_value<T>(args: &mut impl Iterator<Item = String>, flag: &str) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = value(args, flag);
    raw.parse().unwrap_or_else(|error| {
        eprintln!("invalid value for {flag}: {error}");
        std::process::exit(2);
    })
}

fn help() {
    println!(
        "calmtoad perf harness

Usage: cargo bench --bench crawl -- [options]

Options:
  --json                  print machine-readable JSON on stdout
  --list                  list scenarios without running them
  --samples <n>           timed samples per scenario (default {DEFAULT_SAMPLES})
  --warmup <n>            warmup runs per scenario (default {DEFAULT_WARMUP})
  --save-baseline <path>  write the median times to a baseline file
  --compare <path>        compare against a baseline; exit 1 on regression
  --threshold <pct>       regression threshold in percent (default {DEFAULT_THRESHOLD})"
    );
}

struct Scenario {
    name: &'static str,
    /// The unit of work one invocation performs, for reporting.
    unit: &'static str,
    /// Logical operations in one invocation.
    ops: u64,
    /// Invocations per timed sample.
    reps: u32,
    /// Whether a regression should fail the gate. Multi-threaded work is reported but not gated
    /// because its throughput depends on the runner's core count.
    gated: bool,
    run: Box<dyn Fn()>,
}

struct Metrics {
    name: &'static str,
    unit: &'static str,
    ns_per_op: f64,
    min_ns_per_op: f64,
    p95_ns_per_op: f64,
    ops_per_sec: f64,
    gated: bool,
}

fn scenarios() -> Vec<Scenario> {
    let outcome = support::crawl_300();
    assert_eq!(outcome.pages.len(), 300, "{}", outcome.message);
    let pages = std::sync::Arc::new(outcome.pages);
    let audit_data = Audit::from_pages(pages.as_slice());
    let typical = support::page_body(1);
    let large = support::large_body();

    // A fixed CPU-bound task that lets `--compare` normalize for machine speed.
    let calibration = Scenario {
        name: "calibration",
        unit: "run",
        ops: 1,
        reps: 20,
        gated: false,
        run: Box::new(|| {
            black_box(calibration_work());
        }),
    };

    let crawl = Scenario {
        name: "crawl",
        unit: "page",
        ops: 300,
        reps: 10,
        // Parallel by design; report it, but gate the single-threaded scenarios.
        gated: false,
        run: Box::new(|| {
            let outcome = crawl_with(
                "https://bench.test/",
                Limits {
                    max_pages: 300,
                    max_depth: 4,
                },
                support::synthetic_fetch,
            );
            assert_eq!(outcome.pages.len(), 300, "{}", outcome.message);
            black_box(outcome);
        }),
    };

    let audit = {
        let pages = pages.clone();
        Scenario {
            name: "audit",
            unit: "page",
            ops: 300,
            reps: 60,
            gated: true,
            run: Box::new(move || {
                black_box(Audit::from_pages(black_box(pages.as_slice())));
            }),
        }
    };

    let audit_incremental = {
        let pages = pages.clone();
        Scenario {
            name: "audit_incremental",
            unit: "page",
            ops: 300,
            reps: 60,
            gated: true,
            run: Box::new(move || {
                let mut audit = Audit::default();
                for page in pages.iter() {
                    audit.push(black_box(page));
                }
                black_box(audit);
            }),
        }
    };

    let counts = {
        let pages = pages.clone();
        let audit = audit_data.clone();
        Scenario {
            name: "counts",
            unit: "page",
            ops: 300,
            reps: 120,
            gated: true,
            run: Box::new(move || {
                black_box(counts_with(black_box(pages.as_slice()), black_box(&audit)));
            }),
        }
    };

    let parse_typical = Scenario {
        name: "parse_typical",
        unit: "parse",
        ops: 1,
        reps: 2_000,
        gated: true,
        run: Box::new(move || {
            black_box(parse_stats(black_box(&typical)));
        }),
    };

    let parse_large = Scenario {
        name: "parse_large",
        unit: "parse",
        ops: 1,
        reps: 250,
        gated: true,
        run: Box::new(move || {
            black_box(parse_stats(black_box(&large)));
        }),
    };

    vec![
        calibration,
        crawl,
        audit,
        audit_incremental,
        counts,
        parse_typical,
        parse_large,
    ]
}

/// A deterministic, allocation-free task used only to estimate machine speed.
fn calibration_work() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for i in 0..200_000_u64 {
        hash ^= i.wrapping_mul(0x100_0000_01b3);
        hash = hash.rotate_left(13).wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn measure(scenario: &Scenario, samples: usize, warmup: usize) -> Metrics {
    for _ in 0..warmup {
        (scenario.run)();
    }

    let ops_per_sample = scenario.ops as f64 * scenario.reps as f64;
    let mut per_op = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        for _ in 0..scenario.reps {
            (scenario.run)();
        }
        per_op.push(start.elapsed().as_secs_f64() * 1e9 / ops_per_sample);
    }
    per_op.sort_by(f64::total_cmp);

    let median = per_op[per_op.len() / 2];
    let min = per_op[0];
    let p95 = per_op[(per_op.len() * 95 / 100).min(per_op.len() - 1)];
    Metrics {
        name: scenario.name,
        unit: scenario.unit,
        ns_per_op: median,
        min_ns_per_op: min,
        p95_ns_per_op: p95,
        ops_per_sec: 1e9 / median,
        gated: scenario.gated,
    }
}

fn human(metrics: &[Metrics], samples: usize) {
    println!("calmtoad perf · {samples} samples · median time per operation\n");
    println!(
        "{:<16} {:>11} {:>11} {:>11} {:>18}",
        "scenario", "median", "min", "p95", "throughput"
    );
    for metric in metrics {
        println!(
            "{:<16} {:>11} {:>11} {:>11} {:>18}",
            metric.name,
            duration(metric.ns_per_op),
            duration(metric.min_ns_per_op),
            duration(metric.p95_ns_per_op),
            rate(metric.ops_per_sec, metric.unit),
        );
    }
}

fn json(metrics: &[Metrics], samples: usize) {
    println!("{{");
    println!("  \"version\": 1,");
    println!("  \"samples\": {samples},");
    println!("  \"scenarios\": [");
    for (index, metric) in metrics.iter().enumerate() {
        let comma = if index + 1 == metrics.len() { "" } else { "," };
        println!(
            "    {{\"name\":\"{}\",\"unit\":\"{}\",\"gated\":{},\"ns_per_op\":{:.3},\"min_ns_per_op\":{:.3},\"p95_ns_per_op\":{:.3},\"ops_per_sec\":{:.3}}}{comma}",
            metric.name,
            metric.unit,
            metric.gated,
            metric.ns_per_op,
            metric.min_ns_per_op,
            metric.p95_ns_per_op,
            metric.ops_per_sec,
        );
    }
    println!("  ]");
    println!("}}");
}

fn duration(ns: f64) -> String {
    if ns >= 1e6 {
        format!("{:.2} ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.1} µs", ns / 1e3)
    } else {
        format!("{ns:.0} ns")
    }
}

fn rate(per_second: f64, unit: &str) -> String {
    if per_second >= 1e6 {
        format!("{:.2}M {unit}s/s", per_second / 1e6)
    } else if per_second >= 1e3 {
        format!("{:.1}k {unit}s/s", per_second / 1e3)
    } else {
        format!("{per_second:.0} {unit}s/s")
    }
}

fn save_baseline(path: &str, metrics: &[Metrics]) {
    let mut text = format!(
        "{{\n  \"version\": 2,\n  \"metric\": \"min_ns_per_op\",\n  \"os\": \"{}\",\n  \"arch\": \"{}\",\n  \"unit\": \"ns_per_op\",\n  \"scenarios\": {{\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    for (index, metric) in metrics.iter().enumerate() {
        let comma = if index + 1 == metrics.len() { "" } else { "," };
        text.push_str(&format!(
            "    \"{}\": {:.3}{comma}\n",
            metric.name, metric.min_ns_per_op
        ));
    }
    text.push_str("  }\n}\n");
    std::fs::write(path, text).unwrap_or_else(|error| {
        panic!("could not write {path}: {error}");
    });
    eprintln!("wrote baseline to {path}");
}

/// Compare against a flat `{ "scenarios": { "name": ns_per_op } }` baseline.
fn compare(path: &str, metrics: &[Metrics], threshold: f64) -> bool {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("could not read baseline {path}: {error}");
            return false;
        }
    };
    let baseline = load_baseline(&text);
    if baseline.is_empty() {
        eprintln!("baseline {path} has no scenarios");
        return false;
    }
    if let Some((os, arch)) = baseline_platform(&text)
        && (os != std::env::consts::OS || arch != std::env::consts::ARCH)
    {
        eprintln!(
            "note: baseline was generated on {os}/{arch}; this run is {}/{}. \
             Refresh the baseline on this platform for a tight gate.",
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
    }

    let scale = calibration_scale(&baseline, metrics);
    eprintln!("\nbaseline comparison · threshold {threshold:.0}% · {path}");
    if (scale - 1.0).abs() > 0.01 {
        eprintln!("machine calibration: {scale:.2}x baseline");
    }
    eprintln!(
        "{:<16} {:>11} {:>11} {:>10}  status",
        "scenario", "baseline", "current", "delta"
    );
    let mut regressions = 0;
    for metric in metrics {
        // Calibration only exists to scale the other rows.
        if metric.name == "calibration" {
            continue;
        }
        let Some(&base) = baseline.get(metric.name) else {
            eprintln!(
                "{:<16} {:>11} {:>11} {:>10}  no baseline",
                metric.name,
                "—",
                duration(metric.min_ns_per_op),
                "—"
            );
            continue;
        };
        let expected = base * scale;
        let delta = (metric.min_ns_per_op - expected) / expected * 100.0;
        let status = if !metric.gated {
            "info"
        } else if delta > threshold {
            regressions += 1;
            "REGRESSION"
        } else if delta < -threshold {
            "faster"
        } else {
            "ok"
        };
        eprintln!(
            "{:<16} {:>11} {:>11} {:>+9.1}%  {status}",
            metric.name,
            duration(expected),
            duration(metric.min_ns_per_op),
            delta,
        );
    }

    if regressions > 0 {
        eprintln!("\n{regressions} scenario(s) slower by more than {threshold:.0}%");
        false
    } else {
        eprintln!("\nno scenario slower by more than {threshold:.0}%");
        true
    }
}

/// Scale the baseline by how much slower or faster this machine's calibration run is.
fn calibration_scale(baseline: &BTreeMap<String, f64>, metrics: &[Metrics]) -> f64 {
    let Some(&base) = baseline.get("calibration") else {
        return 1.0;
    };
    let Some(current) = metrics.iter().find(|metric| metric.name == "calibration") else {
        return 1.0;
    };
    if base > 0.0 {
        (current.min_ns_per_op / base).clamp(0.25, 4.0)
    } else {
        1.0
    }
}

/// Read the `os` and `arch` recorded in a baseline, when present.
fn baseline_platform(text: &str) -> Option<(String, String)> {
    fn field(text: &str, name: &str) -> Option<String> {
        let key = format!("\"{name}\"");
        let start = text.find(&key)? + key.len();
        let rest = &text[start..];
        let colon = rest.find(':')?;
        let rest = rest[colon + 1..].trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }
    Some((field(text, "os")?, field(text, "arch")?))
}

/// Minimal reader for the flat baseline format this harness writes. Kept dependency-free.
fn load_baseline(text: &str) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let Some(pos) = text.find("\"scenarios\"") else {
        return out;
    };
    let after = &text[pos + "\"scenarios\"".len()..];
    let Some(open) = after.find('{') else {
        return out;
    };
    let body = &after[open + 1..];
    let Some(close) = body.find('}') else {
        return out;
    };
    let mut cursor = &body[..close];

    while let Some(quote) = cursor.find('"') {
        let rest = &cursor[quote + 1..];
        let Some(end_quote) = rest.find('"') else {
            break;
        };
        let key = &rest[..end_quote];
        let rest = &rest[end_quote + 1..];
        let Some(colon) = rest.find(':') else {
            break;
        };
        let number = rest[colon + 1..].trim_start();
        let end = number
            .find(|ch: char| ch == ',' || ch == '}' || ch.is_whitespace())
            .unwrap_or(number.len());
        if let Ok(value) = number[..end].parse::<f64>() {
            out.insert(key.to_string(), value);
        }
        cursor = &number[end..];
    }
    out
}

fn main() {
    let options = Options::parse();

    if options.list {
        for scenario in scenarios() {
            println!(
                "{} ({}, {} ops/run)",
                scenario.name, scenario.unit, scenario.ops
            );
        }
        return;
    }

    let scenarios = scenarios();
    let metrics: Vec<Metrics> = scenarios
        .iter()
        .map(|scenario| measure(scenario, options.samples, options.warmup))
        .collect();

    if options.json {
        json(&metrics, options.samples);
    } else {
        human(&metrics, options.samples);
    }

    if let Some(path) = &options.save_baseline {
        save_baseline(path, &metrics);
    }

    if let Some(path) = &options.compare
        && !compare(path, &metrics, options.threshold)
    {
        std::process::exit(1);
    }
}
