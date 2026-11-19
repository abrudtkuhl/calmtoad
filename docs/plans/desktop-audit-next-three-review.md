# Desktop audit expansion review

November 18, 2026. Local implementation and correction pass against released v0.6.0 (`700ab0d`). All three requested features remain integrated in the working tree. No commit, push, tag, release, version bump, or deployment was performed; root and calmtoad-eng review again before shipping.

The independent findings in `docs/plans/desktop-audit-next-three-independent-review.md` are preserved unchanged. This document records how each one was resolved and the evidence that now guards it.

## Round 2 resolution map

The second independent review (`docs/plans/desktop-audit-next-three-review-round-2.md`) is preserved unchanged. Its seven findings are resolved as follows.

| # | Round 2 finding | Resolution | Maintained evidence |
| --- | --- | --- | --- |
| S1 | **Standards P2 — inventory snapshot preparation blocks the UI and copies unnecessary data.** Sorting, full page copies, audit, and link-check copies happened before the worker; Visible copied every page. | `Page` shares its heavy `links`, `link_evidence`, and `extraction` collections through immutable `Arc` values, so page cloning copies metadata and references, not evidence. All-scope URL sorting moved into the CSV worker. Visible clones only the selected pages. Preparation is now a chunked state machine: at most 512 pages are cloned per UI poll, then the coherent snapshot is handed to the worker; a `pages_epoch` guard aborts instead of mixing results if `set_pages` replaces the crawl. | `tests::inventory_export_snapshots_an_active_run_and_builds_off_the_ui_thread` (Visible subset), `tests::inventory_snapshot_preparation_is_chunked_and_coherent` (512-page chunks), `tests::inventory_trigger_latency_with_heavy_shared_evidence` (measurement). Measured trigger: **236 µs All / 1.30 ms small Visible** with 2,000 pages, 500,000 link occurrences, and 20,000 extraction outcomes. Direct full snapshot without chunking measured 5.07 ms All and 2.36 ms Visible; 50,000 light pages measured 59.6 ms for a direct page copy, now spread across polls with a sub-millisecond trigger. |
| S2 | **Standards P2 — background CSV writer detached from install/close ownership.** | Inventory export tracks preparation, dialog, and write states. `can_install` requires preparation, dialog, and write completion. Writes go to a temporary file in the destination directory, check cancellation per 1 MiB chunk, and only replace the destination after flush. Close/update cancellation removes the temporary file; an existing destination stays untouched. Dialog callbacks clear `inventory_dialog_open`, and starting a crawl refuses while inventory work is active. | `tests::inventory_writer_is_safe_and_gates_install_until_finished` covers complete write, cancelled write preserving the original destination, temp cleanup, dialog/install gating, and close cancellation. `write_inventory_temp` is the single writer used by the background path. |
| P1 | **Oversized robots policies were treated as permission.** The 256 KiB cap returned a truncated prefix and parsed it as complete. | `read_robots_body` now detects any byte beyond 256 KiB and returns `RobotsFailure::Unavailable`; the origin policy is cached as `RobotsPolicy::Unavailable`, so no target is requested on that origin. An exactly-256 KiB policy ending at EOF remains valid. | `tests/review_regressions::review_oversized_robots_is_not_complete_permission` reproduces the long-comment/Disallow case and asserts `/blocked` has zero requests. |
| P2 | **Retry-After was displayed but not enforced per origin.** | A per-origin `retry_after_until` map is set from 429/503 `Retry-After` seconds. Later same-origin work waits until the later of the normal crawl delay and the cooldown, through the cancellable, deadline-bounded wait. If the wait cannot finish before the 30-second chain deadline, the check is `Limited`. Integer seconds are supported; HTTP-date values remain advisory text. | `tests/review_regressions::review_retry_after_delays_the_next_origin_request` asserts a 7-second gap before `/b`; `tests/link_evidence.rs` keeps the `Retry-After: 7` reason. |
| P2 | **Shared redirect destinations were fetched repeatedly.** Results were cached only under the starting key. | Every completed actual response is cached under its own URL with the chain stripped. A later redirect reuses it without consuming budget and builds a per-source chain by appending the cached destination's chain. Loop and credential checks remain in force. | `tests/review_regressions::review_redirect_destinations_are_checked_once` asserts `/common` is requested once and both sources retain a one-hop chain plus final URL. |
| P2 | **Stop during the final optional check reported Completed.** | The optional loop checks cancellation immediately after each result and before finalization, not only at the next iteration. A cancelled final check now marks the run stopped. | `tests/review_regressions::review_slow_robots_honors_cancellation_promptly` asserts `CrawlOutcome::Stopped` as well as prompt return and a limited check reason. |
| P2 | **Browser source plus rendered evidence exceeded the page cap.** Each stage retained 10,000 independently. | `retain_occurrence` enforces one combined 10,000-occurrence page budget across rendered and source stages. Omitted occurrences set the page limit string; ordinary discovery and scheduling still consume the full discovery list. | `crawl::tests::combined_rendered_and_source_evidence_share_the_page_cap` exercises the shared budget with rendered then source occurrences; `crawl::tests::evidence_cap_does_not_cut_ordinary_page_discovery` keeps discovery independent. |

Root's round-two reproductions were adapted into `tests/review_regressions.rs` (three loopback tests) and the strengthened slow-robots assertion. The original probe files outside the repository and both independent review files were not modified.

## Independent review resolution map

| # | Review finding | Resolution | Maintained evidence |
| --- | --- | --- | --- |
| S1 | **Standards P2 — truncated hrefs become real destinations.** Parsing bounded an href before scheduling, so a long URL could be fetched as an invented replacement. | `Parsed.links` now keeps every discovery href in document order with an `overlong` flag. A href over 8 KiB is declined explicitly, never resolved or requested; the occurrence is bounded for display and reports `Limited` / “URL exceeds 8 KiB; not requested”. `LinkOccurrence.truncated` and the Link evidence CSV `TRUNCATED` column expose bounded strings. The 10,000-occurrence evidence cap is separate from the complete discovery list. | `crawl::tests::overlong_hrefs_are_declined_without_requesting_a_truncated_destination` asserts no request is longer than 8 KiB, contains no ellipsis, the overlong evidence is `Limited`, and `/real` is still crawled. `crawl::tests::anchor_evidence_caps_without_truncating_page_discovery_links` checks the full discovery list, and `crawl::tests::discovery_beyond_the_evidence_cap_still_rejects_embedded_credentials` keeps credential-bearing tail anchors from being requested. |
| S2 | **Standards P2 — duplicate extraction IDs misattribute exported values.** | `CompiledRules::compile` now requires non-empty unique IDs. `load_from` refuses a saved file with duplicate/empty IDs and returns a recoverable message instead of loading ambiguous rules; `save_to` also rejects duplicates. Inventory still joins outcomes by the documented stable ID. | `extract::tests::duplicate_rule_ids_are_rejected_before_capture_and_from_saved_state` covers compile, load, and save; `extract::tests::saved_rules_round_trip_with_stable_ids` keeps the stable identity contract. |
| S3 | **Standards P3 — fixed temporary CSV fixture path.** | The representative inventory test writes into a unique `tempfile::TempDir` and verifies the round trip there. It never writes the shared temp directory. An opt-in `CALMTOAD_INVENTORY_ARTIFACT` path is used only to regenerate the reviewed `shots/inventory-sample.csv`. | `inventory::tests::writes_a_representative_inventory_csv`; concurrent test runs cannot collide. |
| P1.1 | **Evidence limit truncates ordinary page discovery.** Scheduling consumed the capped anchor list. | Discovery and retained evidence are now separate. The retained anchors still stop at 10,000/page, but the remaining `parsed.links` tail is scheduled through the same same-host, asset, robots, depth, dedup, and page-budget path. Page link counts include that tail. | Root probe adapted as `review_regressions::review_evidence_cap_does_not_cut_discovery`; the unit test `crawl::tests::evidence_cap_does_not_cut_ordinary_page_discovery` also runs the real engine. |
| P1.2 | **robots retrieval failure became permission.** A failure cached `None`, identical to a confirmed absence; ureq status errors also bypassed the intended 4xx policy. | Optional checks cache a typed `RobotsPolicy::{Known, Unavailable}`. `Unavailable` blocks every later destination on that origin without another robots request or target request. 4xx is a confirmed absence and allows; redirects are followed to a bounded hop limit; 5xx/transport/read failures stay unknown/blocked. The main crawler's long-standing permissive robots behavior is unchanged, so the fix is scoped to the new optional checker. | `review_regressions::review_robots_failure_stays_unknown_for_the_whole_origin` asserts both observed destinations are `Blocked` and neither `/a` nor `/b` was requested. |
| P1.3 | **Optional redirects bypassed internal crawl limits.** An external redirect could fetch an unvisited internal page outside depth/page budgets. | Every redirect hop is revalidated. Internal targets are fetched only when already known to the page crawl or already queued as an observed optional document. Unvisited internal pages return `Limited` (“outside the page crawl”) without a request. Redirect targets also reject non-HTTP schemes, embedded credentials, and repeated URLs (loop detection), and use the same robots/budget/deadline path. Known pages and already-checked targets are reused. | `review_regressions::review_external_redirect_respects_internal_page_budget` asserts `/never` receives zero requests at `max_pages=1`, `max_depth=0`. `tests/link_evidence.rs` also covers redirect chains, loops, 403, 429, and deduplicated page responses. |
| P1.4 | **Blocking optional requests ignored prompt cancellation and total deadlines.** | Each destination now has an absolute 30-second chain deadline. Requests use `Request::timeout(remaining)`, robots bodies are read in bounded chunks with deadline/cancel checks, crawl-delay waits are capped by the deadline, and cancellation is polled between chunks and before every request/hop. The robots failure mode is typed `CheckStop::{Cancelled, Deadline}` and becomes `Limited`, not a completed result. `Retry-After` is preserved in the reason without retrying. | `review_regressions::review_slow_robots_honors_cancellation_promptly` uses a robots response that trickles forever and asserts the crawl returns in under 5 seconds with a stopped/limited check. `crawl::tests::optional_waits_honor_the_absolute_deadline_and_cancellation` covers the deadline/cancel wait directly. `tests/link_evidence.rs` asserts `Retry-After: 7` is reported for a 429 without a retry. |
| P1.5 | **Preview workers were missing from close/restart ownership.** | `stop_owned_work` cancels both the crawl job and `extraction_preview_job`. `can_install` requires no active crawl, no preview job, and no inventory job. Starting a crawl still refuses while a preview is running, and the preview shares the bounded crawl lifecycle and cancellation flag. | `tests::install_and_close_own_an_active_extraction_preview` checks install eligibility and close cancellation; existing preview validation/isolation tests remain. |
| P2.1 | **Extraction lost data while reporting `Matched`.** A 5,000-character value was silently truncated, and All mode stopped at a missing attribute. | Value production is bounded while it is produced: descendant text stops at the character cap, attribute values are flagged when truncated, and a truncated value forces `Limited` with a reason. All mode skips a missing attribute, keeps later valid values, and reports `AttributeMissing` (or `Limited` when a cap also applies) rather than `Matched`. Empty values remain real observations. | Root probes adapted as `review_truncation_is_not_reported_as_complete` and `review_all_matches_preserve_later_attributes`. `extract::tests::value_caps_and_run_caps_are_explicit` covers value/run caps. |
| P2.2 | **Inventory export froze the UI and refused streaming runs.** | Export now copies a coherent immutable snapshot at trigger time, including pages, audit, indices, run context, link checks, and frozen extraction rules. It is allowed during an active crawl and is labeled `in-progress snapshot at <UTC>`; completed runs are labeled `complete snapshot at <UTC>` in the new appended `RUN SNAPSHOT` column. Projection, CSV encoding, and file writing run on background threads. The save dialog opens after the snapshot is ready. | `tests::inventory_export_snapshots_an_active_run_and_builds_off_the_ui_thread` proves a streaming snapshot ignores later pages and starts a cancellable background job. `tests::inventory_export_is_available_while_a_crawl_is_streaming` proves the Overview action works during a crawl. `tests::inventory_scope_uses_all_or_the_saved_internal_view_and_a_frozen_run` preserves saved-run scope semantics. |
| P2.3 | **Completed preview provenance changed with draft edits.** | Completed previews now freeze the actual `RenderMode` and full `ExtractionRule` definitions used for that capture. The UI labels the snapshot mode and rule names from those definitions, not current drafts. If the current mode or drafts differ, it adds a visible stale-capture warning without relabeling old values. | `tests::completed_preview_keeps_its_capture_mode_and_rule_definitions` and `tests::stale_preview_is_visibly_marked_after_draft_edits`. |

Root's five probes were adapted through the real caller seams in `tests/review_regressions.rs`; the original findings file and `/private/tmp/calmtoad-audit-review-probes.rs` were not modified.

## Feature checkpoints

### Stage 1 — actionable link diagnostics

- `src/links.rs` keeps per-occurrence evidence: source, raw href, resolved URL, fragment, normalized anchor, stage, order, truncation, skip/check facts. Duplicate destinations stay separate; network requests remain deduplicated.
- `Page.links` and existing page-level audit/CSV semantics are unchanged. Retained evidence is bounded at 10,000 occurrences/page, 250,000/run, 2,048 anchor characters, and 8 KiB URLs. The evidence cap no longer affects discovery.
- Optional **Check external links and documents** is off by default and snapshotted in the run. It checks observed external HTTP(S) anchors and same-host document downloads, never document bodies or recursive links, excludes image/script/font/media assets, and reuses page-crawler responses.
- Guardrails remain: 1,000 destinations, one request at a time, one per origin, at least one second between starts per origin, robots.txt per origin, one GET, five redirect hops, 10-second request and 30-second chain bounds, no retries. States distinguish Checked, Blocked, Limited, Rate limited, Restricted, Unavailable, Invalid, and Fragment.
- Links has **Pages / Link evidence** views with independent search/filter/sort/selection, an occurrence table, occurrence CSV (including `TRUNCATED`), and an inspector with stage, raw href, fragment, truncation note, reason, redirect chain, source navigation, and copy/open actions. Page-level Links CSV remains unchanged.
- Overview adds optional link-check progress and occurrence-based findings/coverage only when the saved run opted in.

### Stage 2 — complete content inventory export

- `src/inventory.rs` defines stable ordered columns and an immutable projection over retained facts. Existing page and Sitemaps CSV schemas are untouched.
- **Export inventory CSV…** is reachable from Overview and Internal. Default scope is **All captured URLs** (URL-sorted). **Visible Internal URLs** uses the Internal view's saved search, filter, and sort.
- A snapshot can be taken during streaming. It is labeled in the notice, the save title, and the appended `RUN SNAPSHOT` column. Unknown values stay empty; real zeros remain `0`; non-HTML/failed pages do not invent content findings.
- Each row includes URL, status/error, content type, capture mode/completeness/reason, title/description, H1 JSON, known words, depth, inbound/outbound, canonical, indexability label/reasons, per-check findings, link occurrence/check/finding coverage, run context, snapshot label, and frozen extraction columns.
- Locked column order: `URL, STATUS, ERROR, CONTENT TYPE, CAPTURE, CAPTURE COMPLETE, CAPTURE REASON, TITLE, META DESCRIPTION, H1S, WORDS, DEPTH, INBOUND, OUTBOUND, CANONICAL, INDEXABILITY, REASONS, TITLE FINDING, META FINDING, H1 FINDING, RESPONSE FINDING, CANONICAL FINDING, IMAGE FINDING, LINKS FINDING, LINK OCCURRENCES, LINK CHECKED, LINK FINDINGS, LINK EVIDENCE LIMITED, RUN SEED, RUN STARTED UTC, RUN PAGE LIMIT, RUN DEPTH LIMIT, RUN SITEMAPS, RUN CHECK LINKS, RUN OUTCOME, RUN CAPTURED URLS, RUN INCOMPLETE CHECKS, RUN SITEMAP URL CHECKS INCOMPLETE, RUN SITEMAP FILE FINDINGS, RUN LINK EVIDENCE LIMITED, RUN SNAPSHOT, RUN EXTRACTION RULES`, then per rule `EXTRACT <id> NAME, VALUES, STATE, STAGE, REASON`.
- Representative generated CSV: `shots/inventory-sample.csv` (ignored artifact; 17 data rows + header) regenerated with `RUN SNAPSHOT`.

### Stage 3 — custom CSS extraction

- `src/extract.rs` supports up to 20 named rules (unique stable ID, unique name, selector, Text/Attribute, optional attribute, First/All), with bounded validation and reserved inventory-name checks.
- Rules compile once before page work and evaluate the same selected snapshot as the audits: document HTML in HTML mode, rendered DOM in Browser mode. Missing rendered DOM reports `Unavailable`; source HTML is not substituted.
- Outcomes distinguish Matched, No match, Attribute missing, Not applicable, Unavailable, Partial, Limited, and Cap reached. Truncation and missing attributes are explicit; All mode retains later valid values.
- Caps: 100 values/rule/page, 4,096 characters/value, 64 KiB/page, 32 MiB/run. Text production stops at the cap instead of collecting unbounded strings first.
- Rules persist in `extraction.json` with a schema version and recoverable malformed/duplicate-ID message. Drafts validate before crawl/preview; invalid rules cannot alter retained results.
- The crawl-options disclosure opens a compact scrolling editor with add/remove/edit, move up/down (IDs stable), output/match toggles, attribute field, and field-adjacent errors. **Test on URL** validates first, then captures one page through the normal scope/robots/timeout/cancellation path. Preview is disabled during an active crawl and blocks starting a crawl while running.
- Completed previews freeze mode and rule definitions and mark themselves stale when current drafts differ. The Internal inspector shows retained values, state, stage, and reasons; inventory uses the frozen rules.
- Selector support is Scraper's CSS subset, not full browser-engine CSS.

## Changed paths

- `src/links.rs`, `src/inventory.rs`, `src/extract.rs` (new)
- `src/crawl.rs`, `src/main.rs`, `src/lib.rs`, `src/sitemap.rs`
- `tests/review_regressions.rs`, `tests/link_evidence.rs`, `tests/extraction_crawl.rs` (new)
- `tests/browser_render.rs`
- `PRODUCT.md`, `README.md`, `DESIGN.md`, `CHANGELOG.md`
- `docs/plans/desktop-audit-next-three-review.md`
- `shots/` representative screenshots and CSV (ignored artifacts)

## UI artifacts

Generated after the correction pass; no bulk image attachment was used in this review:

- `shots/link-evidence-{880x560,1180x740}-{light,dark}.png`
- `shots/link-evidence-detail-{880x560,1180x740}-{light,dark}.png`
- `shots/inventory-scope-{880x560,1180x740}-{light,dark}.png`
- `shots/extraction-preview-{880x560,1180x740}-{light,dark}.png`
- `shots/extraction-editor-*`, `shots/extraction-results-*` from the earlier pass remain representative of unchanged layouts.
- `shots/inventory-sample.csv`

The changed surfaces are compact rows and disclosures: the evidence table adds a truncation flag, the inventory export adds a snapshot label, and the completed preview adds frozen mode/rule provenance plus a stale-capture warning.

## Validation (final working tree)

- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --lib` — **118 passed, 0 failed, 1 ignored**.
- `cargo test --bins` — **82 passed, 0 failed, 2 ignored**.
- `cargo test --test review_regressions` — **9 passed, 0 failed**.
- `cargo test --test link_evidence` — **2 passed, 0 failed**.
- `cargo test --test extraction_crawl` — **1 passed, 0 failed**.
- `CALMTOAD_REQUIRE_BROWSER=1 cargo test --test browser_render` — **31 passed, 0 failed, 0 ignored** on the final full run with the installed Chrome. The extraction test holds the existing `SERIAL` lock and proves rendered-DOM extraction (`Hydrated heading`), not source HTML. Two earlier full runs this pass hit the pre-existing transient Chrome “did not report a debugging endpoint within 15 seconds” startup flake (20/11 and 27/4); the failed tests passed individually and the next full run passed 31/31.
- Windows compilation: `cargo xwin check --target x86_64-pc-windows-msvc` — passed.

### Local inventory measurement

`cargo test --lib synthetic_inventory_10k -- --ignored --nocapture` and `cargo test --bins inventory_trigger_latency -- --ignored --nocapture`:

```
10000 rows: snapshot clone 12.142375ms, audit clone 335.25µs, project 231.697042ms, csv 112.529917ms, bytes 4179556
50000 rows: snapshot clone 59.641958ms, audit clone 136.75µs, project 1.099443541s, csv 572.941ms, bytes 20939556
trigger latency: all 236.167µs, visible subset 1.304834ms
```

The direct snapshot figures come from the ignored measurement harness. In the shipping path, page copying is split into 512-page pulses, so the trigger itself measured 236 µs for All and 1.30 ms for a small Visible subset on 2,000 pages with 500,000 link occurrences and 20,000 extraction outcomes. Projection, encoding, and writing run in background threads. This is a local measurement, not a CI performance gate.

## Remaining limitations and tradeoffs

- The optional checker is sequential (one in-flight request) to respect the one-per-origin cap without a new scheduler. Large external sets can be slow; the 1,000-destination and 30-second chain bounds limit it.
- Cancellation during an idle blocking `ureq` read is bounded by the 5-second read timeout, not instantaneous. A trickling response is interrupted between chunks; an idle socket can take up to that timeout to notice the flag.
- The main page crawler keeps its existing robots-failure behavior (a failed robots read does not block page crawling). The stricter unknown/blocked policy applies to the new optional checker.
- A robots `Unavailable` result is reported as **Blocked** because the check could not obtain permission; it is not a confirmed broken link.
- Final Standards correction: inventory start captures small immutable Arc handles and saved view options. Page scanning/copying is split into 512-page pulses; audit/check map cloning and sorting run in a worker, with event consumption paused until the snapshot is ready. The 50k-page benchmark stayed below 0.15 ms for the trigger and 1.6 ms for a preparation poll. Dataset replacement invalidates preparation rather than mixing generations.
- The inventory writer writes a temporary file and replaces the destination only after a complete flush. The pinned tempfile library handles replacement on Windows; root removed the manual delete-and-retry fallback so a replacement error preserves the existing destination. A hard process kill can leave an orphan temporary file, but not a partial destination.
- Final root correction: Retry-After accepts integer seconds and standard HTTP-date forms. Values beyond the remaining deadline or platform clock range leave later destinations explicitly Limited; shared intermediate redirects also reuse cached responses with the correct per-source chain.
- Robots policy over 256 KiB is explicit unknown/blocked. The page crawler keeps its existing permissive robots failure behavior; the stricter policy is scoped to optional checks.
- Link evidence is bounded per page/run and marks the omitted tail. Overlong URLs are declined with evidence; they are not checked or scheduled.
- Source fallback is covered at the Browser capture/model seam; a full real-browser timeout-to-source-fallback crawl is not added as a new long-running integration test.
- Native Windows/Linux runtime walkthroughs were not performed from this Mac; only Windows compilation is checked locally. CI owns exact-commit gates and runtime matrices.
- No cloud account, AI recommendations, crawl history, XPath/regex/JS extraction, or new browser automation was added.

## Hand-back

Root and calmtoad-eng should review the working tree and the artifacts above, then run exact-commit CI and release verification. Nothing here has been committed, pushed, tagged, released, or deployed.
