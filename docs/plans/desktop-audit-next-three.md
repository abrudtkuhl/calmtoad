# Desktop audit expansion: next three features

November 17, 2026. Authorized by the user: plan and have the existing OpenCode agent build the next three desktop priorities. Baseline: released v0.6.0, commit `700ab0d`. Implement all three stages in order, with reviewable checkpoints. Do not stop after the first stage. Keep changes local; no version bump, commit, push, tag or deployment in this implementation handoff.

## Product outcome

An auditor can identify the exact links that need attention, export a complete factual content inventory, and collect site-specific fields without leaving the free desktop product. Keep the current compact native UI, Overview, saved-run provenance and honest incomplete states. No cloud account, AI recommendations, persistent crawl history, new browser automation language, or redesign.

Read PRODUCT.md, DESIGN.md, applicable repository instructions and the Impeccable craft floor before UI work. The historical v0.5/v0.6 plans describe shipped behavior, not outstanding blockers. Preserve it.

## Current implementation and shared design

- `src/crawl.rs`: Page.links is a same-host URL list. parse_html_staged parses HTML once; scheduling currently drops robots-blocked, external and asset links, including PDFs. Browser captures retain source and rendered HTML transiently, and use source links only when rendered DOM is absent. Keep that stage distinction.
- `src/render.rs`: managed isolated browser, bounded resources/captures and cancellation. Reuse its capture policy; do not attach to a normal profile, relax TLS/sandbox controls or alter indexing evidence semantics.
- `src/main.rs`: existing Links inspector has inbound sources, outgoing targets, observed broken destinations and redirects. Cached relations, bounded lists, search/filter/sort and export scope are already shipped. Extend them.
- `src/export.rs`: shared UTF-8 CSV quoting and spreadsheet-formula protection. Reuse it for every new exported field, including rule names and crawl settings. Existing public CSV schemas stay compatible.
- `src/update/settings.rs`: local preferences have backward-compatible defaults. Extraction rules may use a separate versioned local file to keep updater preferences small.

Put behavior behind small library module interfaces used by both UI and tests. Suggested modules: link evidence/checking, inventory export, extraction. Final names are implementation choices. Do not grow three independent implementations inside main.rs or introduce interfaces with only hypothetical adapters. Accept network dependencies at the existing engine seam; pure extraction/export logic should return data rather than doing network/UI work.

A run snapshots link-check settings and extraction rule definitions alongside current seed, limits and rendering settings. Next-run edits never relabel, reinterpret or overwrite retained results. Streaming event changes must stay within the existing frame budget. Cache derived rows/counts; no reparsing, selector compilation or network calls during view rendering.

## Stage 1 — finish actionable link diagnostics

### Evidence

Retain individual anchor occurrences independently from scheduling. Each record needs source page URL, raw href, resolved HTTP(S) destination, fragment if present, anchor text, capture stage, and a stable occurrence order. Keep duplicate occurrences with different anchors/positions; deduplicate destination requests only. Preserve empty anchor text as empty; do not call it an accessibility name. Nested text and image-only anchors must not invent a label.

Resolve relative links against the applicable document base. Apply current URL-key semantics consistently for joins; keep the actual requested/resolved URL available as evidence. Exclude non-HTTP(S) schemes from network checks. Same-document fragments are contextual links, not independently fetched resources; do not claim fragment-ID validation. Malformed hrefs are explained as invalid, not fetched or silently marked healthy.

Scheduling eligibility and observed evidence are different facts. Retain links skipped by robots, depth/page budget or scope, with a reason, without scheduling those targets. Existing inbound/outbound counts must keep their documented semantics; label new occurrence versus unique-target counts clearly. Source fallback links on an incomplete Browser capture stay source evidence and never become completed rendered observations. Do not merge source and rendered occurrences as though both exist in one DOM.

Bound retained occurrence data: initially 10,000 occurrences per page, 250,000 per run, 2,048 Unicode characters of anchor text and 8 KiB per raw/resolved URL. Use UTF-8-safe truncation. Mark affected pages/run evidence as limited and distinguish an omitted tail from a complete link inventory. A cap on optional evidence must not silently truncate ordinary page discovery or change an existing complete capture into a fabricated failed fetch. Keep cap accounting bounded and deterministic; document any necessary performance-driven adjustment.

### Destination checking

Internal targets already fetched by the page crawler reuse that response evidence. Never issue duplicate requests just to populate the new table. An unchecked internal page remains unchecked if robots/depth/page limits prevented a fetch; the new checker must not bypass those limits.

Add opt-in **Check external links and documents** in a compact crawl-options disclosure, default off. It checks observed HTTP(S) external anchors and skipped same-host document downloads such as PDFs/Office files/archives, without crawling their contents, extracting PDF text, following links from those documents or turning their responses into Page rows. Ordinary image/script/font assets remain excluded. Document the actual extension rules; no content guessing presented as fact.

Initial fixed guardrails for this opt-in pass:

- 1,000 unique new destination URLs per run, including redirect destinations in the budget; no recursive link discovery. Redirect targets already known reuse observations.
- At most two requests globally and one per origin, at least one second between starts per origin, honoring a longer existing crawl delay or Retry-After. Share scheduling/rate policy with internal work where origins overlap.
- One bounded GET response for status/headers; never download the document body. Disable automatic redirects and decompression/body buffering; explicitly manage at most five observed hops under the same scope/check policy.
- Ten-second per-request deadline and 30-second total per-destination chain deadline. Stop/app close interrupts queued and in-flight work, including delays, promptly. Do not add retries that multiply these caps.
- Respect applicable robots rules for each newly contacted origin using bounded/cancellable retrieval and existing parser/failure policy. Cache per-origin results. Robots retrieval: max 256 KiB decoded, ten-second deadline, at most five redirect hops with a bounded number of origin transitions. Expose failures or refusal as unknown/blocked, not confirmed broken.
- No implicit credentials, browser cookies, login flows or authenticated browser profile reuse. Reject embedded URL credentials rather than exporting/fetching them. Keep local/intranet crawling possible; do not introduce unrelated host restrictions.

Make the check phase and its remaining/limited coverage explicit while preserving the page crawl's own terminal outcome and budget. Do not claim the entire run finished while optional checks are active; Stop must apply to the whole run, and updates remain blocked until all workers stop. Capture partial results when stopped. Separate destination response facts (2xx, 3xx, 404/410, 403, 429, 5xx, transport/TLS failure) from the verdict. Label restricted/rate-limited/unavailable as such; a 403 is not proof of deletion, a redirect is not inherently broken, and a timeout is not a 404.

### UI and exports

Reuse Links with compact **Pages / Link evidence** views rather than another top-level tab. Pages preserves existing behavior. Link evidence lists source, destination, anchor and observed state, with search, state filtering, sort, source/destination copy/open and in-dataset navigation. Keep independent view state and bounded/virtualized rows. The inspector shows provenance, skip/check reason and observed redirect chain. Multiple sources to one destination remain inspectable/exportable.

The Link evidence CSV is one row per retained occurrence and includes source, raw/resolved destination, fragment, anchor, stage, check state/reason, observed status, final known URL, redirect chain and evidence-limit indicators. Export precisely the visible sorted evidence rows. Preserve the current page-level Links CSV when Pages is active. Incomplete evidence and unchecked destinations cannot export as healthy. Overview counts remain explicitly per-page or per-destination; do not mix units or sum overlapping groups.

### Stage acceptance

Fixtures must cover repeated destinations from multiple sources/anchors, base-relative links, fragments, invalid/scheme links, source fallback, blocked and budget/depth-skipped links, PDFs/external HTTP checks, per-origin robots/rate policy, redirects/loops/budget exhaustion, restricted/rate-limited/server responses, cancellation and evidence caps. Assert request counts and no recursive external fetches. Verify opt-in off preserves existing network behavior and existing link/page CSV schemas. A link source must remain discoverable in UI and CSV even when its destination was requested only once.

## Stage 2 — complete content inventory export

Add **Export inventory CSV…** accessible from Overview and Internal through a compact export choice. Keep existing **Export CSV** semantics intact. Default inventory scope is **All captured URLs**; offer **Visible Internal URLs** explicitly, using that table's actual saved search/filter/sort. Never inherit an unrelated Titles/Links/Sitemaps filter invisibly. Dialog/feedback names scope and row count. Do not add a permanent toolbar of export buttons.

One row per captured Page URL in deterministic order, without requiring spreadsheet joins. Include:

- URL/stable join key, actual response status/error, content type, actual capture mode/completeness and reason.
- Full title, description, all H1s (JSON array in one quoted cell), known word count, crawl link depth, existing inbound/outbound count semantics, canonical target and indexability label/reasons, and existing per-check findings in clearly named columns. Distinguish absent/unmeasured values from zero. A missing title on a PDF or failed fetch is not a missing-title finding.
- Link-evidence/check completeness and confirmed finding context from stage 1, without treating unknown destinations as confirmed broken links.
- Run seed, start timestamp, effective page/depth limits, sitemap option, link-check settings, run terminal reason and relevant coverage/limit counts. Repeat these columns per row so the single CSV is self-contained. Snapshot actual run settings; never read next-run controls at export time.

List the concrete columns and units in docs and lock their ordering with meaningful golden/round-trip coverage. Use JSON arrays for multi-value lists to avoid ambiguous ad-hoc separators. Reuse shared CSV protections, support Unicode/newlines/quotes/formula-leading text, and preserve a stable URL key. Human ownership, agency, content disposition and recommendations remain user annotations, not inferred fields. Export only available evidence and explicitly report coverage; the inventory does not claim it contains every site/CMS URL.

Build/export from a coherent immutable snapshot when invoked during streaming; identify an in-progress snapshot as in progress. Large exports must not freeze the UI or build redundant full-dataset clones; use a worker/streaming writer where needed, preserve safe cancellation and report errors. An empty dataset has a clear disabled/explanation state, not a misleading successful blank inventory.

### Stage acceptance

A mixed crawl yields one row per captured URL with actual metadata/headers/canonical evidence and true null/zero distinctions. Completed, limited, stopped, incomplete and in-progress runs retain correct context after draft settings change. Verify All versus Visible scope, ordering, formulas/escaping, non-HTML rows, absent source evidence and link coverage. Existing CSV exports remain byte/schema compatible except previously approved fixes. Verify a 10k/50k synthetic export stays responsive and memory bounded; measurements are local, not CI performance gates.

## Stage 3 — custom extraction with CSS selectors

First version: named CSS-selector rules, with **Text** or **Attribute** output, and **First / All matches**. No XPath, regex language, script execution, JSONPath, custom JavaScript or AI. Reuse the existing scraper parser/selector support, confirming supported selectors against the actual dependency. Do not claim browser-engine CSS parity where scraper does not provide it.

Rule definition: stable ID, unique nonempty display name, selector, output kind, optional attribute name, and match mode. Compile/validate before starting any crawl or preview; errors are field-adjacent and preserve retained results. Enforce max 20 rules, names 80 characters, selectors 2,048 characters and attribute names 128 characters. Reject duplicate case-insensitive names and ambiguous reserved export names. Compilation happens once per run, never once per page/frame.

Evaluate on the same selected HTML/DOM snapshot that supplies content audits. In HTML mode use captured document HTML; Browser uses retained rendered HTML. If rendered DOM is unavailable, report that stage unavailable; do not silently substitute source HTML for a rule configured against rendered content. Keep capture stage and incompleteness on every result. No network refetches per rule and no retention of all page HTML solely for extraction.

Return typed outcomes that distinguish matched values, no match, matched element missing the requested attribute, not applicable non-HTML response, unavailable/partial capture, and value/match/output limits. Empty attribute/text values are distinguishable from a missing attribute or no element. Preserve usable observations on partial documents but label them partial rather than complete/no-match. Rule matches are not SEO defects by default.

Evaluate in document order. Text is normalized descendant text with script/style/template content excluded; call it **Text**, not computed visible text or accessible name. Attribute mode retains the literal attribute value; it must not silently resolve relative links or infer dates/ownership. All matches preserves duplicates and order. First is a deliberate first-match result, not truncation.

Initial caps: 100 matched values per rule/page, 4,096 Unicode characters per value, 64 KiB total retained extracted value bytes per page, 32 MiB per run. Enforce UTF-8-safe bounds without collecting unlimited strings/matches first. Report per-rule/page/run limits. Once the run cap is hit, later extraction outcomes explicitly say not evaluated because of the cap; ordinary crawling continues. Bound selector/rule work using the existing bounded document capture, rule count and output limits; record any unsupported expensive selector issue rather than hiding it behind UI waits.

### Configuration, preview and results

Use a compact **Custom extraction…** entry in crawl options. Editor supports add/edit/remove, validation, rule count and local persistence. Draft rules are saved between app launches in a versioned, backwards-compatible format; malformed rules/files produce a recoverable message and never prevent normal crawling. No cloud requirement. Run details show the frozen rules used for retained results; editing draft rules only affects a new crawl.

Provide **Test on URL** with an explicit URL (prefill draft seed), current HTML/Browser mode, current rules, and visible progress/Stop. It captures just one page through the same capture, scope/robots, timeout/resource and cancellation policy, previews values and statuses, and never recursively crawls. Validate before network work. Disable preview during an active crawl and prevent parallel previews; preview cannot mutate the existing crawl or pretend to recalculate old results. No hidden background fetch on keystrokes. A preview status says its actual capture mode and limitations.

In the Internal inspector add a compact disclosure showing each saved rule, its values, stage and state; full bounded values can be copied. Keep long lists paged, with access to every retained match. Do not add another cramped top-level tab. Stage 2 inventory appends deterministic custom columns from the run snapshot: use the stable rule ID in the column key, with separate name, JSON values, state, stage and limitation/reason columns. Export rule selector/output/match definitions as run-level JSON in a quoted CSV column so results are reproducible without external files. Empty/missing/partial/limited states must survive export. Current per-tab exports keep their existing schemas.

### Stage acceptance

Test text and attribute/First/All, nested text, empty versus absent attribute, no matches, Unicode, duplicate matches/order, relative literal attributes, invalid selector/name, rule reorder/rename/IDs, malformed saved configuration, cap exhaustion and partial/unavailable/non-HTML captures. Use real Browser fixtures where scripts change matching values; verify source and rendered provenance do not blur. Test preview cancellation, robots refusal, failure, no recursive requests and retained-crawl isolation. Changing draft rules after a crawl must not change its displayed/exported values or definitions. The final inventory contains all three feature stages together.

## Build sequence, verification and hand-back

1. Inspect current code/instructions and write a short implementation note if the actual seams require adjustment. Implement link evidence/model and pure checks first, then bounded opt-in network work, then Links UI/export. Checkpoint with exact paths and focused test results.
2. Implement the shared inventory projection/writer and export UX. Checkpoint with a representative generated CSV path and scope/correctness checks.
3. Implement extraction rule module, capture integration, persistence/editor/one-page preview, inspector and inventory integration. Finish all three before declaring the request complete. Do not claim blocked waiting for feedback between stages unless an actual unresolved decision prevents progress.
4. Keep all existing cancellation, source/indexability evidence, non-HTML completeness, sitemap, saved-run context, large-crawl responsiveness and signed-update behavior intact. New browser lifecycle tests must hold the existing SERIAL lock when sharing the process-wide browser registry; v0.6.0 fixed precisely this race. No speculative broad renderer/runtime rewrite.
5. Run focused meaningful module/engine/UI regressions as each stage changes. At the final integrated state run fmt, all-target Clippy with warnings denied, unit/UI suites and browser integration with CALMTOAD_REQUIRE_BROWSER=1; clearly distinguish real-browser runs from skipped tests. Check Windows compilation and supported-platform cfg paths; do not claim native Windows/Linux UI walkthroughs from a Mac. Do not add a CI performance gate or reset committed baselines. Repeat broad checks only after changes/failures justify it.
6. Inspect actual UI/interaction states at 880×560 and 1180×740 in light/dark: link evidence and long/empty/limited rows, optional checks off/on/running/stopped, export scope, extraction editor errors/preview/results, and unchanged default Overview. Follow Impeccable/native design constraints: minimal chrome, no repeated explanations, no extra card grid or nested bulk. Include keyboard access and accessible names.
7. Update PRODUCT.md, README/help and CHANGELOG Unreleased for actual behavior. Create `docs/plans/desktop-audit-next-three-review.md` with checkpoint status, design tradeoffs, changed paths, representative CSV and screenshot paths, actual test commands/results, measurements and concrete remaining limitations. Do not mark unimplemented acceptance criteria done. Report completion to the user/session for independent review before shipping.
