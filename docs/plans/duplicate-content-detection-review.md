# Duplicate content — joint review

November 19, 2026. Reviewed the uncommitted implementation against `5a61a9266db1d62e02f80e491f4d63841033b28f` and the implementation brief. Root, calmtoad-eng2 and an independent spec reviewer participated. **Not ready to ship.** No feature fixes, commits or releases were made.

Supporting reports: [eng2](duplicate-content-detection-eng2-review.md), [UI/export spec review](duplicate-content-detection-spec-review.md). This document consolidates overlaps and adjusts priority for current reachable behavior.

## Spec / correctness and responsiveness

1. **P1 — Hidden/excluded ancestors can supply the selected main content.** `src/content.rs:287` scans every candidate root but `clean_subtree` only checks the root and descendants. A main under a hidden section wins over a visible article. Root reproduced `Some(Main)` with excerpt `hidden wrong content`. Check the full ancestor chain when selecting main/article, including navigation/footer exclusion. Add fixtures for hidden and excluded ancestors.

2. **P1 — Deep differences become false exact matches.** `src/content.rs:341` silently drops descendants after depth 512, then hashes the remainder as eligible complete content. Root reproduced two documents with distinct text at depth 520: both exact eligible, equal hashes, excerpt `shared prefix`. Use bounded iterative traversal or propagate an explicit incomplete-extraction state; never hash silently omitted text as a full capture.

3. **P1 — The 512 MiB budget omits major retained allocations.** `src/content.rs:270` charges shingles/signature plus a constant, but not excerpts (including all short pages), URL maps, exact memberships/copies or LSH indexes (`:712`, `:820`, `:1002`). This fails the brief's explicit total-analysis accounting requirement; near reservations may reach 512 MiB while other retained content data continue growing. Account for real capacities/ownership and construction peaks, report exact versus near coverage loss, and measure peak RSS. The implementation report explicitly acknowledges the narrower accounting; the benchmark counter does not validate the specified cap.

4. **P1 — Result completion can freeze the UI on large memberships.** `ContentAnalysis::similar_affected` (`src/content.rs:685`) inserts every member for every incident pair, called synchronously by `refresh_counts` on result arrival (`src/main.rs:1355`). A 40k-member exact group incident to 200 pairs repeats eight million insertions. Root invoked the actual method on that work shape: **7.38 seconds in debug**; the spec review's equivalent optimized loop took **2.12 seconds**. These are cost probes, not measured production UI timing. Compute distinct affected group IDs and cache/sum disjoint membership counts in the worker. Also budget final result projection/search preparation, not only the first snapshot poll.

5. **P1 — Duplicate CSV preparation blocks the UI.** `src/main.rs:6897` builds all string rows and encodes CSV synchronously; only writing is backgrounded. Exact membership expansion can be much larger than the number of visible groups. The independent optimized projection/actual encoder probe took **90.5 ms for 50k rows / 18 MB**; exact end-to-end callback timing was not measured. Freeze shared result/filter/sort handles and run cancellable projection/encoding off-thread, with lifecycle gates covering preparation through write. Correct the report's claim that encoding is already background work.

6. **P2 — Near search does not partition capture stage/policy.** `src/content.rs:820` keys only band/signature and verification never checks stage or policy. Root reproduced one Document/Rendered pair at **97.4026%** overlap. Current desktop crawls ordinarily use one mode/policy, so this is a module-contract violation rather than a demonstrated ordinary mixed-mode crawl. Partition candidate indexes before caps and verify compatibility defensively; regression-test both stage and policy boundaries.

7. **P2 — Skipped comparison coverage lacks URL-level states.** `src/content.rs:969` only adds extraction-ineligible/short/large/resource cases to Not compared. Eligible inputs skipped after pair/verification limits or cancellation remain labeled `Compared` (`NearState::label`, `:115`) and are absent from the URL-level explanation view. Preserve extraction eligibility separately from analysis progress; record pending/cancelled/search-limited states and affected counts/URLs. Tests should stop during indexing/verification and hit each cap. Approximate candidate retrieval still needs its general caveat even when uncapped.

8. **P2 — Repeated subtree traversal and cancellation gaps.** `src/content.rs:290` cleans every article separately, repeating nested descendants before choosing a winner. Sorting/map creation, exact-group construction and the full band index (`:705`, `:737`, `:820`) also lack cancellation checkpoints. The measured 50k corpus doesn't validate adversarial nested roots/long URLs or every phase. Bound traversal work, check cancellation through substantial phases and test Stop latency there. Avoid relying on the silent depth cutoff to control work.

9. **P2 — Similar search misses nonrepresentative members.** `src/main.rs:2521` indexes representatives/group IDs but not member URLs. For exact group A/B and similar pair A/C, searching B hides its evidence. Add member-aware lookup without copying a huge member list into every incident pair.

10. **P2 — Same-text export reports incorrect per-member scope.** `src/main.rs:6764` writes `group.scope` for all members instead of `member.scope`. Equal text selected via Main on A and Body fallback on B exports B as Main. Export actual member provenance and test mixed-scope exact groups.

The eng2 note's additional 2 MiB exact-input limit concern is not a release blocker on the current capture path: upstream source/rendered HTML is already bounded near 1 MB. Preserve full-text hashing within supported captures; revisit streaming normalization if those capture limits change.

## Standards

No independent actionable documented-standard breach or material style-only finding. The concerns above are behavior/performance defects, not preferences about naming or abstraction.

## Validation

- Root reran **136 library + 86 UI + 16 integration tests = 238 passed** (5 intentionally ignored measurement tests). Initial sandbox run blocked local sockets/settings; rerun with the needed local access passed. Log: `/private/tmp/calmtoad-duplicate-review-tests.log`.
- Root temporary integration probe invoked the real public extractor/analyzer/affected-count method. It reproduced stage mixing, false deep exact matching, hidden-ancestor selection and expensive membership aggregation. Probe moved out of the repository after running: `/private/tmp/calmtoad-duplicate-root-review-probe.rs`; output `/private/tmp/calmtoad-duplicate-root-probe.log`. It uses observations rather than expected-behavior assertions; these cases need permanent regression tests when fixed.
- Reviewed the supplied 880px Similar dark screenshot: controls are present; screenshots do not establish runtime responsiveness or coverage correctness. No full interactive walkthrough or independent cross-platform build was performed this round.
- Independent required-Browser run: **32 passed**, no failures/ignored, 279.52 seconds. Log: `/private/tmp/calmtoad-duplicate-review-browser.log`. Combined with the tests above: **270 passed**. Existing passing tests do not cover the defects reproduced above.

Review totals: **Spec: 10 consolidated findings (5 P1, 5 P2); Standards: 0.** Re-review corrections before shipping.
