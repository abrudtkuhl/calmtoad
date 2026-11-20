# Duplicate content detection — round-two correction pass

Local implementation evidence for the eight findings in `duplicate-content-detection-review-round2.md`. This is not a claim that the review is resolved; independent re-review remains required. The three round-two review documents remain unchanged.

## Finding map

### 1. Failed resource reservations still retained/allocated data

- Exact evidence reservation now **rejects evidence retention**: if the excerpt/hash reservation fails, extraction returns an incomplete evidence object with an empty excerpt, no shingles/signature and an explicit exact reason. It does not return the previously built excerpt.
- Failed URL-metadata reservation returns a bounded coverage result immediately instead of allocating page maps and cloning URLs. Round three verified the executing source and added a retained-collection regression; the earlier report claim was premature until that correction.
- Exact-group member storage and the final group copy are reserved. A failed group copy clears the retained groups/pairs and reports exact resource loss. Near-pair reservations use the variable-size pair, URL and group-ID lengths rather than a fixed 256 bytes.
- The member index and LSH index have explicit reservation/omission reasons.
- Regressions: `tests/duplicate_root_regressions.rs` asserts an exhausted budget retains **zero excerpt bytes**; `content::tests::evidence_and_analysis_charges_share_the_run_budget_and_report_limits`; the exact-group limit test asserts nonempty reasons.

### 2. Article selection retained every candidate's text

- Article selection now keeps only one best candidate (`String`, length, completeness) at a time. A new candidate replaces the previous one only when longer; rejected candidate text is dropped immediately.
- Traversal remains iterative, uses a shared node-work bound, and checks cancellation. The later length comparison uses the stored length instead of rescanning large strings.
- Regression: `content::tests::largest_article_wins_with_earlier_dom_order_on_ties`; hidden/deep root cases remain.

### 3. Export preflight undercounted concurrent row/CSV data

- `estimate_content_export_bytes` now computes full-schema field costs for each projected row, including representative/group/hash/status/context repetition and worst-case CSV escaping, and counts the simultaneous row vector plus encoded CSV.
- The production worker projects/encodes with a runtime byte limit (`content_export_rows_with_limit`); exceeding it aborts before returning a complete result. The worker remains cancellable.
- `duplicate_export_preserves_long_context_and_csv_escaping` covers long run context, quotes/newlines, formula markers and escaping; the 50k-member ignored test measures the real background payload.

### 4. Case-colliding member URLs overwrote associations

- The member index is now `HashMap<String, Vec<String>>` with Unicode `to_lowercase()` matching the UI search normalization. Every member/group association is retained even when distinct case-sensitive URLs fold to one key.
- Regression: `content::tests::case_colliding_member_urls_keep_every_group_association` creates two groups with `/Product` and `/product` and asserts both group IDs remain under the folded key.

### 5. Result/search preparation rebuilt giant member strings on the UI

- `content_row_search` no longer concatenates member URLs. It uses compact representative/group/stage/hash metadata.
- Member matches use the prebuilt association index; `rebuild_content_rows` skips search-string construction when the query is empty and uses cached row text for nonempty queries. This removes giant membership concatenation from result arrival, sort, subview switches and query changes.
- Regression: the nonrepresentative `/pricing` Similar query test; long-context export test; UI 50k/40k result measurements.

### 6. Representative lookup was quadratic and uncancellable

- The URL → input index is built before group processing and reused for representative eligibility and propagation. No per-group `inputs.iter().find` scan remains.
- Group/member processing and index construction have cancellation checkpoints.
- Regression: `content::tests::many_small_exact_groups_use_indexed_representative_lookup` (2,000 duplicate pairs) and cancellation coverage tests.

### 7. Group members did not inherit limited/cancelled near coverage

- Exact membership remains `CoveredByExactGroup` only while the representative’s near search completes normally.
- After representative terminal states are known, `SearchLimited` and `Cancelled` are propagated to every exact member of that group, so Not compared, affected-limit counts, search and export agree.
- Regression: `content::tests::exact_group_members_inherit_terminal_near_search_limits` with 100 duplicate pairs and a constrained analysis budget; asserts at least 200 URL-level SearchLimited entries and nonempty reasons.

### 8. Exact-analysis resource failure produced an empty reason

- Not-compared reason selection now distinguishes near-input resource failure from exact-analysis failure. Exact failure uses the exact reason or an explicit exact-analysis budget message; every emitted Not compared row has a nonempty reason.
- Regressions: the budget/exhaustion tests and root regression assert nonempty exact reasons.

## Root probe adaptation

The observation probe was converted to expected assertions in `tests/duplicate_root_regressions.rs`:

- Exhausted extraction has no retained excerpt.
- Document/Rendered near comparison has no pair.
- Deep differing leaves have different exact hashes/excerpts.
- Hidden `main` does not beat the visible article.
- Case-colliding member groups, group-wide limited coverage and large affected counts are covered by content tests.

The probe/log outside the repository were not modified.

## Measurements

- 10k corpus: analyze **~531 ms**, retained ledger **38 MiB**, cancellation **~31 ms**.
- 50k corpus: analyze **~2.18 s**, retained ledger **191 MiB**, cancellation **~167 ms**; `/usr/bin/time -l` peak RSS **365,940,736 bytes (~349 MiB)** including transient construction.
- 50k UI analysis trigger **~0.70 ms**, first poll **~79 µs**.
- 40k-member exact group / 200 pairs: same-result preparation **~0.31 ms**, similar **~0.14 ms**.
- 50k member CSV export: trigger **~2.78 ms**, total background projection/encoding **~238 ms**, payload **~7.1 MB**, escaping included.
- These are local measurements; the 512 MiB ledger is conservative retained-data accounting, not allocator-level RSS.

## Validation

- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --lib` — **142 passed, 2 ignored**.
- `cargo test --bins` — **88 passed, 5 ignored**.
- `cargo test --test duplicate_root_regressions` — 1 passed.
- `cargo test --test content_analysis` — 1 passed.
- `cargo test --test review_regressions` — 12 passed; `link_evidence` — 2 passed; `extraction_crawl` — 1 passed.
- `CALMTOAD_REQUIRE_BROWSER=1 cargo test --test browser_render` — 32 passed, 0 failed, 0 ignored on the final run.
- Windows compile — passed.
- `scripts/perf-gate.sh` — passed with the unchanged baseline; audit -0.4%, parse_typical +8.8%, parse_large +3.0%.

## Remaining limitations

- Resource accounting uses conservative capacity estimates; it is not a custom allocator or proof that process RSS cannot transiently exceed the ledger.
- Near retrieval remains approximate and capped.
- Structural selection is heuristic; CSS-only hiding and body-fallback template noise remain possible.
- The 2 MiB exact-input retention limit remains as directed by the consolidated review’s upstream-capture note.
- Long-URL and adversarial fixtures are bounded representative tests; the 1,400-nested-article/1.33 GB source arithmetic was not run as a destructive stress test.
