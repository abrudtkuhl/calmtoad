# Desktop workflow review

Reviewed the implementation against desktop-workflow-optimization.md. November 14, 2026. Combined working tree is still based on HEAD 9031fe8 with prior browser correctness and UI polish preserved. Root reviewed source and rendered workflow/inspector screenshots; independent spec and standards passes reviewed behavior and scaling. Not cleared to ship.

## Spec and behavior

### P1 — Column-header sorting changes the arrow without applying the chosen order

`src/main.rs:3344-3347` updates TableState only. `apply_page_sort` at 1153 reads ViewState.sort instead. The two sources of truth diverge for all eight ordinary page tabs; sitemap callbacks correctly update both.

An actual Rune Runtime click on the Titles length header produced `table_sort=(len, Ascending)` but `view_sort=(issue, Ascending)`. Existing tests set ViewState.sort directly, bypassing the broken callback. Record the requested sort consistently and test actual clicks on URL, numeric and issue columns, both directions, per-view restoration and export order.

### P1 — Findings excludes confirmed problems on incomplete captures

`src/main.rs:1018-1020` returns false for every incomplete page before evaluating a known finding. `sitemap_url_has_finding` at 1091 also requires Checked and complete, dropping relevant blocked/skipped states previously surfaced by needs_attention.

A focused probe retained a confirmed Noindex assessment on an incomplete capture but produced `confirmed=true, finding=false`. Known HTTP errors and observed broken links can be similarly hidden. The new UI must not quietly reduce existing severity semantics. Incompleteness and confirmed observations can coexist; allow overlap or preserve both facts explicitly. Keep withholding completed title/meta/H1/image claims when those checks were not completed. Test retained source/header noindex, observed error status, observed broken target, and relevant sitemap states alongside informative successful non-HTML context.

### P2 — Incoming crawl events undo the user's sort and reorder existing rows

`refresh_visible_inner` at 1179-1205 rebuilds all indices in source arrival order, then `apply_page_sort` at 1150 skips sorting during active work. A focused probe force-sorted by descending URL, appended one page and refreshed; existing rows reverted to arrival order while the sort indicator retained the user's choice. The sitemap refresh path has the same pattern.

Preserve existing visible order when appending during streaming, or consistently maintain the selected sort with stable URL selection. Do not reset old rows as an incidental consequence of a new event. Test successive bursts after a real header click, after a query/filter change, and in both sitemap views; ensure completion uses the intended order without losing selection.

### P2 — Internal CSV exports an unknown/partial word count as a measured value

`src/main.rs:3007` exports page.words unconditionally, while the Internal screen uses auditable gating at 3776. A probe exported `60` for a non-auditable incomplete capture; uncaptured/non-HTML rows can export `0`. Use the same unknown-count rule for screen and CSV, or explicitly qualify a retained partial observation. Test missing, partial and actual known zero separately. Preserve all other CSV fields/escaping and exact filtered order.

### P2 — Full sitemap coverage is still available only on hover

`run_details_panel` at 1799-1846 shows settings, phase/message, partial reason and file-error count but omits listed/checked/blocked/skipped/not-checked/pending totals. Those remain in toolbar help through sitemap_summary at 2612. The toolbar's visual text is truncated at narrow widths and only includes findings/incomplete totals.

Add the complete reconciled coverage breakdown to the click/keyboard-accessible Run panel, including pending discovery versus discovery that did not run or stopped. Keep it concise without adding dashboard cards. Verify that essential coverage can be read without hover at 880px, including a partial report and an active filter.

## Standards and responsiveness — additional findings

### P2 — Deferred count refresh has no reliable wake/redraw

`refresh_counts` at 1378 postpones updates for 100ms but schedules no wake. A later poll can update counts when no new data arrives, yet `poll_crawl` returns only page/sitemap/finished changes and does not include a counts-changed flag. Rune can leave tab counts stale until a separate event on a slow crawl.

Schedule the deferred count refresh and return a visible-change signal when it occurs. Add a deterministic poll/wake test with one event arriving within the throttle window followed by no further page events. Preserve the count cache and avoid reverting to a full scan on every frame.

### P2 — Fixed inspector cutoffs leave retained evidence inaccessible

The H1 inspector at 4210 takes only 50 H1s and reports that more were captured without offering access to them. Incoming sources at 4461 takes 40 and provides only a remaining count, with no pagination, expansion or copy-all. This violates the plan's requirement to expose every captured H1 and provide access beyond bounded relation lists. Outgoing links and duplicate peers at least have copy-all.

Provide bounded pagination/virtualization/expansion or appropriate full-value actions. A broken outgoing destination beyond row 40 should also remain easy to locate rather than being buried after successful links. Do not solve this by rendering tens of thousands of native elements at once. Test more than 50 H1s and more than 40 inbound/outbound entries, including navigation to an item beyond the first page.

## What is working

- Shared start validation, typed run context/phase, search, per-view state, selected-URL identity, contextual inspectors, direct copy/open actions and the Run disclosure are present.
- Rendered screenshots cover the compact interface at supported sizes/themes and show useful contextual evidence. No broad identity redesign or cloud scope was added.
- The production header callback defect and incomplete Findings exclusion escaped tests because several tests directly set read-model state instead of exercising the user action. Fix the seam and test the actual callback path.

## Verification and limitations

- The spec reviewer ran three isolated current-library/Runtime probes. They reproduced an actual header click failing to update effective sort, active streaming order reset, and retained confirmed Noindex being excluded. The incomplete count/export mismatch was also inspected in that probe. Temporary copied probe source was removed; no production source edits were retained.
- Independent source review corroborated those paths and identified count wake/redraw and evidence cutoff issues.
- Root inspected the 880px contact sheet and full duplicate, link and Run inspector captures. There is no claim that Windows runtime or native keyboard menu delivery was independently exercised in this review.
- OpenCode reports 92 library, 40 UI and 25 real-browser tests passing. Those results are useful but do not cover the reproduced action paths above; no duplicate full suite was run merely to reconfirm that report.
- The synthetic measurement named Selection probe at main.rs:7540 times Runtime::find("tab-links"), not selection interaction or inspector rendering. It cannot support claims about duplicate/inbound inspector latency. Measure actual selection + frame construction for representative large duplicate/link datasets and accurately label the result. Per-frame incoming-source scans/peer normalization remain a scaling concern to quantify, not a profiled failure claimed here.

## OpenCode correction pass

Fix all findings and add regressions at the real interaction/read-model boundaries. Use one authoritative sort/check classification seam rather than duplicating strings/state between widgets and view models. Preserve working search/export/copy ordering, incomplete evidence and prior browser fixes. Run relevant pure/UI tests, fmt and Clippy, and update affected measurements/screenshots/docs where necessary. Keep changes local and review-ready; no version bump, commit, push, tag, release or deployment.
