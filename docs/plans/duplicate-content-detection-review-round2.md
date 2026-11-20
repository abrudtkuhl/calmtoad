# Duplicate content — re-review

November 20, 2026. Current uncommitted changes against `5a61a9266db1d62e02f80e491f4d63841033b28f`, including untracked content module/tests. Reviewed against the original brief, joint review and correction report. **Not ready to ship.** No implementation edits, commits or releases.

Root and independent spec/core reviewers completed this pass. The requested calmtoad-eng2 task has **not** performed this re-review: its earlier turn is still `waitingOnApproval` (confirmed through `read_thread` and `wait_threads`); the new request and findings are queued there. Its previous review is historical evidence only. Finishing its independent pass requires resolving the older prompt in that task; the tools available here cannot resolve that approval.

Supporting reports: [UI/export spec](duplicate-content-detection-spec-round2.md), [core/standards](duplicate-content-detection-standards-round2.md).

## Confirmed corrections

- Original findings 1/2/6: excluded ancestor handling, deep-text traversal and stage/policy isolation now have code fixes and permanent expected-behavior tests. Root reran them successfully.
- Original finding 4: affected-URL counting is cached and no longer repeatedly expands memberships per pair in the UI. A separate row/search preparation issue remains below.
- Original finding 5: CSV projection and encoding now run in a worker. Export allocation bounds remain incorrect.
- Original finding 10: each member's actual extraction scope/stage is exported.
- Original findings 3/7/8/9 are partial: resource bounds, URL-level coverage, bounded/cancellable work and member-aware search still have the issues below.

## Spec findings

1. **P1 — Resource reservations still do not enforce the memory bound.** `src/content.rs:335` returns evidence retaining its excerpt when its reservation fails; `:899` warns on failed metadata reservation but still allocates maps and clones URLs. `:1378` deep-clones group/member data without charging it. Pair reservations use a fixed 256 bytes despite variable-length URL copies. Root reproduced extraction retaining a 26-byte excerpt with zero budget remaining. This is a concrete allocation bypass, beyond normal RSS-versus-ledger estimation. Refuse/shed allocations on failure, share or charge every owned copy, and test exhausted budgets by inspecting retained data, not only counters.

2. **P1 — Article selection retains every candidate's full text.** `src/content.rs:387–424` stores every cleaned article String until choosing a winner. About 1,400 nested articles around a 950 KB text node fit under the upstream HTML cap and near the two-million-node work bound, yet their candidate Strings can total roughly 1.33 GB. This is source-derived arithmetic, not a measured RSS result. Keep only a bounded best candidate or summarize/select roots before extracting the winner. Measure actual transient bytes and cancellation on nested candidates, not just flat corpus pages. The current work bound does not bound retained bytes or ancestor-selection work.

3. **P1 — Export preflight undercounts simultaneous row/CSV data.** `src/main.rs:7058` estimates one member URL plus 256 bytes per same-text row and adds context only once. Actual rows repeat representative/group/hash/context fields, then a second complete encoded CSV coexists with them. Similar/Not compared use fixed constants regardless of field lengths. The independent schema byte-count probe gives **14.1 MB estimated versus at least 236.7 MB in row/CSV payloads** for 50k rows with a valid ~2 KiB crawl seed. With 400 MiB retained, preflight passes while payload alone exceeds the 512 MiB budget. Account for all concurrent capacities/escaping or stream bounded records into the atomic temporary destination. Add runtime checks during production, not only underestimated preflight.

4. **P2 — Member search overwrites distinct case-sensitive paths.** `src/content.rs:1039` maps a folded URL to one group ID, so `/Product` and `/product` in different exact groups overwrite one another. Root reproduced two groups/four members yielding only three index entries. Similar queries can omit one group's nonrepresentative member evidence. Preserve every URL/group association and use the same normalization as the UI query.

5. **P2 — Result/search preparation still builds huge strings on the UI thread.** `src/main.rs:2549` concatenates every member URL into group search text; `:2660` invokes it on rebuild, and nonempty queries rebuild it again through `:2606` instead of using the cached text. This contradicts the correction report's claim that expansion was removed. Use compact metadata plus the member association index; prepare/cache substantial indexes off-thread or in bounded steps. Validate long URLs and query/sort/result-arrival paths, not only short-URL fixtures.

6. **P2 — Representative eligibility is quadratic and lacks cancellation.** `src/content.rs:1052` scans all inputs for each multi-member exact group's representative. A 50k-page crawl with 25k distinct duplicate-pair groups produces roughly 625 million URL comparisons without a Stop checkpoint in that loop. This is a source-path finding, not a measured timing. Build/reuse the URL index currently created immediately afterward, and check cancellation during group/member processing. Add a many-small-groups fixture; zero groups or one giant group misses this shape.

7. **P2 — Group members do not inherit limited near-search coverage.** Members become `CoveredByExactGroup` at `src/content.rs:1062` before search completes; only representatives receive SearchLimited/Cancelled updates at `:1295`. Since covered members count as compared, Not compared and affected-limit counts omit the other members of an incompletely searched group. Keep exact membership separate from near progress and propagate the representative's terminal near state to every member. Root's limited-index probe is recorded in the validation log.

8. **P2 — Exact-analysis memory failure can export an empty reason.** When exact member reservation fails (`src/content.rs:991`), analysis sets ResourceLimited, but reason construction at `:1351` reads `content.near_reason`. For an otherwise eligible page this string is empty. Root reproduced `exact_resource_limited=1`, ResourceLimited state and `reason=""`. Store a reason for the operation that failed rather than borrowing an unrelated extraction reason; retain separate exact/near coverage and test this path.

## Standards

No independently actionable documented-standard violation or material style-only finding. All eight findings above concern the spec's behavior, resource limits or responsiveness; no unrelated scope expansion found.

## Validation and limits

- Independently passed **139 library + 87 UI + 17 integration = 243 tests**, plus the focused required-Browser rendered/source extraction test (**1 passed**, 31 filtered), for **244 checks passed**. Seven measurement tests intentionally ignored in the normal suite. Full Browser suite, release/platform CI and perf gate were not repeated in this pass. OpenCode's own reported runs remain implementation evidence, not independent verification here.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `git diff --check` passed after removing the temporary review probe. Formatting output from the temporary unformatted probe was not a feature-code defect.
- Root temporary public-API probe reproduces exhausted-budget excerpt retention, empty resource-limit reasons, folded-key loss and exact-group coverage under a constrained analysis budget. Source/output saved under `/private/tmp/calmtoad-duplicate-round2-review-probe.rs` and `/private/tmp/calmtoad-duplicate-round2-probe.log`; removed from repository tests after execution. Observational probes need permanent expected-behavior regressions when corrected.
- No dangerous memory-amplification stress run was performed. Nested-article and export payload calculations are explicitly source/schema-derived, not peak-RSS observations. No cross-platform GUI claim.

**Totals:** Spec: 8 findings (3 P1, 5 P2); Standards: 0. Requested eng2 re-review remains pending its older approval prompt. Correct remaining findings and re-review before shipping.
