# Independent review — desktop audit expansion

November 18, 2026. Baseline `700ab0d` (released v0.6.0), compared to the entire current working tree, including new/untracked modules and integration tests. No implementation fixes or broad test suites in this review. OpenCode's completion report is evidence of its claimed checks, not independent acceptance.

Root reviewed the plan and ran five focused behavioral probes. A separate Spec reviewer reviewed requirements independently. The user-requested calmtoad-eng thread reviewed Standards separately. Review axes are kept separate below.

## Standards

Report from the user-requested calmtoad-eng thread (read-only; no tests), lightly cleaned:

- **P2 — Truncated hrefs can be treated as real destinations.** `src/crawl.rs:2566` truncates hrefs at 8 KiB before the scheduling path at `src/crawl.rs:2054` resolves/fetches them. A long valid URL becomes an altered requested destination, without an occurrence truncation flag. This contradicts PRODUCT.md’s evidence principle, “Do not add a claim the crawl did not earn.” Keep original scheduling URLs separate from bounded display evidence, or explicitly decline overlong URLs without requesting an invented replacement.
- **P2 — Duplicate extraction IDs misattribute exported values.** `src/extract.rs:136` validates names but not ID uniqueness; `src/inventory.rs:183` finds outcomes by ID. Two loaded rules with distinct names and the same ID can export the first rule’s values/state twice. This breaks documented stable rule identity and reliable factual export. Validate IDs before capture/preview and reject malformed persisted duplicates.
- **P3 — CSV fixture test uses a fixed temporary path.** `src/inventory.rs:457` writes `calmtoad-inventory-sample.csv` in the shared temp directory and leaves it behind. Concurrent runs can overwrite one another or an existing file. This is a test-isolation judgement call, not a cited repository-standard violation; use a unique TempDir.

Root confirmed the cited code paths. These three findings were not part of root’s five behavioral probes.

## Spec

### P1 — optional evidence limits truncate ordinary page discovery

`src/crawl.rs:2054`, `src/crawl.rs:2572`: scheduling consumes `parsed.anchors`, which is capped at 10,000, rather than the complete discovery link list. A page with 10,000 home links followed by `/tail`, with a two-page budget and depth one, crawls only home. Its link counts also omit the tail. The existing parser test only checks `parsed.links.len()` and misses the actual scheduling path.

Spec: “A cap on optional evidence must not silently truncate ordinary page discovery.” Root probe failed against the actual engine. Keep discovery independent of retained evidence and add an engine-level regression.

### P1 — robots retrieval failures become permission for later destinations

`src/crawl.rs:1402`: failure caches `None`, the same value used for unrestricted access. Two optional URLs on an origin whose robots returns 500 yield Blocked then Checked; the second target is fetched. Separately, `src/crawl.rs:1324` converts status errors (including 404) into an early return before the intended 4xx branch.

Spec: “Expose failures or refusal as unknown/blocked.” Root loopback probe failed with `[Blocked, Checked]` and one target request. Represent unavailable policy separately from a known absence, and apply the intended status policy consistently.

### P1 — optional redirects bypass internal crawl limits

`src/crawl.rs:1469–1491`: a redirected destination is scheme-checked and compared to already fetched pages, then requested without applying the page crawler's scope/budget rules. An external link redirecting to an unvisited internal page fetches it even at max_pages=1/depth=0.

Spec: “An unchecked internal page remains unchecked if robots/depth/page limits prevented a fetch; the new checker must not bypass those limits.” Root loopback probe failed with one unexpected internal request. Revalidate every redirect hop under its applicable policy.

### P1 — blocking optional requests do not enforce prompt cancellation or total deadlines

`src/crawl.rs:1324`, `src/crawl.rs:1344`, `src/crawl.rs:1391`, `src/crawl.rs:1437`: cancellation is checked between blocking operations; the 30-second check precedes robots retrieval and crawl-delay waiting. Read inactivity timeouts do not bound a robots response that continually trickles bytes. Long Crawl-delay can also run past the chain deadline.

Spec: “Stop/app close interrupts queued and in-flight work…promptly” and “30-second total per-destination chain deadline.” Source-confirmed, not a timed root reproduction. Carry an absolute deadline and cancellation through requests, robots reads and waits.

### P1 — preview workers are missing from close/restart ownership

`src/main.rs:1717`: shutdown cancels only `state.job`, not `extraction_preview_job`. `src/main.rs:1471` also allows updater install while a preview is active. A preview still preparing a browser can race the browser sweep without receiving a cancellation flag.

Spec: preserve “cancellation…owned-process cleanup” and use the same bounded capture lifecycle for previews. Source-confirmed; no claim of a reproduced native process leak. Include previews in shutdown cancellation and restart eligibility, with lifecycle regressions.

### P2 — extraction loses data while reporting Matched

`src/extract.rs:300`: a 5,000-character value is truncated to the 4,096-character cap but reports Matched with limited=false. `src/extract.rs:293–319`: All mode stops at a missing attribute, then overwrites AttributeMissing with Matched if earlier values exist. `[first, missing attribute, last]` returns only `[first]` as Matched.

Spec: “Report per-rule/page/run limits” and distinguish “matched element missing the requested attribute.” Both root probes failed. Retain later valid observations in All mode while reporting missing/limited evidence explicitly; bound text while producing it rather than first collecting unlimited descendant strings.

### P2 — inventory responsiveness and streaming snapshots are not implemented

`src/main.rs:4822` refuses all active runs. `src/main.rs:4836` builds the complete inventory on the UI thread, then `src/main.rs:5473` encodes the entire CSV synchronously before the save dialog. The implementation report itself measures about 1.67 seconds at 50k rows; root did not repeat that measurement.

Spec: “Export…a coherent immutable snapshot…during streaming” and “Large exports must not freeze the UI.” Implement an explicitly labeled snapshot and bounded background projection/writing rather than deferring this requirement.

### P2 — completed preview provenance changes with draft settings

`src/main.rs:3809`, `src/main.rs:3817`: completed preview mode and rule labels read current draft mode/rules. Switching HTML/Browser or renaming/removing a rule relabels old results without another capture; the preview does not retain the rule definitions needed to explain those values.

Spec: a preview must state its “actual capture mode and limitations,” with captured evidence distinct from draft edits. Source-confirmed. Snapshot mode and rule definitions with the preview result, or visibly invalidate the old preview on relevant edits.

## Focused reproduction evidence

Temporary test source is retained outside the repo at `/private/tmp/calmtoad-audit-review-probes.rs`. Root temporarily installed it as `tests/root_audit_review_probe.rs`, ran it, then removed it. No production code changed.

Commands:

- `cargo test --test root_audit_review_probe -- --nocapture` — four tests ran, all four failed on the intended assertions: silent value truncation, early All-attribute termination, robots failure cache, and redirect internal-budget bypass.
- `cargo test --test root_audit_review_probe review_evidence_cap_does_not_cut_discovery -- --exact --nocapture` — one test ran and failed: only one page captured instead of discovering `/tail` after the evidence cap.

The four-test loopback run finished in 0.01 seconds after compilation; the discovery-only engine run in 0.25 seconds. These are defect reproductions, not passing regression coverage. The source should be converted into maintained tests when fixing the defects.

No release approval: resolve the findings and review again before versioning/shipping. Passing existing tests alone does not cover these paths.

## Review totals

Standards: three findings (two P2 correctness issues, one P3 isolation concern); worst within this axis is fetching a URL altered by truncation. Spec: eight findings (five P1 and three P2); worst within this axis includes the confirmed loss of ordinary crawl discovery and policy-bypassing requests. No implementation fixes were made.
