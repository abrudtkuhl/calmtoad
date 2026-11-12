# Sitemap discovery and auditing

Status: ready to build. User requested planning and implementation handoff to OpenCode.

## Product decision

Keep local saved/reopened crawl projects out of this feature; retained history is planned for Cloud. This feature improves a single local crawl: discover URLs from a site's sitemaps and inspect whether those listed URLs are healthy.

Primary workflow: enter a site, leave **Use sitemaps** enabled, crawl, then inspect a **Sitemaps** tab. The user can see which files were read, which URLs were listed, which were checked, and why a listed URL needs attention. The crawl remains useful when no sitemap exists or one file fails.

## First release scope

1. Discover sitemap declarations in the seed origin's robots.txt. These declarations are independent of user-agent groups. Reuse the robots response/cache rather than downloading it again. Deduplicate declarations.
2. If there are no usable in-scope declarations, try /sitemap.xml; if absent, try /sitemap_index.xml. A missing guessed path is absence, not an SEO error. A missing explicitly declared sitemap is a reportable file failure.
3. Read XML urlset and sitemapindex documents, including gzip-compressed files. Follow child sitemap references with deduplication and cycle protection. Only the page loc directly inside a url element is a page URL: image/video extension loc elements and hreflang links must not become crawl seeds.
4. Add eligible sitemap URLs to the existing crawl queue. A URL listed in several files or found through links is fetched once, while retaining every discovery source.
5. Audit sitemap-listed URLs using actual crawl evidence. Show file errors and unvisited/blocked entries without manufacturing page fetches or content issues.
6. Add Sitemaps results and CSV export using the existing design, filter, sorting, selection, and export conventions.

No sitemap generation/submission, saved projects, Cloud upload, scheduling, lastmod accuracy grading, RSS/Atom/text sitemap support, cross-site sitemap hosting, or orphan-page classification in this pass. A crawled page missing from a sitemap is not automatically an issue. Lack of observed inlinks does not prove a page is an orphan.

## Discovery and crawl policy

- Add one **Use sitemaps** toggle near the crawl controls, enabled by default in the desktop app. Off means the existing links-only crawl, without sitemap requests. No custom sitemap URL field in v1.
- Scope sitemap documents and listed page URLs to the seed origin (scheme, host, effective port). Show out-of-scope references as skipped, without requesting them. Cross-host sitemap declarations can be legitimate; label them unsupported/out of scope rather than invalid XML. Do not broaden the existing HTML crawl's scope as a side effect.
- Handle sitemap redirects explicitly (at most five hops), validate scope and robots rules before every hop, detect cycles, and report failures. The current shared HTTP agent disables automatic redirects; don't silently enable them globally.
- Respect robots restrictions, crawl delay, server backoff and cancellation. Listing a URL in a sitemap does not grant permission to fetch it. A robots-blocked entry is shown as blocked without fetching the page or consuming the page budget.
- Fetch pages through the existing page pipeline so retries, content parsing and audits agree with ordinary link crawling. Sitemap documents are metadata, not Page records; they do not appear as HTML pages or count toward the page limit.
- One combined page budget covers the seed, link-discovered pages and sitemap-discovered pages. Never fetch a separate unlimited sitemap batch. Preserve the meaning of the current crawl page cap.
- Sitemap URLs are additional entry points, independent of link depth. Their outgoing link traversal starts at depth 0 internally and respects max_depth from there. Keep provenance so no UI claims that a sitemap-only URL is zero clicks from the homepage. Do not add synthetic inlinks or infer graph depth from sitemap membership.
- Stream page results and discovery status from background work; never parse or fetch sitemaps on the UI thread. Avoid delaying the seed's first result until every sitemap has finished. Ensure completion accounts for both discovery and page work, without a queue-drained race that loses late sitemap URLs.
- Stop cancels both kinds of work. Surface a partial report immediately after workers wind down within existing bounded network timeouts. A new crawl clears the prior report and rejects late events from the previous run.

## Bounded parsing and fetching

Use a proper XML parser, not HTML parsing or regex. Match structural element names/namespaces; handle UTF-8, BOM, XML entities, whitespace, namespace prefixes and optional metadata. Support standard namespace documents and document the policy for namespace-less documents. Reject malformed XML/unsupported roots with a visible file diagnostic. Never fetch XML external entities/DTDs.

Keep sitemap fetching separate from the current HTML wants_body filter: crawl.rs currently discards application/xml bodies and Fetched stores String, which cannot represent raw .gz bytes. Add a bounded byte-oriented sitemap fetch path with a test seam, reuse the HTTP connection pool, and preserve existing HTML fetch behavior. Existing flate2 can decode gzip. Detect compressed bytes/transport decoding carefully so an HTTP-decoded gzip body is not decompressed twice.

Initial app guardrails (constants, with clear partial-result reasons):

- 50 MiB (52,428,800 bytes) maximum uncompressed XML per document; also bound compressed/download bytes and check cancellation while reading/inflating.
- 50,000 entries per document; report oversized documents instead of treating truncated input as valid XML.
- 100 unique sitemap documents per crawl, five levels of index traversal, and 100,000 total URL entries processed per crawl. Count invalid/duplicate entries toward the processing budget so they cannot evade it.
- Bound redirect/retry totals, use existing network timeout/backoff behavior, avoid unbounded DOM and quadratic scans.

When any app guardrail is reached, keep the validated results already obtained and label discovery partial. These metadata guardrails are distinct from the user-selected page budget. Do not silently claim complete sitemap coverage.

## Results and audit semantics

Store the normalized page URL, its source sitemap(s), and discovery/visit state separately from Page. Reuse URL normalization (fragments removed, query strings retained). Sitemap loc must be an absolute HTTP(S) URL; invalid/relative entries get a diagnostic and are not fetched. Distinguish URL validation from scope skipping. Preserve evidence for duplicate entries rather than refetching them.

Report these observed URL findings:

- HTTP error or fetch failure.
- Redirect, with the observed target.
- noindex on a fetched response/page, from existing header/meta checks.
- Canonical points to another URL, only when the fetched HTML declares it. Missing canonical alone is not a sitemap error.
- Blocked by robots.txt, invalid URL, or outside supported scope.

Do not mark non-HTML responses as missing titles or missing canonicals. Do not assert actual search-engine indexing status from these checks. Optional lastmod/changefreq/priority fields can be parsed/ignored safely; absence is not an error.

Visit states: pending while running; checked when a Page exists; blocked/skipped with reason; not checked after page cap/Stop. Not checked is not broken. A URL can have several observed findings, but summary/count semantics must be explicit and consistent.

## Native UI

Follow DESIGN.md and the current tabs/table/details drawer. Add Sitemaps as a peer tab. Use compact **URLs** and **Files** views within it if needed to keep file failures visible without mixing metadata documents with page results.

- URLs view: URL, status/visit state, issue(s), source sitemap; selected-row details can list additional source files and redirect/canonical targets.
- Files view: sitemap URL, fetch/parse state, URL count, issue/skip reason. Files with no URL entries must still be inspectable.
- Summary distinguishes listed, checked, blocked, and not checked; show discovery partial reasons. Example: “950 listed · 40 checked · 910 not checked” when the page cap stops the audit. Counts must reconcile with actual states, including invalid/out-of-scope entries.
- Issues only and sorting work for the active view. Zero sitemaps is a calm empty state. While discovery runs, do not show “No sitemap found” prematurely.
- Export CSV exports the active Sitemaps view and current filter/sort, including provenance and unchecked reasons. Keep the existing CSV escaping/formula protection. Existing tab exports keep their behavior.
- Verify at the supported minimum window size and both light/dark themes. UI copy states what was observed rather than promising comprehensive indexing validation.

## Implementation structure

Introduce a focused sitemap module (for example src/sitemap.rs) that owns XML parsing, bounded discovery, membership/provenance and file diagnostics behind a small interface. Keep HTTP transport injectable for deterministic tests. Integrate scheduling/report events in crawl.rs; keep UI rendering/state in main.rs. Do not turn sitemap XML into Page or scatter parsing into UI callbacks.

Prefer explicit discovery/report events over polling a global mutable store. Preserve links-only library callers and deterministic benchmark behavior using explicit options/wrappers as appropriate. Update examples/fixtures only where the interface actually changes. Do not build an extension/plugin framework for this feature.

## Work order

1. Inspect current code/instructions and dirty changes; implement and test parsing + bounded discovery.
2. Integrate robots declarations, the combined queue/budget, cancellation and provenance, with end-to-end fake-fetch fixtures.
3. Add Sitemaps UI/results/export and offline sample scenes that show mixed outcomes.
4. Update README, PRODUCT and an Added entry under CHANGELOG Unreleased; run checks and render the UI for review.

## Acceptance checks

- A page present only in a sitemap is fetched and audited; a page in both a sitemap and links is fetched once.
- Multiple robots Sitemap declarations, fallback paths, prefixed namespaces, escaped query strings, index cycles, gzip, gzip transport handling, malformed XML, invalid loc, foreign-namespace loc and duplicate memberships behave predictably.
- Out-of-scope URLs and robots-blocked URLs are never requested. File/page redirects cannot bypass scope/robots. Missing guessed roots are not reported as broken pages.
- Existing links-only tests still pass. Page limits hold with concurrent sitemap/link discovery; zero-page and low-depth cases are explicit. Discovery caps/Stop leave accurate incomplete states; every failure path terminates without deadlock.
- Audit fixtures include a healthy URL, 404, redirect, noindex, different canonical, non-HTML, blocked URL, and untouched URL due to page cap. No incorrect orphan, missing-sitemap or “all URLs healthy” claim is made on a partial crawl.
- Sitemaps file errors remain visible even when no page URLs were read. CSV matches visible rows/columns/filter/order and protects formulas.
- Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, cargo test, plus relevant local benchmark checks for crawler changes. Performance checks stay local; do not reinstate CI performance gating or reset baselines just to get green results. Baseline already showed audit/counts regressions before this work, so distinguish existing results from newly introduced regressions.
- Render representative new UI scenes in light/dark and minimum/normal sizes; report screenshot paths and verification limits. Actual GitHub platform checks happen through the normal CI when changes are ready for a PR/release.

## Coordination and delivery

The shared tree currently has unrelated main.rs changes hiding the initial Ready footer text, and a matching CHANGELOG Improved bullet. Preserve them. The comparison-page website work belongs to calmtoad-eng; do not edit the website during this task. Coordinate before replacing shared hunks. Keep this plan in docs/plans/sitemap-discovery.md.

User authorized implementation. Build the feature through a local review-ready result, not just a skeleton or plan. Do not commit, push, tag, publish or deploy as part of this handoff; report implemented behavior, tests and screenshots to Codex for review. If the provider or permissions block you, report the exact blocker instead of silently remaining queued.

## Sources

- Sitemap protocol: https://www.sitemaps.org/protocol.html (robots declarations, XML/index structure, gzip and per-file size/count limits).
- Current app code: src/crawl.rs (Robots, Fetched, crawl_engine, from_response, normalization), src/main.rs (tabs/results/CSV export), src/export.rs, PRODUCT.md and DESIGN.md.
