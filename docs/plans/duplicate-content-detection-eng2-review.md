# Duplicate content detection — independent review

Reviewed the working tree against `5a61a9266db1d62e02f80e491f4d63841033b28f`, including untracked content module and tests, and the implementation brief and report. Findings below are code inspection; no independent runtime probes or Cargo tests were run. No implementation edits were made.

## Spec findings

1. **P1 — Near comparisons cross capture stages and policy versions** (`src/content.rs:820-829, 885-904`). Exact groups key on stage and policy, but the band index keys only on band and signature rows, and verification never checks either field. Two sufficiently similar Document and Rendered pages therefore become a near pair; different policies also compete in the same capped buckets. Include stage and policy in candidate partitioning, and regression-test cross-stage and cross-policy pairs.

2. **P1 — Excluded ancestors do not exclude candidate roots** (`src/content.rs:290-303, 330-336`). The global scan considers every main/article, while `clean_subtree` checks only the root and descendants. For `<div hidden><main>secret</main></div><main>visible</main>`, the hidden main wins. The same applies inside navigation/footer or other excluded ancestors. Select roots only within an eligible ancestor chain; test nested hidden and navigation roots.

3. **P1 — Depth limit silently turns partial text into complete exact evidence** (`src/content.rs:341-343, 212-234`). Descendants past depth 512 are dropped without an incomplete flag, after which the remaining text is hashed and marked exact eligible. Pages identical outside the cutoff and different beneath it can become false same-text matches. Traverse iteratively or propagate an explicit extraction limit and exclude partial evidence from exact matching; test deep differing content.

4. **P1 — The advertised run memory cap does not cover retained analysis data** (`src/content.rs:270-275, 712-715, 820-831, 1002-1007`). Reservation covers shingles, signature and a fixed allowance only. Excerpts and evidence for short pages are retained without any reservation; URLs, exact membership, copied groups, LSH buckets and transient construction also bypass it. For example, many short pages with 2,000-character excerpts consume unaccounted memory even though the counter barely moves. The report acknowledges this, but the brief explicitly includes indexes, evidence, URLs and transient allocations. Account before allocation, communicate affected exact and near coverage, and validate with peak memory on adversarial mixed and short-page corpora.

5. **P2 — Extraction can repeat substantial work without cancellation** (`src/content.rs:290-305, 341-355`). Every candidate article is traversed afresh, including nested candidates, before a winner is selected. Nested articles can repeatedly visit the same descendants; extraction does not receive a cancellation token. The 512-depth guard limits depth per traversal but neither bounds total repeated work nor gives Stop a checkpoint. Add bounded single-pass accounting or equivalent and exercise deeply nested/repeated roots while cancelling.

6. **P2 — Cancellation skips entire analysis phases** (`src/content.rs:705-715, 737-801, 820-836`). Input sorting and page-map construction occur before the first check; draft sorting and group construction and the full 24-band index construction also lack checks. A pre-cancelled or newly cancelled large run still performs those phases, including allocations. The reported 50k measurement does not establish bounded latency for larger or long-URL datasets. Add checkpoints and measure cancellation within each phase.

7. **P2 — Full exact hashes are unavailable above an extra 2 MiB limit** (`src/content.rs:212-228, 442-463`). The brief requires complete exact hashes even for oversized near inputs, without retaining normalized documents. Here normalization stops and excludes the page instead. Stream normalized text into SHA-256 and retain only bounded excerpts, or explicitly revise the product contract; test an oversized exact match and differing suffix.

## Standards

No independent actionable documented-standard violation or sufficiently material judgment-only code smell was identified beyond the functional findings above. The repeated article traversal is also a maintainability and performance concern, but is reported once as a concrete spec failure. Screenshots and UI behavior were outside this review.
