# Duplicate-content spec review

Reviewed the uncommitted implementation against `5a61a9266db1d62e02f80e491f4d63841033b28f`, including new `src/content.rs` and `tests/content_analysis.rs`, against `duplicate-content-detection.md`. No implementation edits or release actions. Findings below are spec correctness/responsiveness issues, not stylistic preferences.

## Findings

### P1 — CSV projection and encoding block the UI and duplicate the export in memory

`src/main.rs:6897–6905`, especially `content_export(state)` and `export::to_csv`, execute directly in the Export callback. `content_export` expands every exact-group member into a `Vec<String>` row (`6756–6774`), repeatedly cloning group/context/hash strings, then `to_csv` allocates another full string. `save_prepared_csv(..., true, ...)` backgrounds only the eventual file write, after all this work. A 50k-member exact group expands into 50k rows before the dialog opens, with no cancellation opportunity or bounded UI step. The implementation report's statement that projection/encoding are background work is incorrect for this feature.

Spec: “Account for ... export copies”; “No ... UI-blocking ... work”; validation targets “<8ms bounded UI polling/preparation”. Fix by freezing lightweight result/filter/sort handles, then project/encode in cancellable bounded background work, retaining the existing atomic writer/lifecycle gates. Benchmark export triggering and complete projection, not only the content-analysis snapshot trigger.

### P1 — Overview affected-URL aggregation repeats huge memberships on the UI thread

`src/content.rs:685–701` rebuilds a URL set by visiting both exact memberships for every near pair. `src/main.rs:1355` invokes this through `refresh_counts(..., true)` synchronously when the result arrives. One 40k-member group with 200 similar representative pairs requires 8 million repeated member insertions, despite only ~40k unique URLs. This adversarial shape is allowed by the candidate cap and absent from the reported benchmark (which has zero exact groups). Index/search rebuilding immediately afterward is also synchronous; the benchmark measures first snapshot preparation, not completion processing.

Spec: “Count unique affected URLs”; “Cross-page analysis runs off the UI thread”; “<8ms bounded UI polling/preparation”. Calculate distinct affected group IDs and membership counts once in analysis, cache the result, and measure the final-result poll on a giant-group/many-pairs fixture.

### P2 — Searching a nonrepresentative member hides its similar-content evidence

`src/main.rs:2521–2534` constructs Similar search text from representatives, group IDs, stage and scope only. Unlike Same text, it never includes group-member URLs. For exact group `[A,B]` with representative A and near pair A/C, searching B produces no Similar row even though the inspector and affected count include B. Filtering then exporting also omits the evidence the user sought.

Spec: “Search includes both URLs and group members.” Add member-aware lookup/indexing without copying a huge membership list into every incident pair. Regression: a nonrepresentative exact member query should retain its pair, while an unrelated URL should not.

### P2 — Same-text CSV reports the representative's extraction scope for every member

`src/main.rs:6764–6767` writes `group.scope` while building each member's CSV row, although `ExactMember.scope` stores that member's actual provenance. Equal normalized text selected via `<main>` on A and body fallback on B legitimately groups, but B exports as Main. This falsely removes the body-fallback caution from exported evidence.

Spec: “record each page's extraction scope even when scopes differ”; exports include “capture stage ... scope”. Export `member.scope`, and test a mixed-main/body exact group. The group-level inspector may continue labeling its representative separately.

## Validation limits

No full Cargo suite was duplicated because root owns it. A standalone optimized Rust probe (`/private/tmp/calmtoad-duplicate-review-probe.rs`, built with `rustc -O`) executed the same repeated-membership insertion shape and imported the actual `src/export.rs` encoder. On this machine, 40k members ×200 pair repetitions took **2.116s**; constructing 50k thirteen-column rows and encoding an 18,050,012-byte CSV took **90.5ms**. These are targeted cost probes, not full-UI end-to-end timings or exact `content_export` benchmarks; the call graph places these work shapes synchronously on the UI. Root/eng2 independently cover extraction, stage isolation, caps and cancellation. Requirements around measured peak RSS and final-result poll time remain unvalidated by the implementation report, which explicitly reports an allocation counter rather than peak process memory and only the first snapshot poll.
