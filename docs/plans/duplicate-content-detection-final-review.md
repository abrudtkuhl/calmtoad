# Duplicate content — final local review

November 20, 2026. **No remaining blocking finding in the reviewed local changes. Ready for commit and release validation; not committed, pushed, tagged or shipped by this review.**

Root plus independent core and UI/export reviewers verified the current implementation against the original duplicate-content brief and all correction reports. The named calmtoad-eng2 task remains blocked on its older approval; it did not perform this final pass. References: [core verification and closure](duplicate-content-detection-core-final.md), [UI/export verification](duplicate-content-detection-spec-final.md), [last two core corrections](duplicate-content-detection-final-core-fixes.md).

## Spec

All previously recorded blocking findings have substantive code corrections and regression coverage. In particular:

- Capture stage/policy, hidden ancestors, deep content and selected-text completeness are handled without the reproduced false matches.
- Failed metadata reservations return before retaining per-URL maps. Reservations include all four simultaneous metadata URL copies. The valid three-long-URL/30 KB failure case now rejects retention; the success test checks actual retained URL payload against the ledger.
- Final-copy resource failure resets finding counts/evidence/associations and remains terminal through near-eligible representative finalization. Tests assert the precise failure reason, both URL states and Not compared rows, including the former probe budget range.
- Article selection retains only its current best candidate. Representative lookup is indexed, affected counts are cached, and exact members inherit limited/cancelled near progress.
- Member search keeps case-colliding associations without giant per-group membership strings in UI row metadata. Per-member export scope is correct.
- CSV preparation runs off-thread with schema/context/escaping accounting, cancellation and generation snapshots. Inventory and duplicate exports share exclusive prepare/dialog/write ownership; a live writer cannot be overwritten. Normal completion removes its own prepare handle before entering the shared save guard.

No new cloud, crawler, release, website or unrelated scope was introduced by the correction passes. No further substantive spec finding in the reviewed scope.

## Standards

No actionable documented-standard breach or material style-only finding. No implementation edits were made by root or the final reviewers; OpenCode implemented the corrections under the user's fix-all request.

## Independent validation

- **253 tests passed:** 142 library, 89 UI, and 22 integration (6 root regressions, 1 content-analysis crawl, 12 existing review regressions, 2 link-evidence, 1 extraction crawl). Seven optional measurement tests remained ignored in the normal suite.
- OpenCode tightened the fixed per-page metadata allowance during hand-back. Root reran the complete suite on that production revision and confirmed content/UI/export source hashes remained stable. The regression file received additional coverage/comments during hand-back; after OpenCode became idle, root reran all six permanent root regressions and compared the file against a frozen snapshot.
- Formatting, clippy and whitespace checks are recorded in `/private/tmp/calmtoad-duplicate-release-ready-clippy.log` and the final root tool results. Final suite log: `/private/tmp/calmtoad-duplicate-release-ready-tests.log`; post-hand-back regression log: `/private/tmp/calmtoad-duplicate-release-ready-regressions.log`.
- Prior root public probes and the permanent expected-behavior cases agree on the corrected metadata, copy-failure and finalization behavior. The earlier observational probe remains outside the repository at `/private/tmp/calmtoad-duplicate-final-review-probe.rs`.

Full Browser/platform/release CI, performance measurements and native save-dialog interaction were not all independently repeated for the final two narrow core changes. Earlier local Browser checks and OpenCode's reported platform/performance runs are recorded in the implementation history. Run the project's required checks against the exact release commit before tagging. This review does not substitute local checks for release CI.

Known product limits remain explicit: structurally selected text is heuristic, near retrieval is approximate/capped, comparisons cover captured pages, and the retained-data ledger is an estimate rather than allocator-level process-RSS control. Limited/cancelled coverage must remain visible; no SEO/indexing guarantee or automatic consolidation instruction is implied.

Final review totals: Spec **0 open blockers**; Standards **0 actionable findings**. Proceed to the normal release workflow when authorized.
