# Duplicate content detection implementation

Local implementation against v0.7.0 (`5a61a9266db1d62e02f80e491f4d63841033b28f`). Delivers the complete vertical slice for independent review. No commit, push, tag, release, version change, network comparison, or website edit was performed.

## Outcome

Internal now has a compact **URLs / Duplicate content** switch. Duplicate content has **Same text / Similar / Not compared** views with independent search, sort, selection and export. Exact groups use normalized full-document SHA-256; similar results are explicit verified MinHash/LSH pairs, never transitive groups. The feature reads already captured pages, makes no extra requests, and partitions comparisons by capture stage and policy version.

The post-review correction pass is mapped finding-by-finding in `duplicate-content-detection-fixes.md`; round-two and round-three corrections are in `duplicate-content-detection-fixes-round2.md` and `duplicate-content-detection-fixes-round3.md`. It adds ancestor-safe selection, iterative text traversal, shared resource accounting, cached affected counts, background duplicate CSV projection/encoding, stage/policy LSH partitions, URL-level analysis progress, bounded traversal/cancellation checkpoints, member-aware search, and per-member export scope.

## Implementation

### Content evidence and extraction

- Added `src/content.rs` with `ContentEvidence`, extraction, normalization, tokenization, shingles, fixed-seed MinHash signatures, LSH candidate retrieval, verified Jaccard checks, coverage and stable result types.
- `Page.content` is an immutable `Arc<ContentEvidence>`; page clones, UI frames and inventory/content snapshots share it rather than deep-cloning text or shingles.
- `parse_html_staged` extracts content once from the DOM it already parses. Source fallback is never treated as rendered content.
- Selection: first nonempty `main`/`[role=main]`, then largest nonempty `article` with earlier DOM-order tie, then `body`. One subtree only. Candidate roots and every ancestor must be eligible, so a hidden/navigation/footer ancestor cannot supply a winning root. Excludes script/style/template/noscript/nav/footer/aside, navigation-role subtrees, hidden/inert/aria-hidden and explicit display:none/visibility:hidden. Body fallback also excludes headers; selected main/article headers remain.
- Traversal is iterative with a total node work bound and cancellation checkpoints. Article selection keeps only one best candidate text at a time. Hitting the bound marks extraction incomplete; incomplete text is never hashed as complete exact evidence.
- Block elements and `br` create boundaries; inline elements concatenate (`co<em>op</em>` → `coop`).
- Exact normalization: decoded DOM text, ASCII case fold, Unicode-whitespace collapse, punctuation/digits/order preserved. SHA-256 covers the complete normalized text. Pages over the 2 MiB retention bound are excluded rather than hashing a prefix.
- Near tokenization: Unicode alphanumeric runs; combining marks remain in tokens; ASCII folding; punctuation separates; digits and negations stay. Unique consecutive five-token shingles use stable 64-bit hashes.
- Near states: eligible, too short (<50 tokens), too large (>10,000 tokens or a >512-character token), resource limited, or not eligible. Exact grouping continues when near analysis is skipped.
- Added direct `ego-tree = "0.10"` dependency for text-node traversal of the tree already parsed by `scraper`; no second parser or runtime engine was added.

### Comparison and bounds

- Exact groups partition by `(capture stage, policy version, SHA-256)`. Stable IDs use stage/policy/hash. Members are sorted by URL; the first URL is representative. Empty text never groups. Noindex/canonicalized pages remain eligible.
- One representative per exact group enters near search. Exact membership stays attached, so matching groups expand without a Cartesian product.
- Fixed seed, 120 MinHash coordinates, 24 bands × 5 rows. Candidates share a complete band **inside the same capture stage and policy partition**; defensive verification repeats the stage/policy check. Verified score is Jaccard over retained sorted five-token shingle hashes, requiring ≥0.80. Safe size pruning rejects pairs whose smaller/larger shingle count is below 0.80.
- No all-pairs fallback. Results are explicit pairs with per-source exact group references and member counts.
- Default bounds: 256 entries/bucket, 200 candidates/representative, 100,000 verifications/run, 10,000 retained near pairs/run, 512 MiB content-analysis budget, 2 MiB normalized exact retention, 2,000-character excerpts. Bucket entries are capped before growth; sorted URL candidate enumeration is deterministic.
- The 512 MiB ledger refuses exact evidence when its reservation fails, accounts for every simultaneously retained URL copy (input, page key, URL-state key and Not compared) plus per-input map/diagnostic scaffolding and returns before result-map allocation when that metadata cannot be retained, reserves excerpts, shingles/signatures, exact memberships, final group copies, member search index, variable-size pair fields and LSH indexes, and checks full-schema projected row/CSV payloads with a runtime export limit. Failed final-copy reservations clear groups, pair/member associations and counts together. Loss of exact or near coverage is reported separately.
- Exact groups use cached distinct affected counts; final-result UI updates use those counts instead of rescanning every membership.
- Coverage distinguishes exact-eligible, exact groups/affected URLs, near eligible/short/large/resource-limited/not-eligible, candidates, verifications, retained pairs, bucket/candidate/verification/pair caps, cancellation, and per-URL not-compared reasons. `AnalysisState` records Compared, covered by an exact group, ineligible, too short/large, resource limited, search limited, and cancelled separately from extraction eligibility.
- Member-aware search uses one URL → **all group IDs** association per folded member URL; it preserves case-distinct paths and does not replicate giant memberships into every incident pair. UI row search uses compact metadata and the index, not member concatenation.
- Analysis checks cancellation during bounded page loops and between verified candidates. Exact groups already built survive cancellation; near coverage is marked partial.
- The UI starts analysis after the crawl settles, including stopped partial crawls, using a chunked 512-Input snapshot. `pages_epoch` rejects a snapshot if `set_pages` replaces crawl facts. The analysis thread is gated by update installation and cancelled by close or a new crawl.

### UI and exports

- `InternalView::{Urls, DuplicateContent}` and `DuplicateSubview::{SameText, Similar, NotCompared}`. The existing Internal URL table and its saved state are untouched; Duplicate content has its own `ViewState`.
- Same text: representative URL, match count, selected-text chars, capture/scope. Inspector shows group ID, hash, length, scope/body-fallback note, excerpt, and a paginated 25-row member list with copy/open/show actions.
- Similar: both URLs, verified overlap, exact-member counts, stage/scope, token counts, side-by-side excerpts, and expandable left/right exact membership lists. The approximate candidate caveat is visible.
- Not compared: URL, capture/scope, exact eligibility, near state and URL-specific reason.
- Empty states distinguish preparing, running, failed, stopped/cancelled, limited, and ready-but-empty; Similar says **No similar pairs found**, never that no duplicates exist.
- Overview adds only same-text affected URLs and similar-content affected URLs. Counts use unique affected group members, not repeated pair endpoints. Pending/cancelled/limited comparison adds a coverage note and does not produce an all-clear.
- Overview links open the correct Internal subview with a cleared query. Existing page inspectors are not forced to navigate.
- Content exports freeze visible row references and the immutable result, then project rows and encode CSV in a cancellable worker with a conservative full-schema runtime byte limit. The native dialog opens only after preparation; the existing atomic writer and preparation/dialog/write lifecycle gates are retained. Same-text writes one row/member with group ID, representative and **that member's actual scope/stage**; Similar writes one row/pair with exact group references and member counts; Not compared writes URL/reason. Exports include capture stage, scope, policy version, token/text counts, score where applicable, run identity, and coverage/limit reasons.
- Existing inventory and per-tab CSV schemas are unchanged.

## Changed paths

- `src/content.rs` (new), `src/lib.rs`
- `src/crawl.rs`, `src/main.rs`, `src/sitemap.rs`, `src/indexability.rs`
- `Cargo.toml`, `Cargo.lock` (`ego-tree = "0.10"` direct tree access, already a `scraper` dependency)
- `tests/content_analysis.rs` (new), `tests/duplicate_root_regressions.rs` (new), `tests/browser_render.rs`
- `PRODUCT.md`, `DESIGN.md`, `README.md`, `CHANGELOG.md`
- `docs/plans/duplicate-content-detection-implementation.md`, `docs/plans/duplicate-content-detection-fixes.md`, `docs/plans/duplicate-content-detection-fixes-round2.md`, `docs/plans/duplicate-content-detection-fixes-round3.md`, `docs/plans/duplicate-content-detection-final-core-fixes.md`
- `shots/duplicate-content-*` (ignored artifacts)

## Validation

Final checks:

- `cargo fmt --check` — passed
- `cargo clippy --all-targets -- -D warnings` — passed
- `cargo test --lib` — **142 passed, 0 failed, 2 ignored**
- `cargo test --bins` — **89 passed, 0 failed, 5 ignored**
- `cargo test --test content_analysis` — **1 passed**
- `cargo test --test duplicate_root_regressions` — **6 passed**
- `cargo test --test review_regressions` — **12 passed** (first-correction regressions preserved)
- `cargo test --test link_evidence` — **2 passed**
- `cargo test --test extraction_crawl` — **1 passed**
- `CALMTOAD_REQUIRE_BROWSER=1 cargo test --test browser_render` — **32 passed, 0 failed, 0 ignored** on the final run. One intermediate full run hit the pre-existing Chrome startup flake and one content assertion used the wrong case; the focused rendered-content test passed after the fix and the next full run passed 32/32.
- Windows compile (`cargo xwin check --target x86_64-pc-windows-msvc`) — passed.
- Existing performance gate: `scripts/perf-gate.sh` — passed; no scenario exceeded the 15% threshold. `crawl` was reported as informational (+116.8%) alongside the existing calibration-sensitive baseline; audit +3.0%, parse_typical +12.4% and parse_large +7.2% were within threshold. An earlier loaded-machine run reported an audit regression; the settled rerun passed.

Round-three source correction: the metadata-failure early return and final-copy atomic cleanup are present in the current source and covered by the expected-behavior tests in `tests/duplicate_root_regressions.rs`; the earlier implementation report statement was corrected. The final core pass is mapped in `duplicate-content-detection-final-core-fixes.md`. Export lifecycle ownership is now one shared predicate across both exporters and writer completion callbacks.

Focused duplicate-content tests include:
- hidden/excluded ancestors, 520-deep differing text, stage/policy near isolation, per-URL cancelled/search-limited states, and cached 40k-member affected counts.
- inline/block boundaries, main/article/body, hidden/navigation, article tie, exact punctuation/digits/negation, empty/incomplete, noindex eligibility, Unicode tokens, stage partition, too-short/too-large, exact stable IDs, hand-calculated Jaccard, non-transitive pairs, size pruning, giant exact groups without pair materialization, cancellation, deterministic label corpus, and live crawl content eligibility.
- UI tests cover independent Internal subview state, subview switching, same-text/similar inspectors, member and exact-membership access, exports, Overview links, and 880×560 controls.

## Benchmarks

Ignored local measurement: `cargo test --lib content::tests::benchmark_deterministic_10k_and_50k_corpora -- --ignored --nocapture`.

```
10000 pages: extract 7.338513458s, analyze 531.034417ms, budget 38 MiB, groups 0, pairs 10000, candidates 10001, verifications 10001, limited 1
10000 pages: cancelled analyze 31.101541ms, cancelled true
50000 pages: extract 35.50096175s, analyze 2.183180875s, budget 191 MiB, groups 0, pairs 10000, candidates 10001, verifications 10001, limited 3
50000 pages: cancelled analyze 167.043625ms, cancelled true
```

The corpus deliberately creates 100 similarity clusters with varying pages; exact groups are exercised separately by the giant-group and stage tests. At 50k, exact coverage remains complete for eligible pages in this corpus while near results hit the explicit 10,000-pair cap. Retained content-analysis budget is 191 MiB of the 512 MiB allowance. The same benchmark under `/usr/bin/time -l` reported a 365,940,736-byte maximum resident set size (about 349 MiB), including transient HTML/token construction; the 512 MiB ledger remains the retained-evidence/index accounting, not a claim that process RSS cannot exceed it transiently.

UI snapshot/poll measurement: `cargo test --bins content_snapshot_trigger_and_poll_stay_within_the_budget -- --ignored --nocapture`:

```
content snapshot 50k: trigger 704.292µs, first poll 78.709µs
```

Large-result completion measurement: 40,000-member exact group with 200 pair repetitions processed in ~0.31ms Same / ~0.14ms Similar. The 50,000-member diagnostic export measured a ~2.78ms trigger and ~238ms background projection/encoding. Cancellation was **31.1 ms at 10k** and **167.0 ms at 50k**, both under the 250 ms target. Analysis remains off the UI thread; preparation is 512 inputs/poll.

Labeled candidate fixture: exhaustive **1** verified pair and candidate retrieval **1** verified pair; every reported pair is verified by the Jaccard check. This is a small fixture, not a claim of theoretical recall.

## Artifacts

- `shots/duplicate-content-{880x560,1180x740}-{light,dark}.png`
- `shots/duplicate-content-similar-{880x560,1180x740}-{light,dark}.png`
- `shots/duplicate-content-not-compared-{880x560,1180x740}-{light,dark}.png`
- Existing inventory/link/extraction screenshots remain representative of unchanged surfaces.

No old screenshot batch was loaded into the session; these are file artifacts only.

## Remaining risks and limitations

- Near candidate retrieval is approximate. The UI says “No similar pairs found,” not “No duplicate content exists.” The small labeled corpus measured 1/1 recall; no universal recall probability is claimed.
- Exact equality is hash-based equality of the normalized selected subtree, not byte-identical HTML and not a collision-free proof. Hash-based detection is explained in the evidence panel.
- The selected subtree heuristic is not Readability. CSS-hidden classes, cookie banners and other non-attribute hiding can escape structural cleanup, especially on body fallback; source mode is not a claim of browser-visible text.
- Unspaced scripts and non-ASCII case folding are documented limitations. Punctuation is significant for exact groups and a token boundary for near groups.
- Near analysis is capped and can be limited at 50k pages; exact coverage remains separate. The 512 MiB ledger reserves evidence, memberships, search indexes, pairs and estimated export copies, but is not an allocator-level measurement of every transient allocation or process RSS.
- Analysis uses a chunked snapshot of `ContentInput` (URL + shared `Arc<ContentEvidence>`). If crawl facts are replaced during preparation, the analysis aborts with a retry message rather than mixing generations.
- Cancellation checks are bounded; a 50k exact pass can take up to the measured ~108 ms before responding, within the target but not instant.
- The content analysis has no persistent history, cloud service, AI, extra requests, or automatic delete/merge advice.

## Hand-back

Review the working tree, implementation tests and screenshot artifacts. Exact-commit CI, performance gate, and release decisions remain with root; nothing has been committed, pushed, tagged, released or deployed.
