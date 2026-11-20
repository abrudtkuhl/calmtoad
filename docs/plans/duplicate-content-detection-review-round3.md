# Duplicate content — third review

November 20, 2026. Reviewed current local working changes against v0.7.0 `5a61a9266db1d62e02f80e491f4d63841033b28f`, original feature brief, round-two review and second correction report. Root and independent spec/core reviewers participated. No feature edits or release actions. The named eng2 task is still blocked on its older approval; this report does not attribute this pass to that task.

**Three actionable findings remain; not ready to ship.** Supporting reports: [core](duplicate-content-detection-core-round3.md), [UI/export](duplicate-content-detection-spec-round3.md).

## Spec findings

### 1. P1 — Failed metadata reservation still allocates

`src/content.rs:904–931`: failed `try_reserve(metadata_bytes)` appends a warning, then allocates `pages`/`url_states` and clones input URLs. This is unchanged despite both the correction report and OpenCode hand-back claiming an immediate bounded return.

Root public-API reproduction with a fully consumed budget and 100 inputs returned **100 pages, 100 states and 100 Not compared rows**, with zero budget remaining. The warning is present but the data is still retained. This violates the original total-analysis memory bound and round-two finding 1.

Implement a genuinely bounded failure result before allocating these maps/copies. Preserve truthful captured/limited counts and a reason; avoid duplicating all input strings in an attempted failure report. Test retained data on failure, not only the budget counter. Correct implementation documentation after verifying actual behavior.

### 2. P2 — Final result-copy failure leaves phantom match counts and stale coverage

`src/content.rs:1280–1292`: on failed reservation for the final exact-group copy, groups/pairs are cleared, but `coverage.exact_groups` and `coverage.exact_affected` were populated earlier and are not reset. Existing URL states and member-search associations can also refer to evidence that no longer exists.

Root reproduction: two identical short pages with 1,800 or 2,000 bytes remaining at analysis start yield **zero groups/all_groups but exact_groups=1 and same_text_affected=2**. Overview therefore links a positive match count to an empty table. Exact resource loss is counted, but those URLs retain their pre-failure TooShort/Compared/Covered states rather than consistent exact-result loss.

Make failure atomic across retained groups/pairs, counts, member associations and exact/near coverage. Alternatively avoid the duplicate ownership entirely. Add tests that force final-copy failure and verify Overview/table counts agree, no dangling group references remain and affected URLs have explicit resource-loss coverage. Preserve valid earlier evidence if possible, but never advertise inspectable groups that were dropped.

### 3. P2 — Inventory and duplicate exports can lose writer ownership

`src/main.rs:5912` inventory start guards omit `content_export_job`; duplicate start `:7211` checks only its own job, while the Duplicate Export button `:6773` omits inventory prepare/export jobs. Both preparation paths can therefore coexist.

Each completion calls `save_prepared_csv` (`:7483`), which lacks a shared busy check. Each save callback unconditionally assigns the single `inventory_write` slot (`:7522`). Choosing a second destination while the first background writer is running can replace its result receiver and cancel handle. Its result/cancellation is then untracked, and updater/new-crawl gates can clear before every writer finishes. Serial native dialogs do not prevent overlap after the first dialog closes.

Use a common exclusive export lifecycle gate across preparation/dialog/write and enforce it in start functions and completion callbacks, not only buttons; or explicitly own and track every concurrent job. Add regressions for both start orders and overlapping completions ensuring no active writer can be replaced. Preserve atomic destination protection and cancellation for existing exports. This is a static reachable call-chain finding; no native-dialog/slow-disk reproduction was performed.

## Confirmed corrections

The previous excerpt retention failure, simultaneous article-candidate text retention, underestimated CSV schema/context copies, case-fold member association loss, giant member-string construction, quadratic representative lookup, group-wide terminal coverage propagation and empty-reason output all have substantive corrections in the current tree. Prior false-match/ancestor/stage regressions continue to pass. No additional issue is asserted merely for minor repeated compact-string work or unmeasured ancestor traversal overhead.

## Standards

No independent actionable documented-standard or material style-only finding. The three remaining issues concern required resource/coverage/lifecycle behavior.

## Validation

- Root independently passed **142 library + 88 UI + 17 integration tests = 247**, with seven measurement tests intentionally ignored. Log `/private/tmp/calmtoad-duplicate-round3-tests.log`. These passing tests do not cover the failures above.
- Root's public-API probe reproduced findings 1/2; source saved as `/private/tmp/calmtoad-duplicate-round3-review-probe.rs`, output `/private/tmp/calmtoad-duplicate-round3-probe.log`. Temporary observational test removed from repo after running. Permanent regressions must assert the desired bounded/consistent result.
- `cargo fmt --check`, `git diff --check` and `cargo clippy --all-targets -- -D warnings` passed. Clippy log `/private/tmp/calmtoad-duplicate-round3-clippy.log`.
- Full Browser suite, platform/release CI, interactive export overlap and performance/RSS corpus were not independently repeated this round. OpenCode reports its runs in the implementation document; that does not replace missing failure-path assertions.

Totals: Spec **3** (1 P1, 2 P2); Standards **0**. Send this narrow correction brief to the existing OpenCode session, then re-review before shipping.
