# Duplicate content detection — correction pass

Local fixes for the ten consolidated findings in `duplicate-content-detection-review.md`. This is implementation evidence for independent re-review, not a claim that the review is resolved. `duplicate-content-detection-review.md`, `duplicate-content-detection-eng2-review.md`, and `duplicate-content-detection-spec-review.md` remain unchanged.

## Finding-by-finding map

### 1. Excluded ancestors could supply main content

- `content::select_content` now requires `eligible_root`, which checks the candidate root **and every ancestor** against the same excluded-element rules before selecting a `main`, `[role=main]`, or `article`.
- Hidden, inert, navigation-role, `nav`, `footer`, `aside`, and other excluded ancestors therefore cannot win.
- Regression: `tests/duplicate_root_regressions.rs::root_review_cases_have_expected_behaviour` asserts the visible article wins and the hidden main is absent. Content tests also cover hidden body content and navigation exclusion.

### 2. Silent deep-text truncation produced false exact matches

- Replaced the recursive depth cutoff with an iterative stack traversal. There is no depth-512 drop.
- Traversal has a total `MAX_TEXT_NODES` work bound and a cancellation checkpoint. Reaching the bound marks extraction incomplete; incomplete extraction is not exact-eligible and is never hashed as a complete page.
- Regression: the hidden/deep root review test constructs 520 nested containers with different leaves and asserts different hashes and visible differentiating excerpts. `content::tests` also covers block/inline boundaries and article selection.

### 3. Resource accounting omitted retained/transient data

- `ContentBudget` now reserves for excerpt/evidence retention as well as shingles/signature. `ContentEvidence::retained_bytes` includes excerpt, shingles, signature and structural overhead.
- Analysis is seeded with evidence retained during extraction through `ContentBudget::with_used` and reserves URL metadata, exact-group member storage, the member-URL index, the LSH index and retained pair evidence before use.
- Exact evidence and analysis use the same 512 MiB run accounting. Exact vs near loss is reported separately (`exact_resource_limited`, `near_resource_limited`, member-index/LSH omission reasons).
- CSV export estimates projected rows and refuses a scope that would exceed the remaining content budget; actual projection/encoding happens in a cancellable worker.
- Regression: `content::tests::evidence_and_analysis_charges_share_the_run_budget_and_report_limits`; `same_text_export_uses_each_member_scope_and_cancels_before_projection`.
- Limitation: reservations are conservative estimates of capacities/ownership, not allocator-level accounting. Construction peaks are bounded by the explicit item caps; process RSS was measured separately where reported below.

### 4. Large memberships froze result completion

- `ContentAnalysis` now stores `similar_affected_urls` calculated once during analysis. `count_similar_affected` deduplicates exact group IDs before expanding memberships; it does not visit a 40k-member group 200 times.
- Result arrival updates the cached same/similar Overview counts directly. It no longer calls the full page/audit count refresh.
- `rebuild_content_rows` no longer expands group members into search text, and empty queries skip member-index construction.
- Regressions: `content::tests::large_group_affected_count_is_distinct_and_bounded_work` (40,000 members × 200 pair repetitions; bounded under 250 ms in debug) and the ignored `duplicate_content_result_completion_is_bounded_on_large_memberships` UI measurement.

### 5. CSV projection/encoding blocked the UI

- Duplicate-content export now freezes an `Arc<ContentAnalysis>`, visible row references, subview, context and sort scope, then projects rows and encodes CSV in a cancellable worker (`to_csv_cancellable` checks every 64 rows).
- Only after preparation does `poll_content_export` open the native save dialog. Atomic destination writing and preparation/dialog/write lifecycle gates remain.
- The export source is generation-tagged; a new crawl invalidates stale results. Close, updater install and new-crawl guards account for the export job.
- Regressions: `same_text_export_uses_each_member_scope_and_cancels_before_projection` and ignored `duplicate_content_export_of_50k_members_runs_in_background` (50,000 member rows; measured below).

### 6. Near search crossed stage/policy boundaries

- LSH bucket keys now include capture stage, policy version, band and signature rows. Caps apply inside each (stage, policy) partition.
- Verification additionally checks stage and policy equality defensively.
- Regressions: `content::tests::comparison_partitions_by_capture_stage`, `comparison_partitions_by_policy_version`, and `tests/duplicate_root_regressions.rs` (Document vs Rendered near pair must be absent).

### 7. Skipped analysis coverage had no URL-level state

- Added `AnalysisState::{Pending, Compared, CoveredByExactGroup, Ineligible, TooShort, TooLarge, ResourceLimited, SearchLimited, Cancelled}`. Extraction/near-input state remains `NearState`; analysis progress is separate.
- `NotCompared` now carries both `near_state` and `analysis_state`. Eligible pages skipped by cancellation, candidate/verification/pair caps or a bucket/index limit appear in Not compared with the relevant state and reason. Nonrepresentative exact members are marked covered by their exact group rather than falsely “compared” for near search.
- `Coverage.near_search_limited` and the coverage summary expose affected URL counts.
- Regressions: cancellation test asserts URL-level `Cancelled`; UI/export tests exercise the Not compared state and reason fields.

### 8. Repeated subtree traversal and cancellation gaps

- Structural extraction is iterative and uses one shared total work counter across candidate roots. It does not recursively revisit descendants without a bound or rely on a silent depth cutoff.
- Cancellation is threaded through extraction (`extract_with_cancel` via crawl parsing) and checked at the node, page-map, exact-group, member-index, LSH and verification phases.
- Pre-cancelled and mid-analysis latency are measured; see below.

### 9. Similar search missed nonrepresentative members

- `ContentAnalysis.member_groups` records one lowercased member URL → exact group ID entry per member. Similar search checks matching group IDs rather than copying all members into every incident pair.
- `content_row_matches` uses this index for both Same text and Similar queries. Searching a nonrepresentative exact member retains the incident pair; an unrelated query returns no rows.
- Regression: `tests::duplicate_content_inspector_and_exports_expose_evidence` queries the nonrepresentative `/pricing` member and then an unrelated query.

### 10. Same-text export used representative scope for every member

- Same-text CSV writes `member.scope` and `member.stage` per row, not `group.scope`.
- Regression: `same_text_export_uses_each_member_scope_and_cancels_before_projection` builds a mixed Main/Body exact group and asserts each member’s exported scope.

## Root reproductions

The observation-only root probe was converted to expected-behavior tests in `tests/duplicate_root_regressions.rs`. It now asserts:

- Document/Rendered similar text produces no near pair.
- 520-deep differing leaves are exact-eligible with different hashes and different excerpts.
- A hidden `main` cannot beat a visible `article`.
- Bounded affected counting is covered by `content::tests::large_group_affected_count_is_distinct_and_bounded_work`.

The probe and its log at `/private/tmp` were not modified or copied into the repository.

## Measurements

- `content::tests::benchmark_deterministic_10k_and_50k_corpora` (ignored):
  - 10k: extract 6.93s, analyze 512ms, retained budget 38 MiB; cancellation 29.1ms.
  - 50k: extract 34.6s, analyze 2.25s, retained budget 191 MiB; cancellation 164.4ms; `/usr/bin/time -l` peak RSS 365,940,736 bytes (~349 MiB) including transient construction.
- `content_snapshot_trigger_and_poll_stay_within_the_budget` (ignored): 50k trigger ~1.7ms, first 512-input poll ~628µs.
- `duplicate_content_result_completion_is_bounded_on_large_memberships` (ignored): 40k-member exact group / 200 pairs, Same view ~7ms, Similar ~1ms.
- `duplicate_content_export_of_50k_members_runs_in_background` (ignored): trigger ~1.46ms, total projection/encoding ~231ms for 50,000 member rows / ~7.1 MB.
- All measured cancellation paths are below the 250ms target; the first UI preparation trigger and final-result preparation are below 8ms on the measured corpus.

## Validation

- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --lib` — 139 passed, 2 ignored.
- `cargo test --bins` — 87 passed, 5 ignored.
- `cargo test --test duplicate_root_regressions` — 1 passed.
- `cargo test --test content_analysis` — 1 passed.
- `cargo test --test review_regressions` — 12 passed; `link_evidence` — 2 passed; `extraction_crawl` — 1 passed.
- `CALMTOAD_REQUIRE_BROWSER=1 cargo test --test browser_render` — 32 passed, 0 failed, 0 ignored on the final run.
- Windows compile (`cargo xwin check --target x86_64-pc-windows-msvc`) — passed.
- `scripts/perf-gate.sh` — passed with the unchanged baseline; audit -1.6%, parse_typical +9.0%, parse_large +3.2%. An earlier run under machine load showed an audit regression; the settled rerun passed.

## Remaining limitations

- The 512 MiB ledger uses conservative byte estimates for capacities and ownership; it is not a custom allocator or an exact process-RSS measurement.
- Near retrieval remains approximate and bounded; no universal recall or “no duplicates exist” claim is made.
- The structural text selector remains a heuristic; CSS-only hiding and body-fallback template noise can escape it.
- The 2 MiB exact-input retention limit remains. The consolidated review says this is not a current release blocker under upstream capture bounds; full-text hashing within supported captures is preserved.
- Exports and analysis are generation-tagged; a stale result is discarded rather than mixed with a newer crawl.
