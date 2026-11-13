# Changelog

User-facing changes are recorded here as work ships. Versions without a release
date are being prepared for release.

## [Unreleased]

## [0.4.0] - 2026-11-12

### Added

- Replace the Internal tab's yes/no indexability answer with an evidence-backed assessment: Fetch failed, HTTP error, Redirect, Noindex, Review canonical, Canonical elsewhere, Not assessed, or No block found.
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
