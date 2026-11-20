# Duplicate content detection — round-three correction pass

Local implementation evidence for the three findings in `duplicate-content-detection-review-round3.md`. This is for independent re-review, not a claim that the review is resolved. The independent review files remain unchanged.

## 1. P1 — Failed metadata reservation still allocated result maps

**Actual code now:** in `content::analyze_with_budget`, a failed `try_reserve(metadata_bytes)` records the captured and exact/near resource-loss counts and **returns immediately** with `pages`, `url_states`, `not_compared`, `groups`, `pairs` and member associations empty. It does not allocate the two maps or clone input URLs after the failure.

**Permanent regression:** `tests/duplicate_root_regressions.rs::exhausted_metadata_does_not_allocate_per_url_result_maps` passes 100 inputs with a fully consumed budget and asserts:

- `pages.len() == 0`
- `url_states.len() == 0`
- `not_compared.len() == 0`
- the limited reason names URL metadata.

The assertion inspects retained collections, not only the budget counter. The implementation report had incorrectly claimed this early return before the source contained it; the implementation report is corrected to describe the round-three code.

## 2. P2 — Final-copy failure left phantom counts and associations

**Actual code now:** when the final exact-group copy reservation fails, the operation atomically:

- clears `groups`, `all_groups` (from the cleared groups), `pairs`, `member_groups` and `similar_affected_urls`;
- resets `coverage.exact_groups` and `coverage.exact_affected` to zero;
- marks every exact-eligible input `AnalysisState::ResourceLimited`;
- records the exact resource-limit reason and emits Not compared rows with nonempty reasons.

No inspectable group is advertised after its retained copy was dropped.

**Permanent regression:** `tests/duplicate_root_regressions.rs::final_copy_failure_removes_phantom_groups_and_associations` forces the constrained final-copy path with two identical short pages and asserts zero groups/all groups, zero same-text affected URLs, no member associations, all URL states ResourceLimited, and two Not compared rows. Existing Overview/table code reads the corrected counts and group vectors; no dangling group references remain.

## 3. P2 — Inventory and duplicate exports could lose writer ownership

**Actual code now:** a common `export_lifecycle_busy` predicate covers inventory preparation, inventory export preparation, duplicate-content preparation, dialog, writer, and duplicate export job. It is enforced by:

- `start_inventory_export` and `start_content_export` before any allocation/work;
- `export_csv` before projection;
- `save_prepared_csv` before opening a dialog or assigning writer state;
- the writer callback via `export_writer_slot_available`, so a live `inventory_write` receiver/cancel handle cannot be replaced;
- `can_install`, new-crawl and close ownership paths.

Inventory and duplicate export buttons also include the other exporter’s preparation/job state. Atomic destination writing remains unchanged.

**Regression:** `tests::export_entrypoints_share_one_lifecycle_in_both_start_orders` covers inventory-first and duplicate-first starts, and a live writer slot: start attempts fail, `export_writer_slot_available` is false, install is blocked, and close cancels the writer. A second completion cannot replace a live writer because the callback refuses without a destination write.

## Measurements and preserved behavior

No cap, threshold, or performance baseline was changed. Existing measured behavior remains:

- 50k analysis ~2.18s; cancellation ~167ms; retained ledger ~191 MiB; measured peak RSS ~349 MiB.
- 50k analysis trigger ~0.70ms; first poll ~79µs.
- 50k member export trigger ~2.78ms; background projection/encoding ~238ms.
- Large result completion ~0.31ms Same / ~0.14ms Similar.

The metadata-failure and final-copy-failure paths fail before/while shedding retained allocations; they do not add new large fixtures or dangerous memory stress.

## Validation

- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --lib` — 142 passed, 2 ignored.
- `cargo test --bins` — 89 passed, 5 ignored.
- `cargo test --test duplicate_root_regressions` — 6 passed after the final core fixes in `duplicate-content-detection-final-core-fixes.md`.
- `cargo test --test content_analysis` — 1 passed.
- Existing correction and integration tests remain part of the final report’s checks.
- `CALMTOAD_REQUIRE_BROWSER=1 cargo test --test browser_render` — 32 passed, 0 failed, 0 ignored on the final run. Windows compile passed.

## Remaining limitations

- The 512 MiB ledger is conservative retained-data accounting, not allocator-level RSS.
- Exact and near coverage can both be resource-limited; exact failures now have no fabricated inspectable groups.
- Near retrieval remains approximate and bounded.
- Structural text selection remains heuristic.
- The 2 MiB exact-input retention limit remains as directed.
- The three round-three findings were fixed locally; independent review must verify them.
