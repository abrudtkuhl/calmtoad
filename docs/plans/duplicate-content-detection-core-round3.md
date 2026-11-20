# Duplicate content — core review, round 3

Reviewed the latest uncommitted implementation against `5a61a926`, including the new content module, original spec, round-two review and `duplicate-content-detection-fixes-round2.md`. This independent core reviewer is not the named calmtoad-eng2 task. No implementation edits or Cargo suites were run.

## Findings

### P1 — Failed metadata reservation still allocates the rejected data

`src/content.rs:904–908` appends a warning when `budget.try_reserve(metadata_bytes)` fails, but does not return. It immediately allocates two maps sized for all inputs at `:910–911` and clones every URL into both at `:917` and `:931`. An exhausted ledger can therefore continue retaining URL metadata; the warning is not an allocation bound. This is the same material failure path as round-two finding 1, despite the fix report stating “Failed URL-metadata reservation returns a bounded coverage result immediately.”

Spec: “512 MiB total retained content-analysis data (including indexes/evidence)” and “a numeric counter that omits the dominant allocations is insufficient.” Return a bounded failure result or reserve enough storage for the retained failure evidence before allocating. Test that an exhausted-budget result does not contain the rejected maps/URL copies, not merely that its warning is nonempty. Root owns the runtime exhaustion probes; this finding is confirmed from the unchanged control flow.

### P2 — Final-copy budget failure leaves findings and coverage inconsistent

On final exact-group-copy reservation failure, `src/content.rs:1280–1291` clears `groups` and `pairs`. However, `coverage.exact_groups` and `coverage.exact_affected` were computed at `:1056–1064` and are not cleared/reconciled. `same_text_affected()` (`:860–861`) still returns the positive affected count, so Overview can link a nonzero exact-content finding to an empty Same text table. `url_states` and `member_groups` also retain associations from the discarded groups; members already marked CoveredByExactGroup can remain “compared” and absent from Not compared, even though the evidence no longer exists. Later representative propagation runs over the now-empty `groups`, so it cannot repair those states.

Spec: “Every cap reports its reason and affected counts,” “Not compared explains URL-specific reasons,” and Overview rows link to their views. Either avoid the duplicated result copy through shared ownership, or update counts, group references and every affected URL's resource-limited state when discarding evidence. Add a constrained-budget case that reaches this final-copy failure and asserts consistency among group vectors, Overview counts, URL states and Not compared rows. This is a source-path finding; no independent peak-memory stress run was performed.

## Confirmed corrections

- Rejected exact extraction now drops the excerpt (`:336–344`); deep traversal and excluded-ancestor fixes remain present.
- Article selection retains one best candidate and its length (`:391–439`), fixing simultaneous retention of all candidate bodies.
- Pair reservations now include variable URL/group-ID lengths (`:1219–1224`); final exact-group copying is explicitly reserved (`:1262–1280`), with the consistency issue above on rejection.
- Representative eligibility reuses the URL index (`:977–981`, `:1073–1076`) rather than scanning all inputs per group.
- Limited/cancelled representative states propagate to exact members (`:1360–1381`) when those groups remain retained.
- Every emitted Not compared reason has a nonempty fallback (`:1435–1439`), resolving the previously empty diagnostic. It can still be generic; this review does not treat that wording as a release blocker.
- Stage/policy partitioning and defensive verification remain in place.

## Standards and validation limits

No actionable documented-standard breach or material style-only smell. Both findings are functional spec failures. Root owns current tests and measurements; the implementation report is not independent evidence of resolution. Ancestor/root enumeration still merits adversarial work/cancellation measurement because ancestor checks are outside the traversal-node counter, but this review does not label that a blocker without measured impact. **Functional findings: 2 (one P1, one P2); standards-only: 0.**
