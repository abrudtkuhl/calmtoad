# Desktop UX follow-up: setup, diagnosis, and overview

November 16, 2026. User-requested refinement after v0.5.0 (`b487b89`): analyze the flow and hand implementation to the specifically tagged calmtoad-eng2 task. Keep changes local for review; do not commit, push, bump versions, or release.

## Evidence and direction

User screenshots:

- `/var/folders/_8/2ylyp63n70d6tk_w40frzz300000gn/T/codex-clipboard-LGeCvg.png`: the same indexability status twice, once white and once green.
- `/var/folders/_8/2ylyp63n70d6tk_w40frzz300000gn/T/codex-clipboard-5waxcw.png`: browser picker opens Documents and asks for an executable instead of a normal Mac application.
- `/var/folders/_8/2ylyp63n70d6tk_w40frzz300000gn/T/codex-clipboard-1Mlapv.png`: installed Chrome fails its ten-second version probe, followed by a wall of candidate paths/errors. The user still cannot tell what Sitemaps does.

Preserve the current native, compact Crawl Ledger visual identity. Use the Impeccable polish/clarify guidance and read its craft floor before UI edits. Operate mode: task completion, clear state, familiar OS controls. No card grid, health score, oversized metrics, extra onboarding wizard, new rendering framework, or cloud features.

Implement in this order: browser recovery and duplicate status; sitemap comprehension and result provenance; lightweight default Overview; final whole-flow validation. Send findings back as each slice becomes reviewable rather than waiting for a giant final patch.

## 1. Browser setup should speak in applications, not executables

Observed code: `browser_strip`, `choose_browser`, `probe_browser` in main.rs; `discover_browser`, `normalize_executable`, `browser_version` in render.rs. Discovery currently requires `--version` to exit within ten seconds, and a normal file picker has no initial directory or filters. Its raw aggregate diagnostic is rendered as wrapping primary UI text.

Requirements:

- Reproduce the installed-Chrome timeout before attributing it to a cause. Check the packaged GUI launch environment, an already-running Chrome, cold discovery, and bundle selection. Do not simply lengthen the timeout or call an existing installation missing because its probe timed out.
- Prefer discovered supported applications. Make the selected app clear, and remember an explicit choice between launches using a small compatible preference change. Preserve a stale choice for understandable recovery; do not silently substitute a different explicit executable.
- On macOS the action is **Choose browser…**, the dialog starts in `/Applications`, and a person can select `Google Chrome.app` or another supported application directly. No Show Package Contents, binary path hunting, or typing an executable path. Verify this in the native dialog; path-normalization tests alone do not establish picker usability.
- Rune's existing `FileDialogOptions` supports `directory`, `file_name`, and extension `filters`; `Tx::open_file` and `Tx::open_folder` exist. Test what native .app selection actually permits before choosing the implementation. Keep Windows/Linux executable selection appropriate to their platform.
- If macOS bundle version metadata is used to avoid a fragile version subprocess, still validate the executable/browser capability during controlled render preparation. A version label is not proof that CDP can launch. Preserve isolated profiles, safe cancellation, and owned-process cleanup; do not attach to the user's normal browser session.
- Keep failure UI short and truthful: e.g. **Couldn't start Google Chrome.** with **Retry**, **Choose browser…**, and **Use HTML** as appropriate. Only call it missing when no installation was found. Raw paths, probe errors and attempted candidates belong behind **Details**, with copy access. No two-line path dump under the main command bar.
- Browser unavailability blocks starting a Browser crawl but never blocks inspecting/exporting retained results. Switching to HTML is an explicit one-click recovery. Checking must stay off the UI thread and cannot freeze or discard results.
- Preserve HTML as the default. Keep HTML/Browser terminology consistent for this pass; a broader mode rename is unnecessary.

## 2. Say the indexability verdict once

Observed code: `inspector_lead` emits `primary_issue`; Internal then calls `assessment_detail`, which emits the same assessment label again.

- One status line, one semantic color, one clear hierarchy. No duplicate white heading plus green status; no extra separator introduced to justify either line.
- Preserve the precise assessment: **No indexing block detected** is an observation, not proof of Google indexing or a guarantee that the page is indexable. Keep known restrictions, incomplete evidence and canonical uncertainty truthful.
- Supporting content must add facts: HTTP state, applicable restriction, canonical evidence, capture limitations. Keep technical detail collapsed until needed. Inspect warning/error states too, not only the clean example.
- Remove similarly repeated lead/assessment/reason text across the inspectors when the meaning is identical; keep genuinely different finding evidence and navigation.

## 3. Explain the sitemap option where it is used

Current behavior, confirmed in crawl.rs: on discovers sitemap files from robots.txt, with conventional-path fallbacks, and adds their same-host URLs to the page queue alongside links. These URLs share the same total page budget. Listed URLs enter at depth 0; robots.txt still applies. Off uses page-link discovery, not an unbounded/robots-free crawl. It does not create, upload or submit a sitemap.

- Replace the ambiguous checkbox label with **Use sitemap URLs**.
- Give it one concise visible explanation in crawl options, not only a hover tooltip: **Also crawl URLs listed in the site's sitemaps. Uses the same page limit.** Off can read **Follow page links only.**
- If that label cannot fit the command bar at 880px, put this setting in a compact crawl-options disclosure instead of squeezing the site URL or adding another permanent explanatory strip. Keep the current value discoverable. Reuse existing controls; no settings framework.
- Explain depth-0 entry points and robots behavior in secondary help, not a paragraph in the main toolbar. A 40-page limit is 40 pages total, not 40 plus the sitemap.
- Keep the default on and the option locked for an active crawl. Turning it off for the next crawl must not relabel retained coverage or erase sitemap evidence from the last run.
- The Sitemaps results tab describes the saved run. Distinguish **Sitemap discovery was off for this crawl** from **No sitemap found** and failed/partial discovery.
- Retained results must visibly identify their actual capture mode when next-run controls differ. The user's screenshots show an old HTML crawl beside failed next-run Browser setup; avoid implying those pages were browser-rendered. Use the saved run context in a compact results summary, not extra repeated warning text.

## 4. Add a lightweight default Overview

Recommendation accepted as the implementation direction for this pass: Overview is the default entry point, while Internal and the existing specialized tables remain one click away. It answers “what did we check, what should I review, and what didn't complete?”

Layout:

- Compact site/run line: host, captured URL count, actual capture mode and live/terminal outcome. Keep stopped, limited, failed and partial states visible.
- Plain list/table of nonzero finding groups using existing audit facts: response/fetch failures, indexing restrictions or canonical review, titles, descriptions, headings, image-alt and broken-link findings. Use appropriate Review wording for deliberate noindex/canonical states or heuristic length advisories; do not call every observation an error needing a fix.
- Each group shows an explicit affected-URL count and navigates to the correct existing view/filter. No decorative cards or invented severity score. Category counts overlap; never sum them and call the sum unique affected pages.
- Separate incomplete/unchecked coverage from confirmed findings. Only show counts the retained data actually supports, such as incomplete captures or sitemap URLs not checked. Do not invent a count for an unseen crawl frontier or treat successful non-HTML pages as failed checks.
- With no run, show a short invitation to enter a URL and start. With no findings, say **No findings in completed checks.** A stopped/limited/incomplete run must still explain the missing coverage; it is not an all-clear state.

Behavior:

- Select Overview at initial launch and once when a valid new crawl begins. Stream updates into it. Do not force users back when they switch to a table during the crawl or when completion arrives. Invalid setup preserves the current run/view/results.
- Summary counts and run settings come from the retained run snapshot and cached/incremental audit facts, not the draft controls or repeated full-dataset work on every frame.
- Navigation actions set the relevant view/filter deliberately and keep existing search/sort/selection behavior coherent. Do not leave a hidden old query making a clicked finding group appear empty.
- Overview is not a URL table. Do not carry over meaningless table search/sort/copy-visible controls there. Keep access to the URL table and its CSV export obvious. Cmd/Ctrl+F should have a clear useful destination, such as Internal search; test it.
- Tab counts currently mix meanings: Internal is total crawled URLs, Titles is findings, Sitemaps is listed URLs; this is only explained on hover. Prefer removing the numeric suffixes from the top navigation for this pass and presenting explicit counts in Overview and the existing result/filter summaries. This also makes space for Overview. Keep all navigation reachable at 880px without tiny labels or clipped last tabs.

## Acceptance and hand-back

- Native Mac walkthrough: installed/running Chrome, cold startup, explicit .app choice, cancellation, stale path, retry, HTML fallback, restart with remembered choice. Windows/Linux selection remains supported. Report native checks separately from scripted dialog/unit tests.
- Rune interaction regressions for one verdict, error disclosure/recovery, sitemap on/off meaning and saved-run independence, Overview group navigation, streaming/completion without forced navigation, invalid input retention, partial/no-findings honesty, and keyboard paths.
- Inspect 880x560 and 1180x740 in light/dark: empty Overview; findings; clean complete; partial/limited/failed; Browser checking/unavailable/recovered; sitemap option open; Internal clean/error/incomplete detail. Use actual user flows as well as screenshots. No raw path overflow or duplicated state text.
- Keep 10k/50k interaction costs bounded using existing caches. Avoid a full audit/normalization pass per Overview frame. Do not add a CI performance gate or reset baselines.
- Run focused regressions, fmt, all-target Clippy, relevant browser tests if discovery/process logic changes, and a Windows compilation check. No repetitive full-suite runs without a new change or failure.
- Update PRODUCT.md/DESIGN.md narrowly for the new flow and add user-facing Unreleased notes. Preserve public CSV column compatibility unless a separately justified bug requires a change.
- Return changed paths, screenshots, actual checks, unresolved platform limitations, and any material tradeoff. Root reviews the implementation before a release.
