# Duplicate content — spec review, round 2

Reviewed the corrected working tree against v0.7.0 `5a61a9266db1d62e02f80e491f4d63841033b28f`, the original brief, joint review, and correction report. No feature edits, commits or release actions. Root owns the Cargo suites; this review traces UI/export/lifecycle/search and uses a deterministic byte-count probe rather than duplicating tests.

## Correction status

- **4, affected-count UI freeze:** the primary defect is fixed. `ContentAnalysis::similar_affected` returns cached `similar_affected_urls`; the worker deduplicates group IDs and sums disjoint memberships once. `poll_content_analysis` updates Overview content counts without refreshing all page/audit counts. A remaining result/search projection problem is described below.
- **5, CSV UI projection/encoding:** the primary defect is fixed. `ContentExportSource` freezes the result Arc, selected row references, subview and context. `start_content_export` performs projection/encoding in a worker; generation checks and prepare/dialog/write gates cover install/new crawl/close. Export resource accounting remains incorrect (finding A).
- **9, nonrepresentative member search:** ordinary ASCII member lookup is fixed with `member_groups` and `content_row_matches`, but the new index loses case-colliding URL paths (finding B).
- **10, per-member CSV scope:** fixed. `content_export_rows` uses `member.scope` and `member.stage`; the mixed-Main/Body regression exercises this correction.

## Actionable findings

### A. P1 — Export estimate does not bound projection/CSV allocations

`src/main.rs:7058–7080` estimates Same-text export as `sum(member.url.len()+256)` plus context **once**; each projected row actually clones group ID, representative, hash and run context, and retains thirteen String cells per row. Similar uses a fixed 512 bytes/pair regardless of URL/context length, and Not compared a fixed 256 bytes/row. The worker builds the entire row vector and then an additional entire CSV string (`7117–7123`) without reservation or checks against actual allocations. Thus the new preflight check permits scopes that exceed the explicit budget.

Spec: “512 MiB total retained content-analysis data”; “Account for ... export copies”; “Bound transient allocations too.”

Deterministic byte-count probe matching the Same-text row schema: 50k short member URLs, a valid ~2 KiB crawl seed, ordinary group/hash/status strings. Estimate: **14,102,638 bytes**. Row string payload alone: **117,350,000 bytes**; CSV payload: **119,350,000 bytes**, combined **236,700,000 bytes**, excluding Vec/String/allocator overhead. With 400 MiB already retained, preflight admits ~433.5 MB, while actual payloads require at least ~656.1 MB, above 536.9 MB. This is arithmetic from actual row fields, not an RSS measurement.

Fix with conservative full-schema/capacity accounting, or bounded streaming into the atomic temporary file; charge all simultaneous owned data and check/cancel during production rather than relying on the underestimated preflight alone. Test long run context, URLs, reasons and CSV escaping amplification.

### B. P2 — Member search index overwrites distinct case-sensitive URLs

`src/content.rs:1039` inserts `member.url.to_ascii_lowercase()` into `HashMap<String,String>`, one group ID/value. Distinct paths `/Product` and `/product` may belong to different exact groups, yet the second folded key overwrites the first. If these are nonrepresentative members and their representatives lack that substring, Similar search `/product` retains only the final group's pairs. The search should match both under the UI's case-insensitive semantics. ASCII-only index normalization also differs from `normalized_query`/row-search Unicode lowercasing.

Spec: “Search includes both URLs and group members.”

Store every member/group association (or folded-key→all group IDs), normalize consistently, and add two distinct exact groups with case-colliding nonrepresentative paths plus separate near pairs. Assert both results survive the query.

### C. P2 — Final-result/search projection still expands every exact member on UI

The correction report states `rebuild_content_rows` no longer expands memberships, but `src/main.rs:2549–2553` still concatenates every group member URL and lowercases the entire result; `2660` invokes it for every rebuild even with an empty query. A nonempty query invokes `content_row_search` again via `2606`, rebuilding the same huge string rather than using `content_search`. These paths run synchronously on result arrival, sort, subview switch and query changes. The supplied ~7ms completion measurement uses 40k **short** URLs; it does not establish bounded behavior for long URLs or large retained membership text.

Spec: “Cross-page analysis runs off the UI thread”; “<8ms bounded UI polling/preparation”; “Account for ... URL/index overhead”.

Use short representative metadata in row search and the existing member-association index for membership checks; cache/prepare required indexes off-thread or in bounded steps. Measure completion and query processing with a giant group of long valid URLs, and ensure the fix does not copy giant memberships into each pair. This is residual unbounded UI work; the original repeated-membership affected-count defect is resolved.

## Scope and limitations

No new cloud, release or unrelated behavior was found in this review. Source checks show CSV destination handling continues to use the atomic writer and the frozen result remains consistent across dialog activity. Root and an additional standards reviewer handle core extraction, caps, memory reservations and cancellation; the named eng2 task has not picked up this round because it is still waiting on an older approval prompt. Root independently runs the tests. This report does not claim cross-platform GUI validation or measured RSS for the byte-count example.
