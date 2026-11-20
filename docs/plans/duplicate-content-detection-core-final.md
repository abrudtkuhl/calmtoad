# Duplicate content — final core verification

Reviewed the current local diff against `5a61a926`, original brief, third review, correction report and permanent regression assertions. No implementation edits or Cargo suites; root owns execution and any subsequent correction verification. This reviewer is not the named calmtoad-eng2 task.

## Verified corrections

- Metadata reservation failure genuinely returns before map allocation/URL cloning (`src/content.rs:904–922`). The new test inspects empty retained maps, not only the counter.
- Exact reservation failure drops the excerpt; article selection keeps only one best candidate; stage/policy partitioning, indexed representative lookup and group-wide terminal propagation remain present.
- The final group-copy failure clears group/pair/member associations and zeroes exact finding counts (`:1295–1314`), removing the previous phantom Overview count. Its regression verifies these collections/counts using short, near-ineligible content.

## Remaining findings observed before root's final narrow correction

### P2 — Near-eligible representatives overwrite the final-copy resource-failure state

Final-copy rejection sets every exact-eligible URL to ResourceLimited at `src/content.rs:1310–1313`, but subsequent representative finalization at `:1367–1381` unconditionally overwrites that state with Compared/SearchLimited/Cancelled for near-eligible representatives. The permanent final-copy regression uses short text, so it skips this loop and does not cover the failure. With two identical 80-token documents and about 1,900–2,000 bytes remaining, metadata/member/index reservations can succeed, verification completes and the final group-copy reservation fails; the representative can then claim Compared despite discarded exact evidence and disappear from Not compared.

Spec: “Every cap reports its reason and affected counts” and “Not compared explains URL-specific reasons.” Preserve terminal resource failure during representative finalization, or finalize failure after the normal state pass. Root is running the targeted public-API probe and coordinating this narrow fix; append its verified outcome rather than assuming the initial patch is complete.

### P1 — Metadata accounting still charges one URL payload for multiple retained copies

`src/content.rs:900–903` reserves `url.len() + 64` per input. It subsequently clones the URL into both `pages` (`:932`) and `url_states` (`:946`), then into each Not compared row (`:1464`) for ineligible inputs. The first reservation therefore cannot be conservative for long URLs: three valid 8,000-byte input URLs with 30 KB remaining reserve about 24 KB, while the two maps alone retain 48 KB in URL payloads, before another 24 KB of diagnostics. At 50k such URLs the difference is hundreds of MiB, not allocator rounding. These are source-derived owned-string payload counts, not a measured RSS claim.

Spec: “Account for ... URL/index overhead ... a numeric counter that omits the dominant allocations is insufficient.” Share URL ownership or reserve every retained/transient owned payload before construction, including diagnostics. A small constrained-budget/long-URL test can assert payload accounting without any large memory stress. The corrected early return enforces the reservation result, but it does not correct an underestimated reservation.

## Standards and limits

No material documented-standard or style-only findings. Core behavior is substantially corrected; the two findings above are functional spec issues. No destructive nested-article/RSS stress, cross-platform execution or independent full-suite run was performed by this child reviewer. Root will record actual runtime results and any final closure below.

## Closure after the final narrow correction

Both residual findings above are **resolved in the executing source and permanent regression assertions inspected in this pass**. The earlier descriptions remain as review history, not open findings. This closure is code/assertion verification; root owns independent test execution after OpenCode's current run finishes.

- **Metadata URL accounting:** `src/content.rs` now defines four simultaneous metadata URL copies (input, page-map key, state-map key and Not compared URL) and charges `(url.len() + 64) * 4` before construction. The failed-reservation early return remains before both maps. `long_url_metadata_is_reserved_for_every_retained_copy` uses the actual three ~8 KiB URLs / 30,000-byte-free case, asserts no retained maps/rows and no charge on rejection, then separately checks that a successful result's ledger covers the actual retained URL string payload. This corrects the material multi-copy undercount raised here.
- **Terminal final-copy failure:** representative finalization now skips any URL already marked ResourceLimited, so it cannot undo the all-input resource state assigned when final copies fail. The later Pending-only pass also preserves that terminal state. A `final_copy_failed` flag selects the exact operation's diagnostic text. `final_copy_failure_preserves_near_eligible_resource_state` explicitly asserts NearState::Eligible, forces the final-copy reason, checks both representatives/members remain ResourceLimited, confirms zero finding groups/counts/associations and requires two Not compared rows with the final-copy reason. It covers the control-flow branch that the earlier short-text fixture missed.

No additional substantive core defect or material standards-only finding was identified within this final correction scope. No implementation edits or Cargo execution were performed by this reviewer. Remaining heuristic extraction, approximate/capped retrieval and estimated-ledger-versus-process-RSS limits still apply as documented; this closure does not claim allocator-level memory guarantees or substitute for root's runtime checks.
