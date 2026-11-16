# Desktop workflow optimization

Status: analyzed and authorized for OpenCode implementation. November 14, 2026.

## Outcome and scope

Make the free desktop crawler faster to operate, clearer about what it checked, better at explaining a finding, and easier to act on. Optimize the complete loop: configure → crawl → locate → understand → act → export. Preserve the current restrained native table-first design and the completed UI polish; this is an interaction/information pass, not another visual identity.

OpenCode owns implementation. Root supplies this analysis and will review the result. The working tree contains uncommitted browser-rendering correctness work and UI polish against HEAD 9031fe8: preserve all of it. No version bump, commit, push, tag or deployment in this task.

Do not add cloud analysis, AI plans, accessibility/structured-data engines, custom extraction, persistent crawl history, external-link crawling, CMS editing, or a large dashboard. Use facts the crawler already captures. Existing future feature plans remain separate. A few narrowly scoped read-model helpers are appropriate; no wholesale app/runtime rewrite.

## Evidence from the current interface

Method: source review of src/main.rs and crawl/indexability/sitemap models; fresh offline renders of Titles, Meta, H1, Response, Canonical, Images, Links and Sitemap Files at 880×560; prior current-build renders cover Internal, Browser setup/details/evidence, empty/loading and update states at 880×560 and 1180×740 in both themes. Images are in shots/ux-audit-* and shots/ui-polish-*. This is fixture/source analysis, not a claim that a new human usability study or performance profile has run.

- State has no result query/search. refresh_visible and refresh_sitemap_visible only support Issues only. Finding a URL or issue in a long crawl requires scrolling/exporting.
- detail always leads with indexability and the same generic fields. A Titles duplicate, H1 Multiple, broken link, image-alt count and redirect all open essentially the same inspector. Full H1 list, duplicate peers and related link targets are not brought forward.
- Tab counts mix semantics: Internal counts crawled URLs, content tabs count finding URLs, Sitemaps counts listed URLs. Clicking Titles 3 still shows all nine fixture URLs unless Issues only is enabled; that distinction needs to be apparent.
- Filtering to zero visible rows retains a blank table; there is no specific no-match/no-findings recovery. Selection is stored independently of filters, so the inspector can keep showing a row outside the visible set. On crawl completion selection is assigned from pages.first even in Sitemap Files.
- Invalid numeric settings silently fall back/clamp while their displayed text can remain different. URL submission errors only change the general message; with existing results the error is easy to miss. start_crawl itself does not share the browser-readiness guard used by the button.
- Starting a new crawl clears the previous dataset after URL normalization. Validate every setting before mutating results, and keep the run's actual settings separate from later edits to the next-run controls.
- poll_crawl drains all available channel events in an unbounded loop; view recomputes counts_with over the dataset, and sort_pages physically sorts Page records. These are scaling risks to measure, not claimed profiled bottlenecks. Keep the existing incremental Audit and virtualized table.
- Current stop/coverage reasons and raw evidence are valuable. Do not simplify them away. Some zero counts in the general details/Internal rows still look measured even when that content was never captured.
- Small screenshots for content tabs are seeded by fixture functions that assign tab after loading/sorting. Make final fixtures follow the same view-transition path as real interactions; do not infer a production sort bug solely from their current ordering.

## Implementation priority 1 — trustworthy setup, run state and result state

### Setup and run context

- Keep URL + Crawl/Stop unmistakable and preserve compact Pages, Depth, HTML/Browser and Sitemaps controls. Add concise contextual help for HTML versus Browser and depth 0; do not add a tutorial or paragraphs to the command bar.
- Validate URL and limits consistently in the shared start action, including keyboard submission. Pages must be an integer greater than zero; depth an integer from 0 through 10. Allow intermediate text while editing, but no silent fallback or hidden clamping on submission. Show a concise field-adjacent error and leave existing results intact until a valid crawl starts. Focus the first invalid field if the toolkit supports it.
- Apply browser availability and already-running guards to every start path, not just the visual button. No rejected action may clear results or spawn work.
- Snapshot host/seed, effective limits, capture mode and sitemap setting for each run. The results' context must describe that run, even after a user edits the next URL/options. Avoid inferring domain state from presentation strings where a small typed state is possible.
- Give preparing, crawling, stopping, completed, stopped, limited and failed states accurate concise feedback. After Stop, label/disable the stopping action appropriately until the worker is finished. Updates must remain unable to install during active/stopping work.
- The page cap is a budget, not an estimate of the site's total or a completion percentage. Do not label cap consumption as percent of site crawled. Do not invent queue totals or phases the engine has not reported.

### Counts, coverage and empty states

- Use one compact results summary, not metric cards. Show visible count versus view total, finding count and incomplete count where useful; make the basis clear without repeating numbers across several bars. Counts refer to URLs, not the sum of overlapping individual findings.
- Tab accessible labels/help must distinguish finding URLs, crawled URLs and listed URLs. Preserve informative non-HTML context without making it an issue. Incomplete, known finding and informational absence are different states; reuse the shared assessment/row_issue semantics.
- Keep stopped/limited/partial coverage visible and inspectable, including Sitemap Partial and file errors. Provide click/keyboard-accessible run details with full settings/reason when the compact summary truncates; essential coverage cannot depend only on hover.
- Differentiate no crawl, preparing, failed crawl, empty sitemap discovery, no findings in this view, and no matches for active filters. Add Clear search / Clear filters actions for filtered-empty states. Zero findings among checked pages is not proof the entire site is clean, especially with incomplete checks.
- Keep the inspector tied to a visible selected row. When filters/view changes hide the selected item, select a sensible first visible result or clear the inspector; never leave a stale hidden selection. A Sitemap Files view must select a file, not an arbitrary Page URL.
- Preserve a selection by stable URL identity during new results/sort operations. Do not automatically jump the user back to the first row at completion if their selected result is still valid.

## Implementation priority 2 — fast search and filtering

- Add a clearly labeled compact search field in the results toolbar, only when a dataset exists. No new permanent sidebar. It must work while streaming results arrive.
- Case-insensitive literal search across URL, the current view's principal text (title/description/H1/canonical/type/source) and displayed finding text. Document the actual search scope in concise contextual help. Do not imply arbitrary full-body search, regex or a query language.
- Provide a compact check-state selector with All, Findings and Incomplete (or equally clear wording). Preserve the familiar Issues only capability; avoid two conflicting toggles for the same state. Do not quietly change which assessment severities count as findings.
- Store view state deliberately: switching tabs and Sitemap URLs/Files should restore their query/filter/sort rather than re-sort and forget context every time. A new crawl should clear transient result searches/selections so a fresh run is not hidden by an old query; preserve intentional crawl options. Make applied filters visible and easy to clear.
- Keep sort deterministic, severity-first by default, and user-controlled thereafter. Share the exact visible/sorted row set with CSV export and Copy visible URLs. Query/filter must never change underlying crawl totals or evidence.
- Cache normalized query/searchable facts or debounce sensibly as measurement warrants. Do not lowercase/rebuild every page field during every frame or keystroke. Do not block ordinary editing in order to run a network action.

## Implementation priority 3 — a contextual inspector that explains and supports action

Lead with the selected tab's finding, its evidence and a concise next step. General page fields and indexability remain accessible, but unrelated indexability guidance must not precede every task.

| View | Inspector lead and useful evidence/action |
| --- | --- |
| Internal | Assessment, important reasons, HTTP/capture completeness; retain staged raw evidence and source restrictions. |
| Titles / Meta | Full value, measured character count, exact existing check result. For duplicates, list other matching observed URLs using the same normalization semantics as the audit. Offer copy value and inspect/open related URL. Describe length ranges as this tool's heuristics, not a guaranteed search-engine limit. |
| H1 | Every captured H1 in order, count, existing finding and duplicate peers. Do not show only page.h1() when the finding is Multiple. |
| Response | Actual HTTP status/error, redirect destination and observed known chain/loop information. Allow inspecting a fetched target or opening/copying it. Unchecked target status stays unknown. |
| Canonical | Current assessed declaration(s), source/rendered changes, target observation and what needs review. Reuse stage-aware assessments; never create cross-stage conflicts again. Make the target actionable. |
| Images | Observed image/alt counts and incompleteness, plus concise guidance. The current model does not retain every image URL/element: do not invent an element-level list. Explicitly leave that richer audit for a future data change. |
| Links | List observed incoming source URLs, outgoing target URLs, and known broken targets/statuses from Page.links + fetched pages. Label scope as links observed in this crawl, not a complete site backlink map. Deduplicate list entries deliberately while respecting existing count semantics. |
| Sitemap URLs / Files | State/reason and next action first: inspect a fetched page, inspect source sitemap, open/copy a file/target, or explain robots/scope/page cap. Skipped/unchecked is never healthy. Maintain independent bounded scrolling. |

- A next step is grounded deterministic UI guidance, not an AI site action plan. A canonical preference is not an indexing block; a missing canonical is informational; 403 is not proof of deletion; nofollow alone is not noindex. Preserve all existing correctness rules.
- Related results use the same stored evidence, without new network requests. Reuse/expose narrow read helpers from the existing audit where necessary instead of duplicating normalization/assessment logic in presentation strings.
- Jumping to a related in-dataset URL must show that URL and update the inspector. If filters would hide it, explicitly reveal it/clear conflicting filters and indicate that change. For unchecked/out-of-scope URLs, offer open/copy, not a fake Inspect result.
- Partial fields can remain inspectable as partial observations. Unknown counts/fields show an em dash or explicit incomplete state, not a measured zero or OK. Keep actual observed zero distinguishable from missing data.
- Keep raw evidence, scope, capture stages, error/cap reasons and technical guidance in expandable sections. Show the primary reason without requiring expansion. Avoid duplicated paragraphs between summary, guidance and capture sections.

## Implementation priority 4 — faster direct actions and keyboard flow

- Keep copy URL and open-in-browser easy to find. Add copy full field/evidence value where that saves transcription. Full values must be recoverable when table cells truncate; do not rely on a tooltip alone.
- Add Copy visible URLs beside/within the existing export action, preserving current view/filter/sort. A small action menu is preferable to a permanent row of extra buttons. CSV export must still export exactly the visible ordered dataset and accurately describe its scope in the save prompt/success notice.
- Include Previous/Next visible result controls or equivalent verified keyboard navigation in the inspector so reviewing 20 issues does not require repeated mouse travel. Avoid a new multi-selection/bulk-edit system in this pass.
- Verify existing table arrow-key behavior and implement coherent focus handling using the actual Rune API. Add Cmd/Ctrl+F for result search and Cmd/Ctrl+L for the crawl URL where feasible, expose shortcuts in help/native menus, and preserve normal text editing/copy behavior. Escape can close an inspector/disclosure in its context; it must not unexpectedly stop a crawl or clear unrelated work. No invented unsupported toolkit calls.
- Focus, selected row and active result are visible and agree. Buttons have explicit accessible names and state; color and hover alone do not carry meaning.

## Implementation priority 5 — measured responsiveness and layout

- Cache/refresh counts and expensive read-model indexes when their inputs change, not on every view render. Keep incremental audit updates. Stable result ordering should not jump under the cursor while a crawl streams; apply user sort on view updates deliberately and preserve URL identity.
- Bound work drained per poll by a small event/time budget and schedule the remainder promptly so Stop, typing, selecting and scrolling stay responsive during large sitemap/crawl bursts. Preserve every event, final report, cancellation semantics and audit correctness.
- Measure before changing large-data algorithms. Reuse virtualized Table; do not render a native element for every dataset row or every related URL at once. Large relation lists need bounded/virtualized rendering with a visible total and access to the remaining rows, not silent truncation.
- Avoid wide, incidental changes to crawling or parsing. Extract small query/view-state/inspector helpers if needed to keep main.rs understandable; do not build a framework or migrate the app to async.
- Preserve 30px result rows, readable fonts, quiet light/dark themes, the existing small mark, hairline structure and one primary crawl action. No cards, score gauges, decorative metrics, animations, onboarding, or permanently expanded explanations.
- The main URL and selected finding must remain readable at 880×560. Keep long addresses and values accessible via details/copy. Use a verified column-sizing/scrolling strategy rather than shrinking type; closing the inspector must promptly restore table width, and the inspector needs a nearby close control in addition to the toolbar toggle.
- At 1180×740 and wider, use space for meaningful values rather than enlarging chrome. Keep all nine tabs usable and all toolbar actions in bounds, including an active query, filters and update banner.
- Preserve flattened update banners and signed updater behavior. Do not reintroduce duplicate update/status chips or let incidental update copy dominate crawl results. Installation still requires explicit user action.

## Verification and deliverable

Implement in this order: shared UI read model/run validity → search/filter/selection → contextual inspector/actions → keyboard/layout → measured performance and final QA. Complete the full pass locally; do not stop at another plan or only spacing changes.

Add meaningful runtime/pure tests at the behavior seams:

1. Invalid URL/pages/depth and unavailable Browser mode do not clear existing results or start work; visible error and effective settings agree.
2. Search + check-state filter + sort + CSV/Copy visible URLs select the exact same ordered rows in normal and both sitemap views, including case/Unicode, zero results and incoming streamed rows.
3. Selection remains a valid visible URL across filter/sort/tab/view changes and crawl completion. No page selection leaks into Sitemap Files; filtered empty offers recovery without claiming global health.
4. Contextual details show duplicate peers, all H1s, known redirect/canonical targets and observed inbound/broken links. Unknown, non-HTML and incomplete captures never receive invented facts or completed content checks.
5. Related-page navigation and previous/next work with active filters and preserve intentional view state. Clipboard actions use full values. Shortcuts do not hijack typing or trigger crawls accidentally.
6. Poll budgeting yields without losing/reordering events or final reports. Cancellation/update-install guards remain correct.

Run cargo fmt --check, cargo clippy --all-targets -- -D warnings and relevant cargo tests, expanding to the full pure/UI suite for these state changes. Run real-browser tests only if renderer/crawl/evidence behavior changes; do not change renderer behavior merely to satisfy UI fixtures. Honor platform CI requirements and be honest about unverified OS runtime behavior.

Use synthetic offline data to measure query/filter/sort, count refresh, event bursts and selection/render cost at 10,000 and 50,000 URLs. Record environment, dataset, operations and before/after results; do not present parser benchmarks as UI latency or add a CI performance gate. Do not reset committed baselines to hide a regression.

Render and visually inspect all nine tabs plus Sitemap Files, query/filter empty, duplicate/H1/link/redirect inspectors, expanded evidence, browser unavailable, invalid input, preparing/crawling/stopping/limited/failed outcomes and update states. Batch representative captures at 880×560 and 1180×740 in both themes. Exercise mouse and keyboard transitions with Rune Runtime or the actual app; static screenshots alone do not establish behavior. Keep a reproducible large-data fixture and representative offline scenes.

Read DESIGN/PRODUCT and Impeccable craft-floor before UI edits, preserving the existing native design. Update DESIGN.md, PRODUCT.md, CONTRIBUTING scene/help instructions as needed and concise CHANGELOG Unreleased entries for actual resulting behavior. Return an exact change/test/measurement report, final screenshot paths and concrete remaining limitations. No commit, push or release until review and a separate shipping instruction.
