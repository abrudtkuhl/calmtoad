# Changelog

## [Unreleased]

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
- Oversized robots policies are explicit unknown/blocked, per-origin Retry-After now delays later checks, and shared redirect terminals are fetched once while each source keeps its own chain.
- Cancelling the final optional check now stops the run, and Browser rendered/source evidence shares one 10,000-occurrence page cap without shortening discovery.

User-facing changes are recorded here as work ships. Versions without a release
date are being prepared for release.

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
