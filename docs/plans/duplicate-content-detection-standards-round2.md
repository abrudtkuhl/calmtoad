# Duplicate content — core/standards review, round 2

Reviewed the working tree against `5a61a926`, including the untracked content module, with PRODUCT.md, DESIGN.md, the original implementation brief and correction report. This is an independent child reviewer, **not the named calmtoad-eng2 task**. No implementation edits or Cargo suites were performed.

## Standards

No independently actionable documented-standard violation or material style-only smell. The following are functional resource/coverage defects; grouping them as naming or abstraction preferences would understate their effect.

## Functional/spec findings

### P1 — Resource reservation failure still retains/allocates data

`src/content.rs:335–339` returns `evidence` after exact-reservation failure, but the evidence already owns an excerpt at `:331`; subsequent pages therefore keep adding uncharged excerpts after the run budget is exhausted. In analysis, failed URL-metadata reservation at `:899–903` only appends a warning, after which both maps are allocated and URLs cloned at `:905–926`. `groups.clone()` at `:1378` duplicates exact-member vectors and strings without a reservation. The 256-byte near-pair reservation at `:1206` also ignores variable-length cloned URLs/IDs in the stored pair. These are budget bypasses, not merely a distinction between allocator RSS and conservative capacity estimates.

Spec: “512 MiB total retained content-analysis data (including indexes/evidence)” and “Account for ... URL/index overhead ... a numeric counter that omits the dominant allocations is insufficient.” Refuse or shed the allocation when reservation fails, and use shared ownership or count every retained copy. Add exhaustion tests that assert retained payload stops growing, rather than only inspecting the counter.

### P1 — All candidate article bodies are now retained simultaneously

`src/content.rs:387,403–412` saves the complete extracted `String` of every eligible article until `max_by` at `:424`. Nested candidates copy the same large descendant text many times. Approximately 1,400 nested `<article>` elements around a 950 KB text node fit under the existing 1 MB HTML capture cap while producing about **1.33 GB of simultaneously retained candidate text**; the roughly two million node visits can still fit the shared node-work bound. This is source-derived arithmetic, not measured RSS. Extraction does not reserve these candidate bodies in ContentBudget. The later `.chars().count()` comparisons also scan the large retained strings without cancellation checkpoints.

Spec: “Bound transient allocations too” and cancellation throughout substantial work. Retain only the current best candidate and precomputed length, or select/summarize roots in a bounded pass before extracting the winner. Validate adversarial nested articles under the actual capture cap, including peak memory and Stop latency.

### P2 — Representative eligibility performs a quadratic uncancellable scan

`src/content.rs:1052–1065` finds each multi-member group's representative by scanning all inputs. A 50k-page corpus containing 25k distinct duplicate pairs requires about 625 million URL comparisons, with no cancellation checkpoint in that outer loop. The reported benchmark has zero exact groups and the giant-group fixture has only one, so neither covers this shape. The URL-to-input index is already built immediately afterward at `:1076`; build/reuse it before this loop and check cancellation during group/member processing.

Spec: “Cancellation checks throughout loops” and measure adversarial bounds, rather than only zero-group and single-group corpora. This is a source-path finding; elapsed time was not independently measured.

### P2 — Group members do not inherit their representative's limited/cancelled coverage

Members are assigned `CoveredByExactGroup` at `src/content.rs:1062` merely because the representative has eligible near input. After verification, only representative URLs are changed to SearchLimited/Cancelled at `:1295–1310`. Their members remain `CoveredByExactGroup`, which `AnalysisState::is_compared` treats as compared, and are omitted from Not compared at `:1343`. A large exact group whose representative's near search never completes therefore disappears from affected-URL limit/cancellation counts except for one URL.

Spec: “Every cap reports its reason and affected counts” and URL-specific comparison progress distinct from input eligibility. Propagate the terminal representative coverage state to all members while retaining exact-group membership as a separate fact. Test a cancelled or pair-limited representative with multiple exact members.

## Confirmed improvements / limits

The ancestor-chain exclusion, iterative deep traversal and stage/policy partitioning are present and address the original false-match paths. This review did not independently rerun runtime probes; root owns those tests. Four functional findings above remain (two P1, two P2); standards-only findings: zero.
