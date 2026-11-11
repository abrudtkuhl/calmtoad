# Changelog

User-facing changes are recorded here as work ships. Versions without a release
date are being prepared for release.

## [Unreleased]

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
