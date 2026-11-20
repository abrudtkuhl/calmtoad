# Duplicate content detection

Implementation brief, November 19, 2026. Base: `5a61a9266db1d62e02f80e491f4d63841033b28f` (v0.7.0). Owner: existing OpenCode build session. Deliver local changes for independent review; do not commit, push, tag, release or change the website.

## Outcome

After a local crawl, find pages with the same normalized content and pairs with substantial text overlap. Let a person inspect the evidence and export it. This extends the existing title/description duplicate checks; it does not replace them. No cloud, AI, extra requests, stored crawl history, semantic equivalence claims, or automatic advice to delete/merge pages.

Read PRODUCT.md, DESIGN.md, the Impeccable craft guidance before UI edits, the existing audit expansion plan/final review, and [algorithm research](duplicate-content-research.md). Research supplies algorithm sources; thresholds and limits below are product defaults requiring measurement.

## Content contract

Extract once from the DOM already parsed for the selected content-audit capture. Keep existing word counts unchanged. In HTML mode use the document; in Browser mode require a successful complete rendered capture. Source fallback must never masquerade as rendered content. Only complete successful 2xx HTML documents qualify; exclude redirects, failures, truncated/missing captures and non-HTML. Noindex/canonicalized pages still qualify: these signals are useful comparison context, not exclusions.

Select one cleaned subtree: first nonempty main/[role=main], otherwise largest nonempty article (DOM-order tie), otherwise body. Never concatenate overlapping roots. Exclude script/style/template/noscript/nav/footer/aside, navigation-role subtrees, and explicitly hidden/inert content. Preserve article headers/headings; exclude site-level headers on body fallback. Handle block/line-break boundaries separately from inline boundaries: `co<em>op</em>` remains `coop`. Define exact rules in module docs and fixtures. This is structurally selected text, not a claim of CSS-visible text. Record main/article/body provenance and explain body fallback only in comparison details.

Exact normalization v1: decoded DOM text, ASCII case folding, collapse Unicode whitespace, preserve punctuation, digits and order. SHA-256 over the entire normalized text; never hash a truncated prefix as a complete page. Label results “Same normalized text”, with hash-based detection explained in details. Empty content never forms a match; short identical nonempty text can, with its length visible. Retain only bounded excerpts, not full raw HTML or every normalized document. Do not claim byte-identical HTML or collision-free equality.

Near comparison v1: Unicode alphanumeric token runs, preserve combining marks within tokens, ASCII folding as above; punctuation separates tokens. Keep digits/negations. Document limitations for unspaced languages; report too few tokens honestly. Unique consecutive five-token shingles; stable 64-bit shingle hashes and sorted sets. At least 50 tokens; at most 10,000 tokens/page for near analysis. Oversized pages remain eligible for a complete exact hash but near analysis is explicitly skipped, never prefix-scored. Preserve bounded text excerpts sufficient to understand the extraction, clearly labeled as excerpts rather than full diffs.

Partition comparison by capture stage and extraction policy version; record each page's extraction scope even when scopes differ. Exact matching means selected text, not the entire document. Surface scope differences in pair details.

## Comparison and bounds

Exact groups are independent of approximate search. One deterministic representative per exact group enters near search; keep membership so matching groups can be expanded without materializing a Cartesian product. Stable IDs derive from stage/policy/hash or sorted URL identity, not discovery order.

Use fixed-seed MinHash (120 coordinates, 24 bands of 5) to generate candidates, then verify Jaccard on the retained shingle-hash sets. Require >=0.80 and safely prune when smaller/larger set size <0.80. Display the verified shingle-overlap score, not signature agreement; disclose approximate candidate retrieval and hashing in details. No all-pairs fallback. Near results are explicit pairs, never transitive “duplicate groups”. Suppress the redundant 100% pairs already represented by exact groups.

Starting safety budgets: 256 entries/band bucket, 200 unique candidates/representative, 100,000 verifications/run, 10,000 retained near pairs/run, 512 MiB total retained content-analysis data (including indexes/evidence), and <=2,000 Unicode characters/excerpt/page. Bound transient allocations too. Hot buckets must not expand quadratically before the cap is checked. Account for exact membership, signatures, shingles, URL/index overhead and export copies; a numeric counter that omits the dominant allocations is insufficient. Tune budgets downward if measurements require it, document actual defaults/reasons, and do not silently relax them to pass a test.

Sort URLs before bounded candidate enumeration; avoid randomized map iteration in results/cap decisions. Cancellation checks throughout loops. Every cap reports its reason and affected counts; distinguish ineligible, too short, near-input-too-large, resource-limited, pending, cancelled and compared states. Exact coverage and near coverage are separate. Even an uncapped LSH search can miss pairs: use “No similar pairs found”, never “No duplicate content exists”. Partial crawls are comparisons of captured pages only.

## Execution and module seam

Add a focused content-analysis module with immutable shared page evidence, extraction/fingerprint construction, a cancellable analysis entry point, and a result object carrying groups, pairs and coverage. Keep UI rendering unaware of shingle/index machinery. Reuse sha2 and the existing parser; justify any new dependency. Do not deep-clone text/shingles on Page clones, inventory snapshots or UI frames.

Extraction runs in existing crawl workers. Cross-page analysis runs off the UI thread on a coherent generation-tagged immutable snapshot after crawling settles (including a stopped partial crawl). Show “Comparing content…” while working. Do not rebuild a full snapshot/index every frame or compare after every page event. Reuse the existing bounded snapshot pattern; starting another crawl cancels obsolete analysis and rejects stale results. Stopping analysis retains already finished crawl facts and reports partial comparison coverage. Shutdown/update installation must respect or cancel/join owned workers. No thread/resource leaks, no lost Stop control and no UI-blocking Arc copy-on-write work.

## Compact UI

Keep Overview default. Inside Internal add a compact `URLs / Duplicate content` switch; do not add an eleventh top-level tab. Preserve independent selection, query/filter/sort state for the two views. Switching back restores the existing inventory table.

Duplicate content has `Same text / Similar / Not compared` views with row counts and a short coverage/status line. Same text rows show representative URL, matching URL count, selected-text length and capture stage; inspector lists members with pagination. Similar rows show both representative URLs, verified overlap and affected member counts; inspector exposes expandable exact memberships, scope/stage, token counts and excerpts side by side. Not compared explains URL-specific reasons. Empty, pending, cancelled and limited states must differ. Search includes both URLs and group members; sorting has deterministic URL ties. Virtualize rows and paginate long member lists. All toolbar controls and actions must remain usable at 880px width in both themes.

Overview adds only two actionable rows: same-text affected URLs and similar-content affected URLs, linking to their views. Count unique affected URLs, not pair endpoints repeatedly; don't sum overlapping findings into an SEO score. These are “Review” findings. Do not report an all-clear while comparison is pending or limited. Existing page inspectors may link to that page's matches once results are ready; avoid redundant headings/status text.

Actions: inspect either page, copy/open URL, inspect matches, export visible rows. Retain full URLs for copy even when visually clipped. Do not force navigation when comparison completes. Follow existing keyboard flow and escape/back semantics.

## Exports

Add dedicated duplicate-content CSV export using existing quoting/formula-safety and atomic cancellable writer. Same-text exports one row/member with stable group ID and representative URL. Similar exports one row/representative pair with group membership counts and stable group references; document this compact representation. Not-compared exports URL/reason. Preserve the selected view's visible search/sort snapshot; report scope and counts before choosing a file.

Include crawl identity/context, capture stage, normalization/token policy version, scope, coverage state/cap reasons, selected-text/token counts and verified score where applicable. Include IDs/membership through the same-text view so near matches of exact groups are interpretable. Preserve existing inventory column order/semantics; do not require new inventory columns for this feature. Export pending/cancelled results only with truthful snapshot status; never mix generations or overwrite an existing file on failure/cancel.

## Validation and acceptance

1. Extraction/eligibility fixtures: inline/block whitespace, entities/case, meaningful punctuation/digits/negation, main/article/body selection, repeated navigation/site headers, nested roots, empty/short pages, Unicode combining marks/unspaced text, oversized inputs, failed/truncated/rendered-vs-source captures and noindex pages. Identical template navigation with different substantial main text must not become an exact match.
2. Algorithm fixtures: exact membership and stable IDs, hand-calculated Jaccard, >=0.80 boundary, non-transitive A/B/C pairs, size pruning, deterministic shuffled input, known positives and negatives against exhaustive scoring on a small labeled corpus. Report candidate recall separately from verified precision; don't claim theoretical probability as measured recall.
3. Adversarial bounds: giant identical groups, hot LSH buckets, many similar but distinct pages, maximum-length inputs, stop/restart mid-analysis, stale results, resource-cap and memory accounting. Tests must demonstrate limits before allocation/explosion and correct coverage reasons. Exact grouping continues when near work is limited.
4. UI/export integration: default Overview, links to correct matches, filters/search/member lookup/sort, independent subview state, long URLs, keyboard, empty/pending/partial/cancelled cases, 880px and normal desktop sizes in both themes, formula-like values, snapshot consistency, cancellation preserving destinations and updater lifecycle. Create screenshot files; report selected paths without loading old image batches into this session.
5. Run appropriate existing Rust/UI/browser checks and unchanged local parser/audit performance gate. Benchmark deterministic 10k/50k corpora (mixed lengths and pathological repetition), reporting elapsed extraction/signature/candidate/verification time, peak memory, candidate counts, cap coverage, maximum UI snapshot/poll time and cancellation latency. Target <8ms bounded UI polling/preparation and <250ms cancellation response. Report measured costs honestly; no all-pairs, no weaker baseline or hidden cap changes. Near coverage may be limited at 50k, with visible reasons; exact coverage should remain available for all eligible captured pages within explicit resource budgets.

Deliver the complete vertical slice, tests and a concise implementation/validation report in `docs/plans/duplicate-content-detection-implementation.md`. List remaining risks and exact defaults. Update PRODUCT.md for actual behavior, not future claims. Leave changes local for review; no release/version/workflow changes. If a benchmark exposes a material design issue, fix it or report the blocker with evidence rather than declaring the feature complete.
