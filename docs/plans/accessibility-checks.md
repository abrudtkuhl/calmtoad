# Accessibility checks — implementation brief

Prepared November 22, 2026. Next approved desktop feature after structured-data diagnostics. Build an opt-in, local, automated accessibility audit of the rendered page, with useful evidence and honest coverage. The free desktop product remains fully usable without an account.

## Release boundary and ownership

**Gate update:** User explicitly authorized building after the immutable release commit `5f2226b7e88f8185f7734a6f76fd6b826a20554c` was pushed and its CI started. Root has cleared the source-edit gate and notified calmtoad-eng. Accessibility implementation may proceed locally now; v0.9.0 packaging must use that pinned remote commit. This update supersedes the initial wait-for-acknowledgment condition below. If release source corrections become necessary, coordinate affected files before editing.

The structured-data release is still in progress; GitHub's latest published release was v0.8.0 when this brief was written. This brief is **not** part of that release. Do not change production code, release metadata or its changelog while calmtoad-eng is freezing that release. First obtain its exact release commit and confirmation that source edits may resume. Implementation can start after that commit is frozen; it need not wait for all packaging jobs. Record the baseline SHA in the implementation report and keep accessibility changes out of the release tag. Do not commit, push, change versions, or publish this feature without a separate shipping instruction.

OpenCode owns implementation and focused regressions. Calmtoad-eng owns independent review and broader validation. Root coordinates. Send completed milestones and confirmed findings immediately rather than waiting for a final batch. Avoid simultaneous production-file edits or competing full Cargo runs.

## User outcome and scope

Enable **Check accessibility** in crawl options, run a Browser crawl, then answer: which automated rules found problems, which pages/elements are affected, what evidence supports each finding, and what still needs human review? Keep ordinary crawling fast when the option is off.

- Off by default. Available for Browser mode only. In HTML mode disable the option with the concise explanation “Requires Browser mode”; never silently change capture mode or run an audit of static HTML as though it were the rendered page.
- Audit the same live, isolated document used for Browser capture, after settling and DOM capture, before closing its context. No second navigation or follow-up fetch. Fixed existing desktop viewport; record it with engine version and rule profile.
- v1 checks the main document, including supported open shadow DOM. Set `iframes: false`; frame contents, closed shadow roots, interactive journeys, login flows and alternate responsive states are outside coverage. The iframe element itself may still have findings. Persist this scope even when no frame is observed; never say the entire site was checked.
- Use the pinned axe-core WCAG A/AA tag profile, not a custom compliance checklist: `wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa`, `wcag22aa`. Best-practice, AAA and experimental profiles are deferred. Do not interpret zero violations as full WCAG conformance, legal compliance or an accessibility score.
- No cloud, AI remediation, PDF audit, custom user JavaScript, screenshots, automatic fixes, new browser download, or general plugin framework.

## Dependency and browser integration

Use axe-core **4.14.1**, checked against its official tagged API/source; see [research](accessibility-checks-research.md) for exact sources and any implementation caveats. Vendor the official minified browser bundle with its MPL-2.0 license/notices, source/version URL and SHA-256. Embed it in the Rust binary; no runtime CDN or npm/Node requirement. Do not upgrade silently or use an unversioned download. Preserve required notices and document how to reproduce/update the vendored artifact.

Create a focused `src/accessibility.rs` module for typed facts, limits, projection and grouping. Keep orchestration in `render.rs`, crawl policy in `crawl.rs`, and presentation in `main.rs`. Reuse existing managed-browser lifecycle, cancellation, capture identity and resource interception. Prefer a CDP isolated execution world bound to the main frame so site-owned `window.axe` cannot replace the engine; prove the engine works there on real fixtures. Do not weaken CSP, browser sandbox, TLS or navigation policies to inject it. If isolated-world integration cannot work, report the technical reason before substituting a page-global implementation.

Call `axe.run(document, {runOnly: {type: 'tag', values: PROFILE}, reporter: 'v2', resultTypes: ['violations', 'incomplete'], iframes: false, preload: false, selectors: true, elementRef: false})`; confirm exact behavior against the pinned API. Disabling preload prevents audit-driven cross-origin CSS fetching; record the scope tradeoff. The tag union deliberately selects matching rules even when default-disabled (for example target-size); persist the selected rule inventory and the returned/executed rule IDs separately. Unsupported/error checks remain visible through coverage. Preserve summary counts for all four engine outcomes, but do not retain pass-node DOM inventories or infer passed-element totals from reduced nodes. Only expose safe, bounded engine fields; node HTML is text, never executable markup in the native app.

Verify document/frame identity before and after the scan. A path/query change, redirect, new main document, blocked navigation, resource-limited or incomplete render must not inherit a successful audit from another capture. Findings retained from a partial document may be shown only with explicit partial provenance; v1 may instead decline the scan with a reason. Accessibility failure must not erase successful SEO capture or convert its render state into an SEO failure.

## States and evidence contract

Store accessibility evidence behind immutable `Arc` ownership on `Page`, with a run-owned shared budget. Separate execution/coverage from rule outcomes:

- Execution: Not requested, Requires Browser, Not applicable (e.g. non-HTML/redirect), Pending, Running, Completed, Partial, Timed out, Failed, Stopped, Limited/not run. Stable enums plus an explanatory reason, not prose parsing. A failed capture must not become Not applicable without explaining the capture failure.
- Outcomes: **Violation** is a confirmed automated finding; **Needs review** is axe's `incomplete` outcome, never a confirmed violation or pass. Rule-level needs-review results can coexist with a technically completed scan. Coverage loss is separately visible.
- Zero violations: “No automated violations found” only after successful execution within the declared scope, without hidden coverage loss. For incomplete execution, show retained findings plus the reason; never a green all-clear.
- Page facts: requested/observed URL, capture stage and identity, engine version, rule-profile identifier/tags, viewport, execution state/reason, coverage limits, observed versus retained rule/node counts when known. Unknown totals remain unknown; omitted-tail counts must not be invented.
- Rule/node facts: stable rule ID, title/help text, impact (including Unknown), tags, help URL, result kind, page URL, ordinal, full structured target-selector path (including nested shadow selectors), bounded element HTML and failure/check summary. Preserve target arrays structurally rather than flattening them ambiguously. An empty target or missing summary gets an explicit fallback, not a fabricated selector.
- Repeated elements on one page remain distinct occurrences. Across pages, group by rule ID and result kind; report unique affected URLs and retained occurrences separately. Do not group by selector alone or sum overlapping rule page counts into unique affected pages.

## Bounded work, cancellation and retention

Do not repeat the export/snapshot ownership mistakes found in structured data. Reservations must belong to actual allocation lifetimes, including detached cancelled workers, prepared CSV, native save dialogs and write threads. Use a feature-specific shared retained-evidence budget; do not spend the structured-data ledger.

Initial v1 ceilings (named constants, documented/tested):

- Keep existing Browser render deadline at 15 seconds. Audit gets at most **10 additional seconds** after successful render capture, under an absolute per-job maximum of 25 seconds plus the existing bounded cleanup. Timeout is host-enforced: dropping a Rust future or JS Promise does not stop synchronous page work. Dispose the page/context on cancellation/timeout; prove the next crawl can launch cleanly. Do not extend other pages' deadlines or increase the browser concurrency beyond two.
- Preflight at most **20,000 elements**, counting supported open-shadow descendants with a bounded traversal. Larger documents get an explicit Limited/not-run result. This limits audit input; it is not a claimed hard bound on axe's internal browser-process memory.
- Retain at most **128 rule results and 200 affected-node occurrences/page**, **10,000 occurrences/run**, and **64 MiB/run** for accessibility evidence, caches, index copies and exports combined. Rule/node drops and budget exhaustion must remain visible as partial coverage. Keep counting only when it can be done within the bounded scan; otherwise mark unknown remainder.
- Max projected browser-to-Rust accessibility payload **512 KiB/page**; perform bounded projection in the browser before CDP serialization rather than transferring full raw axe results. Keep only needed fields, check encoded-byte size, and shrink or return an explicit limited result before transfer. Host decoder must independently reject oversized/invalid payloads.
- Field caps: target selector components 2 KiB each and 8 KiB/occurrence in aggregate; HTML snippet 2 KiB; failure/help description 4 KiB; identifiers 256 bytes; help URL 2 KiB. All truncation is UTF-8 safe, flagged and exportable. Do not silently lose complete rule identity; reject/limit an invalid oversized identity.

The bundled engine can materialize internal results before projection; do not claim that projection caps make engine execution a 64 MiB process cap. Document measured browser/Rust memory and containment limitations. Additional network work must not be introduced by the audit: block audit-originated requests and prove no CDN, context, help-page or iframe fetch is added. Existing page scripts may continue making requests under the existing interception policy.

HTML-mode/off path must not launch a browser, inject the engine, reserve a large audit buffer, or iterate audit result structures. Per-page thin state is acceptable. Stop, new crawl and window close cancel owned work promptly; stale outcomes cannot mutate the next run or another active view.

## UI and exports

Keep the existing top-level tab count. Add **Accessibility** to the Internal view menu alongside URLs, Duplicate content and Structured data. Use the existing compact table, inspector, search, All/Findings/Incomplete controls and keyboard flow.

- **Pages**: every captured URL, audit state, violation count, needs-review count, maximum known impact and coverage. Counts must say retained/limited where applicable. Findings includes confirmed violations; Incomplete includes execution/coverage gaps and pages with needs-review results. The two sets may overlap.
- **Rules**: one row per rule/result-kind group, title, impact, unique affected URLs and retained occurrences. Inspector lists affected pages and bounded occurrence pagination; selecting a page opens the evidence for that page. Needs review is visibly distinct from Violations. Keep navigation within captured data; “Open page” opens externally without pretending to highlight the old DOM live.
- Search all retained URL/rule/title/selector/diagnostic fields using bounded field-wise matching. No silently truncated aggregate search prefix. State, search, sort and selection remain independent by subview. Large grouping/filtering/sorting runs off the UI thread with generation and active-view guards.
- Overview adds separate nonzero “Accessibility violations” and “Accessibility needs review” affected-URL rows, plus coverage when an enabled scan failed/was limited. Disabled scans do not create a scary incomplete group. A small persistent scope note in this view is enough; avoid repeated disclaimers, cards and instructional paragraphs.
- Export Pages coverage and occurrence evidence as separate explicit CSV actions using current visible scope/order. Include outcome/state, rule ID, impact, page/selector/HTML/reason, engine/profile/viewport, coverage/truncation, and saved run context. Rules export expands visible groups to occurrence rows, preserving group then occurrence order. Pages with no retained node still appear in the coverage export.
- Follow existing frozen snapshot, exclusive export, cancellation and atomic-save behavior. Estimate repeated context, JSON selector escaping and CSV escaping conservatively and enforce bounds at runtime. Preserve leases until worker/output/dialog/write ownership ends. No warning-only allocation or partial CSV silently presented as complete.

## Acceptance and validation

OpenCode adds meaningful focused regressions during implementation; calmtoad-eng independently verifies and runs required broader checks on stable milestones. No live public-site test dependency.

1. Real Browser loopback fixtures: known image-alt/name/label violations; no-violation document; incomplete/needs-review rule; script-inserted element; open shadow root; iframe excluded with declared scope; page-provided fake `window.axe`; restrictive CSP; unexpected navigation during scan. Verify actual pinned engine/version/profile, not only a mocked parser.
2. Policy/off tests: HTML/off does not inject or make audit requests; redirects, non-HTML, failed/truncated capture get truthful states. Audit timeout/failure leaves SEO capture intact. Document identity change rejects stale findings. No iframe execution/network expansion.
3. Cancellation and limits: delayed/stuck engine, Stop/new crawl/close, next-crawl recovery, huge DOM, large result payload, each page/run/field cap, missing/null engine fields and Unicode truncation. Gate workers to verify ledger occupancy while cancelled allocations are still live.
4. UI tests: independent Pages/Rules state; violation versus needs-review membership; distinct affected URL and occurrence counts; overlapping rule counts; navigation away during worker completion; complete field search; correct filtered-empty states and inspector selection.
5. CSV tests: coverage-only rows, nested target arrays, repeated rule occurrences, long repeated run context, control characters/quotes/newlines, escaping expansion, frozen order while results arrive, cancelling prep/dialog/save and competing memory reservations. No silent row omission.
6. Native offline scenes for Pages/Rules/evidence/disabled/partial states; inspect 880×560 and 1180×740 in light/dark themes. Measure populated 50k-URL UI separately from actual Browser audit throughput; normal UI interactions target under 8 ms, with bounded worker polling. Report audit latency/memory, payload/ledger peaks and measured limits rather than unsupported performance claims.
7. Required repository checks: fmt, Clippy, full tests including real-browser fixtures (`CALMTOAD_REQUIRE_BROWSER=1`), and local performance gate for crawl/render/audit changes. Do not change its baseline or threshold to get a pass. Add vendored-asset hash/version/license checks that work offline.

Update PRODUCT, DESIGN, README/CONTRIBUTING as needed and Unreleased changelog after the release boundary clears. Write an implementation report naming baseline SHA, dependency provenance, profile, limits, each acceptance check actually run and any failures/deferrals. Findings go back to OpenCode incrementally; release only after independent clearance and user authorization. This brief's explicit v1 limits override the research note's exploratory alternative time/node/payload proposals; do not combine both sets of limits.
