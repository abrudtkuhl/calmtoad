# Product

<!-- impeccable:product-schema 1 -->

## Platform

native

Desktop app for macOS, Windows, and Linux. The window follows the system light and dark theme.

## Users

People who need to inspect a website with a desktop SEO spider. They are at a desk with one site to check. The job is the crawl and the issues, not a walkthrough.

## Product Purpose

The app crawls one host and shows the checks that crawl first: internal URLs, page titles, meta descriptions, H1s, response codes (broken links and redirects), canonicals, image alt text, internal links, and the URLs a site's sitemaps list. Success is seeing those facts for the pages the crawl actually reached and a truthful account of every sitemap URL it did not check.

## Positioning

The app serves people who need a desktop crawl followed by on-page, response, and sitemap checks. Its shipped features cover on-page, response, canonical, image, internal-link, and sitemap discovery and auditing; it is not a full crawler.

## Operating Context

A native window. Paste a URL, leave **Use sitemaps** on or turn it off, press Crawl, then switch tabs. Issues only hides clean rows for the current tab. Select a result to open its page details drawer; copy its URL or open it in a browser from the drawer. The Sitemaps tab has URLs and Files views over the same crawl. The footer states the crawl result, including when the page cap stopped it. `cargo run` opens the window. On launch and every 24 hours the app checks a signed release manifest; an available update is offered in a strip and installed only when the person asks to restart.

## Capabilities and Constraints

Confirmed in the current app:

- Same host only. Other hosts are not followed.
- Defaults are 40 pages and 3 levels deep (the seed is level 0). The page limit has no fixed maximum; crawl size depends on the limit you enter and the computer's available resources. Depth can be set up to 10.
- Does not render JavaScript. Reads robots.txt (Allow, Disallow, Crawl-delay) and honors `X-Robots-Tag: noindex`.
- Fetches run in parallel and stream into the table as they land; Stop cancels the crawl. Transient 429/5xx/transport failures retry with backoff, and a 429/503 pauses the pool.
- Skips fragments and non-page schemes (mailto, javascript, tel, data) and common asset extensions. HTML is parsed; redirects are recorded, not treated as pages to audit.
- Titles are judged at 30–60 characters, meta descriptions at 70–160, and only on auditable pages (HTTP 200, HTML, no fetch error). A 404, a redirect, or a non-HTML response is not a missing-title issue.
- Issue wording in use: Missing, Too long, Too short, Duplicate, Multiple, and the combined forms "Duplicate, too short" and "Duplicate, too long". Empty titles are not duplicates of each other. Both copies of a duplicate count.
- Canonical, image, and link checks: a missing canonical or one that points elsewhere, images without alt text, broken same-host links, inbound and outbound link counts, and redirect chains.
- Sitemap discovery reads `Sitemap:` declarations in the seed origin's robots.txt, independent of user-agent groups and deduplicated. If none are usable and in scope, it tries `/sitemap.xml`, then `/sitemap_index.xml`. A missing guessed path is recorded as absence and is not an issue; a missing declared sitemap is a file error.
- Sitemap files are read as XML urlsets or indexes, including gzip, redirects, and nested indexes. Foreign namespaces and unclosed, malformed, or DTD-bearing documents are rejected with a truthful file error instead of guessed entries.
- Guardrails: 50 MiB uncompressed and 16 MiB downloaded per document, 50,000 entries per document, 100 documents, five index levels, 100,000 total entries, and five redirect hops. Hitting one is stated as a partial result rather than silently truncating.
- Sitemap URLs are depth-0 entry points but never count as homepage inlinks. They share the crawl's page limit; a URL left by that limit is reported as not checked, not as healthy. Listed URLs retain whether they were checked, blocked by robots.txt, out of scope, skipped, or not checked, along with response, canonical, noindex, and non-HTML findings.
- The window title is Calm Toad. The interface itself carries no logo or wordmark. The nine tabs are Internal, Titles, Meta, H1, Response, Canonical, Images, Links, and Sitemaps. Sitemaps has URLs and Files views, and CSV export follows the active view, sort, and Issues only filter.
- In-app updates: the app checks a minisign-signed `latest.json` from GitHub Releases on launch and every 24 hours, verifies the package's size and SHA-256, stages it beside the install, and swaps it in after a user-confirmed restart. The old copy is kept until the new app starts; a failed install restores it. The updater only handles official portable installs in writable locations and never interrupts a crawl.
- `cargo bench --bench crawl` times a synthetic 300-page crawl, the audit rebuild, issue counting, and parsing; it can emit JSON and gate on a calibration-normalized baseline. `.github/workflows/ci.yml` runs fmt, clippy, and tests; the performance gate runs locally.

Undecided, and not implied by the market:

- Whether later versions add the rest of a full crawler (JavaScript rendering, sitemap generation or submission, more issue types, adaptive concurrency).
- Pricing, license, and distribution beyond GitHub Releases and GitHub Packages.

## Brand Commitments

The product name is Calm Toad, and the header carries no logo or wordmark. The committed visual identity is the Crawl Ledger system: a near-black or off-white hairline ground, mono micro-labels and measurements paired with system sans for page content, and one filled CRAWL action. The full system is in `DESIGN.md`; new screens follow it rather than introducing new colors, radii, or card treatments.

## Evidence on Hand

The behavior above is the app in `src/main.rs`, `src/crawl.rs`, `src/sitemap.rs`, and `src/update/`. `shots/` holds headless renders of a nine-page example.com fixture, not a customer site. There are no testimonials, case studies, press mentions, or pricing pages. Do not invent them.

## Product Principles

- The person is here to read a crawl. The window should make the rows and the issues obvious.
- Focus on the site audit tasks the app supports. Do not treat unbuilt crawler features as already in the product.
- One host. A partial crawl says what it reached and when it stopped.
- A non-page is a response fact, not a content failure.
- Do not add a claim the crawl did not earn.
