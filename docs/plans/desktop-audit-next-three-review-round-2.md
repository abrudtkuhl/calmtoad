# Independent review — round 2

November 18, 2026. Reviewed the current corrected working tree against released v0.6.0 (`700ab0d`), including untracked modules/tests. Sources: original three-stage plan, first independent review, and OpenCode's finding-by-finding correction report. Root checked behavior, calmtoad-eng reviewed Standards, and a separate reviewer checked Spec. No production fixes, commits or release actions in this review.

## Verification of corrections

Root independently ran:

- All five original reproductions, unchanged from `/private/tmp/calmtoad-audit-review-probes.rs`: **5 passed**. The first review had all five failing.
- `cargo test --test review_regressions -- --nocapture`: **6 passed**.
- `cargo test --bin calmtoad inventory_ -- --nocapture`: **4 passed**.
- `cargo test --bin calmtoad preview -- --nocapture`: **4 passed**.

The discovery tail, robots-failure caching, internal redirect-budget bypass, silent extraction truncation, early All-mode termination, preview lifecycle/provenance, and streaming export paths have concrete corrections. calmtoad-eng confirmed its original three Standards findings closed: altered long-URL requests, duplicate extraction IDs, and fixed temporary CSV path. Inventory responsiveness is improved by moving projection/encoding off-thread, but snapshot preparation and writer ownership still need work below. Do not interpret passing existing regressions as complete acceptance.

## Standards — calmtoad-eng

### P2 — inventory snapshot preparation still blocks the UI and copies unnecessary data

`src/main.rs:4961`, `src/main.rs:4965`, `src/main.rs:5020`: before spawning its worker, export sorts selected indices and clones all pages, the audit and link checks synchronously. Even Visible Internal URLs copies every captured Page. The implementation report measures about 78 ms for a relatively light 50k-page clone, excluding potentially much heavier retained link/extraction evidence. Root did not repeat this measurement. This is a remaining responsiveness concern under the inventory plan; projection/CSV encoding moving to a worker does not eliminate UI snapshot work.

Avoid copying unrelated full evidence or move to a coherent shared snapshot representation. Measure the actual UI trigger with realistic link/extraction-heavy pages and a small visible subset; do not silently weaken snapshot correctness.

### P2 — background CSV writer is detached from install and close ownership

`src/main.rs:1559`, `src/main.rs:1808`, `src/main.rs:5694`: can_install checks the preparation job but not inventory_write. The final std::fs::write worker has neither cancellation nor completion ownership on close/restart. Updating or closing during a large write may terminate the process with the export only partially written. The plan requires safe cancellation and updates blocked until workers stop.

Track file-write lifecycle as well as preparation, block install until it finishes, and ensure interrupted writes do not replace a destination with an apparently finished partial file. Source-confirmed; no timed process-kill reproduction was performed.

## Spec — independent reviewer, with root probes

### P1 — oversized robots policies are treated as complete permission

`src/crawl.rs:1385` breaks at the 256 KiB retention cap, returns the prefix as success, and parses/caches it as a complete policy at `src/crawl.rs:1455`. Root served User-agent: * followed by a long comment and then Disallow: /blocked. The optional checker fetched /blocked because the Disallow tail was omitted.

Spec: “Expose failures or refusal as unknown/blocked.” Detect oversize explicitly (read a bounded extra byte), preserve unknown/limited policy, and do not contact targets on the basis of a truncated allow-looking prefix. Root probe failed: target request count **1**, expected **0**.

### P2 — Retry-After is displayed but not enforced for the origin

`src/crawl.rs:1477` only adds header text to a reason. Scheduling at `src/crawl.rs:1551` uses robots delay or a one-second minimum, without an origin-level Retry-After deadline. Root returned HTTP 429 with Retry-After: 7 for /a; /b on the same origin was requested **1.005 seconds** later.

Spec: “honoring a longer existing crawl delay or Retry-After.” Apply an origin cooldown to later queued work (or leave it unchecked if the deadline prevents a wait); continue to honor cancellation and bounded total deadlines. Root probe failed.

### P2 — shared redirect destinations are fetched repeatedly

`src/crawl.rs:2520` caches a completed check under its starting key only. Redirect reuse consults checked results, but no result is cached for the actual terminal URL. Root's observed links /a and /b both redirected to /common; /common was requested **twice**, not once.

Spec: “deduplicate destination requests” and “Redirect targets already known reuse observations.” Cache each observed response under its actual URL and preserve the caller-specific chain when reusing it. Do not consume the budget again for a known terminal response. Root probe failed.

### P2 — Stop during the final optional check reports Completed

`src/crawl.rs:2500` only sets check_stopped before an iteration. Cancellation during its final request returns a Limited check, but there is no next iteration to mark the run stopped. `src/crawl.rs:2535` then returns Completed. Root strengthened the existing slow-robots test with a CrawlOutcome assertion: it returned **Completed**, with “1 pages … link checks incomplete,” instead of **Stopped**.

Spec: preserve the run's terminal outcome. Record cancellation immediately after a check/before finalization rather than relying on the next iteration. Root probe failed; the maintained test currently checks the link reason but misses run outcome.

### P2 — Browser source plus rendered evidence can exceed the page cap

`src/crawl.rs:2289` appends source anchors after rendered anchors. Each stage retains up to 10,000 anchors, but the combined page collection is only compared against the run cap afterward (`src/crawl.rs:2303`). A Browser page can therefore retain 20,000 occurrences despite the stated page limit.

Spec: “10,000 occurrences per page.” Apply a combined evidence budget across retained stages, mark omissions, and keep uncapped ordinary discovery independent. Stage labels themselves correctly distinguish the documents. Source-confirmed; no new full-browser cap reproduction was run.

## New reproduction artifacts and cleanup

- `/private/tmp/calmtoad-audit-round2-probes.rs`: three loopback tests for oversized robots, Retry-After and shared redirect targets. `cargo test --test root_audit_round2_probe -- --nocapture` ran **3 tests, all failed** on the intended assertions; runtime about 3.02 seconds after compilation.
- `/private/tmp/calmtoad-audit-round2-stop-probe.rs`: copy of maintained review regressions with an added CrawlOutcome::Stopped assertion. `cargo test --test root_stop_outcome_review review_slow_robots_honors_cancellation_promptly -- --exact --nocapture` ran **1 test, failed**; runtime about 0.17 seconds.

All temporary test files were removed from the repository after execution. The above source copies are retained outside it for repair/regression use. No broad suite or native Windows/Linux UI walkthrough was repeated; OpenCode's prior full-check report remains separate from these independent checks.

## Totals and disposition

Standards: **2 P2 findings**, worst within this axis is unowned file writing during close/update. Spec: **1 P1 and 4 P2 findings**, worst within this axis is requesting targets using truncated robots policy as permission. Still not ready to ship. Fix these focused items and review their regressions before exact-commit CI/release work.
