# Desktop workflow correction review

November 15, 2026. Review of OpenCode's corrections to desktop-workflow-review.md, against desktop-workflow-optimization.md. All work remains local on the combined uncommitted tree based on 9031fe8.

Root and calmtoad-eng completed source/interaction review. The explicitly tagged calmtoad-eng2 thread (01a1cbec-e6b0-7f53-bccb-2d15008b6b49) is still waiting on an approval from an earlier turn; the new review was sent there but its current independent report has not run. Do not describe this as both requested reviewers signing off. No release clearance.

## Remaining correctness findings

### P2 — Completed-run reporting reads next-run controls and mutable feedback

`src/main.rs:2733-2735` uses state.use_sitemaps rather than the saved RunContext.sitemaps. sitemap_summary (~2792) and sitemap_empty (~3395) have the same next-run dependency. After a completed Sitemaps-on crawl, unchecking the control for the next crawl changes the existing coverage to “Sitemaps: off for this run,” while the same Run panel still says “Sitemaps on.” Root's isolated probe reproduced:

```
run_sitemaps=true draft=false
before="Sitemaps: 7 files · 9 listed · 6 checked · 1 blocked · 1 skipped · 1 not checked · 3 file errors · partial: ..."
after="Sitemaps: off for this run"
```

Related reporting inconsistencies: run_details_panel at 1915 pairs the retained run phase with state.message, but an invalid next-run start overwrites that message at ~900 with field validation feedback, losing the previous stop/limit outcome. The same panel's noun at ~1916 depends solely on sitemap_view, so returning from Sitemap Files to an ordinary page tab can label page rows as files.

Use the immutable run snapshot and terminal outcome/reason for dataset/reporting state. Keep next-run controls and transient validation/action notices separate. Use the actual active view to label rows. Regression: complete or limit a run, edit next-run Sitemaps/URL/limits, reject an invalid start, and verify old coverage, settings, stop reason and row units remain accurate; no results are lost.

### P2 — Skipped sitemap files still disappear from Findings

`src/main.rs:1136-1141` treats FileState::Skipped as incomplete and then unconditionally excludes incomplete files from Findings. The URL equivalent was fixed, but the file path still suppresses a known issue.

Root probe on the standard report's robots-blocked file produced `needs_attention=true, incomplete=true, finding=false`, with issue “Blocked by robots.txt.” `SitemapFile::needs_attention` at sitemap.rs:162 explicitly considers this state a finding.

Apply the same known-observation/incomplete overlap principle consistently to sitemap files. Keep Pending and guessed Absent informational where appropriate. Test an actual Files → Findings click with a skipped/robots-blocked file; verify counts, visible rows, CSV and Copy visible URLs agree.

## Remaining responsiveness finding

### P2 — Show more is cumulative mounting, not bounded paging

The H1, peer and incoming/outgoing lists compute `limit=(page_count+1)*25` and call take(limit): H1 at main.rs:4355-4369, peers ~4496, incoming ~4648 and outgoing ~4696. Each Show more retains every earlier native row. This fixes access to retained evidence but not the plan's bounded rendering requirement or the reported claim that rendering stays bounded to 25 rows.

Root used a 1,000-H1 fixture and advanced the expansion count to the equivalent of 39 Show more actions. One Rune Runtime frame contained both h1-item-0 and h1-item-999 simultaneously. The temporary probe passed and was removed. This proves cumulative mounting; it does not claim a measured 50k frame time.

Use true windowed pagination, virtualization, or a fixed rendering cap with full-value access and navigation to all entries. Do not regress copy-all or evidence accessibility. Reset paging appropriately when selection/view changes. Verify the mounted list stays bounded after many navigation actions, not only the initial frame or first Show more. Correct documentation/measurements to match actual behavior.

## Verified progress and nonblocking measurement follow-ups

- The shared set_page_sort / set_sitemap_sort seams update the effective view state and widget state together. The new tests exercise actual header clicks rather than only direct state mutation.
- merge_streaming_order preserves existing matching row order before appending arrivals. Confirmed incomplete page observations are no longer blanket-excluded from Findings, and Internal CSV word counts now share the visible table's known/unknown rule.
- Deferred count updates now return change state and schedule the remaining wake interval. Full coverage is present in the Run panel; the saved-run dependency above remains.
- H1/incoming/outgoing copy-all and access beyond the original cutoffs are present. The remaining issue is bounding the mounted list, not retaining the data.
- calmtoad-eng identified per-refresh sitemap formatting/lowercasing and cloning every visible URL in sync_selection (~1168) as avoidable allocations. These are source-based scaling risks, not measured latency regressions in this review. Measure representative sitemap queries and selection refresh before making performance claims or expanding scope.
- Outbound broken-first sorting repeatedly calls linear page_by_url inside the sort key (~4626). Quantify an outbound-heavy inspector, or use a narrow URL/status read index to avoid repeated full-dataset scans. Existing 50k inbound/duplicate measurements do not establish this path's latency.

## Review validation and limits

Root ran two isolated tests from a temporary copied binary: immutable-run coverage/skipped-file classification and the 1,000-H1 rendering probe. Both reproduced the issues above. The copied src/bin/workflow_review_probe_root.rs was deleted; no production source was edited by these probes. git diff --check is clean. Root also inspected updated Run, Findings and H1 inspector screenshots at 880px.

calmtoad-eng's independent pass was source-based, not a duplicate full suite. OpenCode's reported 92 library + 48 UI/bin + 25 browser passing results remain implementation-agent reports; this review does not claim additional Windows runtime or native menu-key validation. Eng2's report is pending its existing approval state.

## OpenCode follow-up

Correct the three remaining groups with focused interaction regressions, preserving all earlier fixes and compact UI. Treat allocation notes as measured follow-ups rather than inventing benchmark results. Run relevant UI/pure tests, formatting and Clippy, and update affected screenshots/docs. Keep local and review-ready; no version bump, commit, push, tag, release or deployment. Additional eng2 findings may follow when that review can run.
