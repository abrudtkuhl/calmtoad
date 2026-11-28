# Changelog

User-facing changes are recorded here as work ships. Versions without a release
date are being prepared for release.

## [Unreleased]

## [0.13.0] - 2026-11-27

### Added

- **Install Calm Toad on macOS or Linux with one command.** The installer detects the computer, verifies the signed release manifest and download checksum, installs Calm Toad without an administrator password, and opens it. It can pin a version, choose a location, skip launch, and safely recover interrupted upgrades.
- **Download a macOS disk image.** Apple silicon and Intel releases include a `.dmg` with Calm Toad and an Applications shortcut, alongside the updater `.zip`.
- **Package macOS releases consistently.** CI creates and checks the app bundle, updater archive, and disk image. When Apple Developer ID and notarization credentials are configured, releases are signed, notarized, and stapled.

### Fixed

- Seal the macOS app bundle during signing so Gatekeeper no longer reports the unsealed app as damaged.

## [0.12.0]

### Improved

- **Read the crawl as a dashboard.** Overview leads with the site title and one run-facts line, then ranks up to three findings by deterministic impact; a homepage 5xx ranks above title and meta findings. Each finding names the actual subtype, explains why it matters, shows a truthful affected/eligible count, and opens the exact view and evidence row, applying the subtype query where the table supports one. Coverage lists only what was not reached or not run, with **Adjust page limit** setting the next limit without starting a crawl. **All checks** keeps all seven core families visible with Findings, Clean, Partial or Not run and a nested optional-checks group, while **Open URL table** and one **Export** menu stay together. Counts remain per check and are never summed into a unique-page claim. Starting a crawl collapses an idle Custom Extraction editor and keeps its drafts.

## [0.11.0] - 2026-11-24

### Added

- Check a hand-picked list of URLs by pasting them, loading a TXT file, or importing a CSV. Preview eligible, duplicate, invalid, and out-of-scope rows before starting.
- Review one coverage row per submitted URL, open checked pages in the existing page details, and export the visible results to CSV.

### Improved

- URL-list crawls follow the same robots rules, redirects, throttling, cancellation, and Browser rendering as a regular crawl. They stay within the chosen site and do not add discovered links or sitemaps to the list.
- Large list previews prepare coverage in the background, keeping the app responsive while preserving clear page-limit and redirect results for every submitted URL.

### Fixed

- Failed file imports keep the current draft and last valid preview. Redirects that converge on the same destination retain each source URL's own status.

## [0.10.0] - 2026-11-22

### Added

- **Check accessibility on rendered pages.** Turn on **Check accessibility** in the crawl options with **Browser** mode to run a local, opt-in audit of the captured main document and supported open shadow roots. The app bundles Deque axe-core 4.14.1 and its notices, uses the explicit WCAG A/AA tag profile, and runs the engine in an isolated execution world so a page-owned `window.axe` is never used. HTML mode says **Requires Browser mode** and never audits static HTML as if it were rendered. Frame contents, closed shadow roots, responsive states and interactive journeys stay outside the declared scope; the app never calls a zero-violation result WCAG conformance or an accessibility score.
- **Review accessibility evidence.** **Internal → Accessibility** adds compact **Pages** and **Rules** views. Pages lists every captured URL with execution state, retained violation and needs-review counts, maximum known impact and coverage. Rules groups by stable rule ID and outcome, keeps unique affected URLs separate from repeated occurrences, and opens page and occurrence evidence in the inspector. Search covers retained URL, rule, title, description, tags, impact, selector, HTML, failure and coverage fields. Selected and engine-returned rule inventories are kept separately, and rule-field, tag, selector, HTML and failure truncation carries explicit flags. Overview adds separate violation, needs-review and incomplete-coverage rows.
- **Export accessibility coverage and occurrences.** Separate CSV actions save the visible Pages coverage or Rules occurrence rows in the current order, with execution state, reason, engine/profile/viewport, outcome counts, rule details, nested target arrays, bounded HTML and failure summaries, truncation, scope and frozen run context.

### Improved

- Accessibility evidence has its own 64 MiB run ledger, separate from structured-data evidence. The browser preflight declines documents over 20,000 elements; each page retains at most 128 rule/result groups and 200 occurrences, a run retains at most 10,000 occurrences, and a page's projected browser-to-Rust payload is capped at 512 KiB. Every drop or limit stays visible instead of becoming a clean result.
- The audit gets at most ten additional seconds after a successful render, under the existing 25-second absolute job ceiling. Cancellation and timeout dispose the owned page/context; an audit failure never erases successful SEO capture. The off and HTML paths launch no audit work.
- The bundled engine is embedded in the binary. No CDN, npm, Node.js, runtime download, second navigation or audit-originated iframe/CSS preload request is added. The pinned bundle, license and third-party notices ship with hashes and reproduction steps in `assets/axe/4.14.1/`.
- Accessibility view grouping, filtering and sorting run on a bounded worker with active-view and generation guards. Export preparation and writing use the same frozen-snapshot and shared-lease discipline as other exports, including cancellation while a paused worker still owns projected rows.

### Fixed

- Accessibility export reservations now stay charged through the preparation worker, native save dialog and background writer rather than being released when the visible job is cancelled. Bounded CSV projection refuses to return a silently partial file.
- An accessibility scan whose main-document URL or frame identity changes during the audit discards its findings instead of attributing them to the requested URL.

## [0.9.0] - 2026-11-22

### Added

- **Review JSON-LD on every captured page.** **Internal → Structured data** adds **Pages** and **Blocks** views for JSON-LD block discovery, JSON syntax diagnostics, duplicate-key review findings, and literal `@type`/`@id` values with JSON Pointer paths. HTML uses the server document; Browser uses only the rendered DOM.
- **Export structured-data evidence.** Save the visible Pages or Blocks rows as CSV with capture completeness, syntax outcome, root and inventory state, observed/inspected/retained/skipped counts, literal types and identifiers, duplicate-key diagnostics, block-relative line/column positions, limitations, and frozen run context.
- **See coverage instead of a false all-clear.** Reports keep capture availability, parser limits, literal-inventory coverage and evidence retention separate. Malformed or empty inline JSON and duplicate keys are confirmed findings only on complete captures; unavailable, partial, cancelled or resource-limited inspections stay visible in Overview and the Incomplete filter.

### Improved

- Update banners reflow their actions below the message when the window is narrow, keeping release actions readable and responsive.
- Structured-data inspection reuses the DOM already parsed for the crate's existing audits. It makes no extra requests, does not fetch `src` or remote contexts, executes no scripts, and retains bounded evidence rather than a second JSON tree. A shared 128 MiB run budget covers captured evidence plus the Pages/Blocks index caches, export snapshots and copies; reservation failures are reported as limited coverage instead of allocating past the cap.
- Large Structured data views index, search, filter and sort on a background worker in bounded steps. Refreshes reserve their snapshot before copying, and leaving the view cancels owned work. Search text is computed per page or block when needed instead of being cached per row, so a 100,000-block view stays inside the same ledger. Small result sets keep the immediate projection.
- Structured data still shows its previous rows while a refresh is running, labels the view as refreshing, and refuses an export until the visible snapshot is settled.

### Fixed

- A JSON Pointer path shortened at the 1,024-byte bound no longer splits a multibyte character. A long key followed by an emoji key keeps the page's other audits instead of panicking the crawl worker.
- Cancelling structured-data traversal is reported as **Cancelled** coverage, never as malformed JSON or a confirmed syntax finding. Parser limits remain distinct from syntax errors and cancellation.
- Non-string `@type` and `@id` objects and arrays go through the same duplicate-key, depth, value-count and cancellation checks as every other JSON value, so a duplicate inside `{"@type":{"x":1,"x":2}}` is still diagnosed and a deep container cannot bypass the traversal bound.
- A nested array directly under the document root, including `[[]]`, parses as JSON but reports an unsupported inventory shape instead of a complete literal inventory.
- Non-string `@id` advisories now name `@id` instead of describing the value as a non-string `@type` entry.
- Literal type/ID occurrence caps now apply across every block on a page, and value-object datatype evidence is fully rolled back (counts, bytes and budget) so later real nodes remain retainable.
- Nested arrays inside non-string `@type`/`@id` values are validated without inventorying their inner strings.
- Structured-data search checks every retained supported field instead of an 8 KiB concatenated prefix, so a marker in a later literal type, ID, path or diagnostic still matches. Raw JSON remains excluded.
- Export now reserves its row snapshot plus a per-row context/projection/encoding allowance and keeps that ledger lease through the save dialog and background write.
- A cancelled or inactive structured refresh cannot free bytes still owned by its worker snapshot or overwrite another tab's counts and selection.
- Cancelling a structured-data export keeps its ledger reservation until the preparation worker, save dialog or background writer actually drops the projected rows and encoded CSV; a paused-worker regression covers the cancellation gap.
- Exports whose retained `@type`/`@id` values contain control characters now reserve their JSON-escaped size, so a valid heavily escaped row is not rejected by an underestimate.
- Structured-data Pages CSV exports now reserve every per-block **INVENTORY STATE** label and separator, so a valid multirow export is no longer rejected by an underestimate.

## [0.8.0] - 2026-11-20

### Added

- **Find duplicate and similar page content.** Open **Internal → Duplicate content** to review pages with the same normalized text and similar-page pairs, beyond duplicate titles and descriptions.
- **Inspect the evidence behind each match.** Compare captured text excerpts, matching URLs, text overlap and capture provenance. HTML and Browser captures remain separate, and incomplete or resource-limited comparisons are explained.
- **Export content comparisons.** Save same-text group members, similar-page pairs or URLs that could not be compared as CSV, using the current search and sort order.

### Improved

- Content comparison and CSV preparation run in the background with cancellation and bounded work. Comparisons reuse the pages already captured locally; they make no additional requests and do not require a cloud account.

## [0.7.0] - 2026-11-19

### Added

- **See the exact links behind a finding.** The Links tab now switches between Pages and Link evidence. Evidence keeps repeated anchors, fragments, invalid schemes, robots/budget skips, source/rendered stages and checked destination facts. An opt-in **Check external links and documents** pass checks external anchors and same-host document downloads with bounded robots-aware requests; document contents are never crawled.
- **Export a complete content inventory.** Overview and Internal offer **Export inventory CSV…** for all captured URLs or the visible Internal set. One row per URL carries response, capture, content, indexability, link coverage, run context and custom-extraction columns so the CSV needs no joins.
- **Extract custom CSS fields.** Add up to 20 named Text or Attribute rules with First/All matching, validate and save them locally, preview one URL without crawling further, and see retained values in the Internal inspector and inventory. Browser rules use rendered HTML and report unavailable rather than substituting source HTML.

### Improved

- Link evidence and the inventory remain bounded during streaming and large crawls; existing page-level CSV schemas are unchanged.
- Inventory export can run during a crawl from a labeled immutable snapshot. Heavy page evidence is shared, preparation is chunked across UI frames, and projection, encoding, and writing run in the background.
- Completed extraction previews keep their captured mode and rule definitions; later draft edits mark the preview stale instead of relabeling it.

### Fixed

- The optional link evidence cap no longer shortens ordinary page discovery, and overlong hrefs are declined rather than fetched as truncated destinations.
- Optional link checks treat unreadable robots policy as unknown/blocked for the whole origin, revalidate redirects against internal page limits, and enforce cancellation plus a 30-second chain deadline, including slow robots reads and long crawl delays.
- Extraction reports value truncation and missing attributes honestly, keeps later All-mode matches, and rejects duplicate rule IDs before capture or export.
- Closing the window or installing an update now owns and cancels extraction preview and inventory work. Inventory preparation, save dialog, and file writing are tracked states; updates wait for all of them, and cancelled writes leave an existing destination untouched.
- Oversized robots policies are explicit unknown/blocked, per-origin Retry-After now delays later checks, and shared redirect destinations are fetched once while each source keeps its own chain.
- Cancelling the final optional check now stops the run, and Browser rendered/source evidence shares one 10,000-occurrence page cap without shortening discovery.

## [0.6.1] - 2026-11-18

### Fixed

- The Linux download starts again on Ubuntu 22.04 and newer, Debian 13, and other systems with glibc 2.35 or later. Calm Toad 0.6.0 for Linux needed glibc 2.43 and quit immediately on most current distributions.
- Identify crawl requests as `CalmToad/<version> (+https://calmtoad.com)`, in both HTML and Browser mode, so site owners see the real release and a working contact URL. Earlier versions sent `Calmtoad/0.1` with a placeholder address. Robots.txt groups for `Calmtoad` still apply.

## [0.6.0] - 2026-11-17

### Added

- Start in Overview to see finding groups and crawl coverage, then jump straight to the matching URL tables.
- Choose a browser application directly on macOS from Applications, including `Google Chrome.app`, and remember your choice between launches.

### Improved

- Explain **Use sitemap URLs** where you configure a crawl: sitemap-listed URLs share the page limit, and robots.txt still applies.
- Show the indexability verdict once, remove mixed-meaning tab counts, and identify the capture mode of retained results when next-run settings differ.
- Keep browser startup errors short, with Retry, Choose browser, and Use HTML recovery actions. Raw diagnostics stay under Details and can be copied.

### Fixed

- Treat Stop during browser preparation as a stopped run, preserving the browser selection instead of reporting an installation failure.
- Keep successful Browser captures of PDFs and other non-HTML responses out of incomplete-check counts, while preserving real capture failures and truncation.
- Match Overview coverage counts to the rows they open, and surface failed or skipped sitemap files even when they supplied no URLs.

## [0.5.0] - 2026-11-16

### Added

- **Crawl JavaScript sites.** Choose Browser to render pages in your installed Chrome or Chromium before checking titles, descriptions, headings, links, images, and indexing directives. HTML remains the default and needs no browser.
- **See what the browser captured.** Inspect source and rendered evidence, capture status, browser version, and script-changed robots or canonical declarations. Incomplete captures stay distinct from clean results.
- **Search and focus your results.** Switch between All, Findings, and Incomplete, with separate search, sort, and selection settings for each view. CSV export and Copy visible URLs follow the rows and order on screen.
- **Follow findings to their source.** Inspect duplicate peers, multiple headings, incoming links, outgoing targets, and redirect chains. Navigate to related results or copy a full relation list.
- **Review the run.** Open Run for the crawl's saved settings, coverage, and completion, stop, or limit reason. Use Cmd/Ctrl+F for search and Cmd/Ctrl+L for the site URL.

### Improved

- Make the desktop interface more compact, with less repeated text, simpler browser setup, and expandable evidence and capture details.
- Keep large result sets responsive with bounded event processing, cached relation data, and paged inspector lists. Every retained relation remains accessible.
- Run Browser crawls in an isolated temporary profile, with bounded page rendering and the same host, robots.txt, page, and depth limits as HTML crawls. Calm Toad does not download a browser or use your everyday profile.
- Include capture status in the Internal and Sitemaps URLs CSV exports. Unknown content measurements remain blank instead of appearing as zero or healthy.

### Fixed

- Keep completed-run coverage and stop reasons accurate when settings for the next crawl change or fail validation.
- Preserve confirmed findings on incomplete captures and show skipped sitemap files in Findings.
- Preserve result selection and sorting as pages arrive, and reset inspector paging when changing results or starting a new crawl.
- Clean up owned browser processes on cancellation, startup failure, and app close without signalling an already-collected process.

## [0.4.0] - 2026-11-12

### Added

- Replace the Internal tab's yes/no indexability answer with an evidence-backed assessment: Fetch failed, HTTP error, Redirect, Noindex, Review canonical, Canonical elsewhere, Not assessed, or No indexing block detected.
- Show every applicable robots meta and `X-Robots-Tag` declaration, including repeated and agent-scoped rules, in the page details drawer.
- Show every HTML canonical declaration with its resolved target and whether that target was fetched or left unchecked. Missing, self, repeated, malformed, and conflicting canonicals stay distinct.
- Report a body-read failure or a body truncated at the crawl size limit as an incomplete assessment instead of a clean result.
- Treat a successful non-HTML response as informational Not assessed context rather than an issue, so it stays out of Issues only and sitemap issue counts while its explanation remains visible.
- Keep applicable noindex and nofollow restrictions found beyond the retained evidence cap, and state when raw evidence was truncated.
- Add REASONS and EVIDENCE columns to the Internal CSV export, and use the same assessment for Issues only, sorting, and checked sitemap URLs.

## [0.3.0] - 2026-11-12

### Added

- Read a site's sitemaps during a crawl and audit the URLs they list. The new Sitemaps tab has URLs and Files views, says when a URL was blocked, skipped, or left by the page limit, and exports the active view to CSV.
- Discover sitemaps from robots.txt declarations, falling back to `/sitemap.xml` and then `/sitemap_index.xml`, and read XML urlset/index files including gzip with bounded document, entry, depth, and redirect limits.

### Improved

- Keep the footer clear of the initial ready message when the app opens.

## [0.2.0] - 2026-11-11

### Added

- Export the active results tab to CSV, keeping its current columns, sort order, and issue filter.
- Update Calm Toad from inside the app: it checks when it opens and every 24 hours, verifies each download against a signed manifest, and installs the new version after you confirm a restart.
- Existing installations need one manual download of this version to enable future in-app updates.

### Improved

- Read plain-language release notes on GitHub and the website changelog.

## [0.1.1] - 2026-11-11

### Improved

- Crawl larger sites by setting a page limit above 2,000.

## [0.1.0] - 2026-11-09

### Added

- Initial desktop SEO crawler for Windows, macOS, and Linux.
- Inspect page titles, meta descriptions, H1 headings, response codes, canonicals, images, and internal links.
- Crawl a single host with configurable page and depth limits, streamed results, and a Stop control.
- Read robots.txt and honor X-Robots-Tag: noindex directives.
