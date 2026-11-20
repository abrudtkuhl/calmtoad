# Duplicate content detection — final core fixes

Local implementation evidence for the two final core findings. This is for root/independent verification, not a claim that any review is resolved. Independent review files were not modified.

## A. P1 — metadata reservation undercharged owned URL copies

**Defect:** `metadata_bytes` charged one `url.len() + 64` per input, but analysis retains the input URL plus separate copies in `pages`, `url_states` and `not_compared`. Three ~8 KiB URLs with 30,000 bytes free retained 71,928 bytes of URL payload while the ledger charged 24,168.

**Actual code now:** before any map allocation or URL clone, `src/content.rs` charges `METADATA_URL_COPIES` (4: input, page key, URL-state key, Not compared) at `url.len() + 64` per copy plus a fixed 384-byte per-input allowance for map capacity, the Not compared row/reason and temporary URL-index entries. A failed reservation still returns immediately with empty result collections, truthful captured/exact/near coverage counts and the URL-metadata reason. Thresholds, caps and the 512 MiB ledger limit are unchanged.

**Permanent regression:** `tests/duplicate_root_regressions.rs::long_url_metadata_is_reserved_for_every_retained_copy`

- Exact root case: 3 URLs of ~8 KiB with 30,000 bytes free returns empty `pages`, `url_states`, `not_compared`, leaves the ledger unchanged, and reports the URL-metadata reason.
- Success case: after analysis the ledger (`retained_bytes`) is at least the URL payload retained in page keys, URL-state keys and Not compared rows.
- The existing fully-exhausted 100-URL early-return test remains and still passes.

## B. P2 — near-eligible representative lost its terminal resource state

**Defect:** the failed final-group-copy reservation marked exact-eligible URLs `ResourceLimited`, but the later representative-finalization loop overwrote the representative with `Compared` (or `SearchLimited`). The previous short-text regression never entered that branch because short pages are not near-eligible.

**Actual code now:** final-copy failure records a terminal failure flag/reason, and representative finalization skips any URL already in `AnalysisState::ResourceLimited`. Not compared reasons for `ResourceLimited` use the actual final-copy reason when it caused the state, otherwise the retained near resource reason. Groups, pair/member associations and exact counts are still cleared atomically.

**Permanent regression:** `tests/duplicate_root_regressions.rs::final_copy_failure_preserves_near_eligible_resource_state`

- Preconditions assert both 80-unique-token pages are exact-eligible and `NearState::Eligible`.
- Runs the near-eligible fixture at the root probe budgets (1,800/2,000 free bytes) and at 3,000 free bytes. At 1,800/2,000 the near index is omitted, so without the guard the representative would become `SearchLimited`; at 3,000 the near index fits and it would become `Compared`.
- Each case asserts the final-copy reason actually occurred, `exact_resource_limited == 2`, zero groups/all groups/affected/member associations, both URLs `ResourceLimited`, and both Not compared rows carry the final-copy resource reason.

`tests/duplicate_root_regressions.rs::near_eligible_final_copy_probe_budgets_remain_coherent` converts every root probe budget (1,600/1,800/2,000/2,200/2,400 free bytes) into an assertion: a retained group must advertise one group/two affected URLs and no resource-limited URL; a shed group must advertise zero groups/counts/member associations and leave both URLs terminally `ResourceLimited` with the final-copy reason. With the new accounting, the two-page probe reaches final-copy failure at 1,600/1,800/2,000 and retains one SearchLimited group at 2,200/2,400.

## Validation

- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --lib` — 142 passed, 2 ignored. The existing terminal near-search-limit test budget was raised from 100,000 to 280,000 free bytes to preserve its intended SearchLimited path under the larger metadata charge.
- `cargo test --bins` — 89 passed, 5 ignored.
- `cargo test --test duplicate_root_regressions` — 6 passed.
- `cargo test --test content_analysis` — 1 passed.

## Preserved limits

No threshold, cap, verification/pair limit, exact-retention limit or 512 MiB ledger limit changed. The ledger remains conservative retained-data accounting, not allocator RSS. The metadata failure returns before retaining per-URL detail; that intentional loss is reported by counts and reason.
