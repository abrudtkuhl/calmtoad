---
name: Calm Toad
description: A near-black crawl ledger for one host, set in hairlines and mono measurements.
colors:
  ground: "#0a0a0a"
  ground-light: "#fafafa"
  ink: "#ededed"
  ink-light: "#111111"
  field: "#0d0d0d"
  field-light: "#ffffff"
  raised: "#121212"
  raised-light: "#f2f2f2"
  control-neutral: "#161616"
  control-neutral-light: "#ececec"
  hairline-weak: "#1f1f1f"
  hairline-weak-light: "#e4e4e4"
  hairline: "#2a2a2a"
  hairline-light: "#d4d4d4"
  hairline-strong: "#3a3a3a"
  hairline-strong-light: "#b8b8b8"
  text-faint: "#8a8a8a"
  text-faint-light: "#6b6b6b"
  signal-green: "#3fb950"
  signal-green-light: "#1a7f37"
  signal-amber: "#d29922"
  signal-amber-light: "#9a6700"
  signal-red: "#f85149"
  signal-red-light: "#cf222e"
typography:
  body:
    fontFamily: "SF Pro Text, -apple-system, BlinkMacSystemFont, sans-serif"
    fontSize: "12.5px"
    fontWeight: 400
    lineHeight: "18px"
  label:
    fontFamily: "SF Mono, ui-monospace, Menlo, monospace"
    fontSize: "10px"
    fontWeight: 500
    lineHeight: "14px"
    letterSpacing: "0.6px"
  data:
    fontFamily: "SF Mono, ui-monospace, Menlo, monospace"
    fontSize: "11.5px"
    fontWeight: 400
    lineHeight: "15.5px"
    letterSpacing: "0px"
  title:
    fontFamily: "SF Mono, ui-monospace, Menlo, monospace"
    fontSize: "13px"
    fontWeight: 500
    lineHeight: "17px"
    letterSpacing: "1.2px"
rounded:
  sm: "2px"
  md: "4px"
  dot: "999px"
spacing:
  xxs: "2px"
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  gutter: "20px"
  xl: "24px"
  xxl: "28px"
  topbar: "44px"
  command: "40px"
  table-row: "30px"
  table-header: "32px"
  footer: "30px"
components:
  command-bar:
    backgroundColor: "{colors.field}"
    textColor: "{colors.ink}"
    rounded: "{rounded.md}"
    height: "40px"
  crawl-primary:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
    rounded: "{rounded.md}"
    height: "40px"
  crawl-stop:
    backgroundColor: "{colors.field}"
    textColor: "{colors.signal-red}"
    rounded: "{rounded.md}"
    height: "40px"
  tab-active:
    backgroundColor: "{colors.control-neutral}"
    textColor: "{colors.ink}"
    rounded: "{rounded.sm}"
  table-row:
    backgroundColor: "{colors.ground}"
    textColor: "{colors.ink}"
---

# Design System: Calm Toad

## Overview

**Creative North Star: "The Crawl Ledger"**

The app should feel like a crawl written directly onto the window: a dark sheet ruled by hairlines, with the measurements set in mono and the site's own words set in sans. There are no cards, no panels, and no decoration competing with the rows. The page is the instrument. Every element sits on one grid, separated by a single-pixel rule, so the eye moves from URL to status to issue without re-learning the surface on each tab.

The world is near-black in dark mode and off-white in light mode, both derived from the same seed. That keeps the window native to macOS while preserving one identity: ink on ground, with the same hairline rhythm either way. Type does the explaining. Mono marks anything the tool measures or names — labels, tab names, counts, status codes, the footer state. Sans is reserved for the site's content — URLs, titles, meta descriptions, H1s, canonicals — so the crawled page never sounds like the interface.

The only filled area on a resting screen is the CRAWL button. Color otherwise appears as a functional signal: a green dot when the crawl is healthy, amber for redirects and soft warnings, red for failures and broken pages. The restraint is the point. A user should trust the screen immediately and spend their attention on the crawl, not the chrome.

**Key Characteristics:**
- Hairline-separated regions instead of cards or shadows.
- One full-bleed table aligned to a single 20px gutter.
- Mono for instrument data; sans for page content.
- One white primary action; every other control is a flat ghost with a 1px ring.
- Status color only where it carries meaning; healthy rows stay quiet.
- No logo or wordmark in the header; the tabs are the first thing at the left gutter.

## Colors

A two-ground system: carbon black or warm off-white, with a narrow neutral ladder and three functional signals. Nothing is tinted for decoration.

### Primary
- **Carbon Ground** (`#0a0a0a`): The dark-mode page. Also the fill behind the detail strip and the area outside the table.
- **Paper Ground** (`#fafafa`): The light-mode page. The same system inverted, not a second theme.
- **Bone Ink** (`#ededed`): Dark-mode text, the CRAWL button fill, and the brand square. The brightest thing on a dark screen is the action, not a heading.
- **Carbon Ink** (`#111111`): Light-mode text and the light-mode CRAWL fill.

### Secondary
- **Signal Green** (`#3fb950` / `#1a7f37`): A completed crawl, a healthy status dot, a finished footer. Never used for non-state decoration.
- **Signal Amber** (`#d29922` / `#9a6700`): Redirects, soft audit issues, and the crawling state.
- **Signal Red** (`#f85149` / `#cf222e`): 4xx/5xx responses, fetch errors, and hard audit failures.

### Neutral
- **Field** (`#0d0d0d` / `#ffffff`): The command bar and text inputs. One step above the page, never a card.
- **Raised** (`#121212` / `#f2f2f2`): The selected table row wash and the active tab ground.
- **Control Neutral** (`#161616` / `#ececec`): The active tab's button fill.
- **Hairline Weak** (`#1f1f1f` / `#e4e4e4`): Row rules, the top-bar rule, the footer rule, and vertical column dividers.
- **Hairline** (`#2a2a2a` / `#d4d4d4`): The command-bar outline and the neutral button ring.
- **Hairline Strong** (`#3a3a3a` / `#b8b8b8`): Focus rings and the command bar's focus-within outline.
- **Faint Ink** (`#8a8a8a` / `#6b6b6b`): Placeholders and em dashes. Still readable, never a whisper.
- **Muted Ink** (derived, ≥4.5:1): Secondary copy, inactive tabs, table headers, and footer text. Derived by the kit rather than fixed, so it stays legible on both grounds.

### Named Rules
**The One White Action Rule.** Only the primary crawl action is filled. If two elements are filled at rest, one of them is wrong.

**The Signal Only Rule.** Color is a factual status, never emphasis. A 200 stays muted; only a redirect, a failure, or a soft issue earns a dot or a colored word.

## Typography

**Display Font:** none — this is an instrument, not an editorial surface
**Body Font:** SF Pro Text (system sans fallback)
**Label/Mono Font:** SF Mono (system monospace fallback)

**Character:** Two voices, strictly divided. Mono reports what the tool measured or named; sans shows what the site actually says. The split is the hierarchy, so the interface never needs display type or oversized headings.

### Hierarchy
- **Body** (400, 12.5px/18): URLs, page titles, meta descriptions, H1s, canonicals, and empty-state copy. This is the content the user came to read.
- **Data** (400, 11.5px/15.5, tabular numerals): Status codes, counts, word totals, lengths, and the detail-strip URL.
- **Label** (500, 10px/14, 0.6px tracking, uppercase): Tab names, column headers, section titles, field labels, and the footer's right-hand note.
- **Title** (500, 13px/17, 1.2px tracking, uppercase): Empty-state titles only. The screen has no marketing headline.
- **Brand** (500, 12px, 3px tracking, uppercase): The wordmark only.

### Named Rules
**The Two-Voice Rule.** If it is a measurement or an instrument label, it is mono. If it is a word from the crawled page, it is sans. Do not set a URL in mono for style, and do not set a count in sans for warmth.

**The Quiet Scale Rule.** The type scale stays inside 10–13px. Hierarchy comes from case, tracking, color, and spacing, not from size jumps.

## Layout

The window is one column, top to bottom, with no outer container and no page margin beyond a 20px gutter.

1. **Top bar** — 44px: the existing small toad mark and ten tabs, after the platform titlebar inset. Tabs carry bare uppercase labels with no numeric suffix; counts belong to Overview and each view's summary. Crawl and update status stay in the footer or update strip rather than competing with the tabs. A hairline closes the bar.
2. **Progress line** — 2px, only while crawling: a hairline track with an ink fill for pages found against the cap.
3. **Command bar** — 12px above and 8px below a 40px strip: URL, Pages, Depth, HTML/Browser, **Use sitemap URLs** and CRAWL/STOP. The disclosure also holds **Check external links and documents** and **Custom extraction…** so optional work does not become a second toolbar. Numeric fields are compact so the URL stays readable at 880px. A disclosure arrow beside the sitemap checkbox opens a compact explanation; it is not a permanent strip. In Browser mode a quiet line names the detected browser and offers **Choose browser…**. Paths and diagnostics stay behind **Details**; an unavailable browser keeps Crawl disabled but never blocks retained results. Settings remain disabled during a crawl.
4. **Results toolbar** — 40px, on table views: one visible-versus-total count with findings and incomplete checks, a compact search field, All/Findings/Incomplete filters, **Run** details, Export, Copy visible URLs and the details toggle. Overview is not a table and has no search, sort or copy toolbar. Do not repeat the active tab as a second heading. Sitemaps adds compact URLs/Files controls; the full sitemap coverage breakdown and filter basis are available on hover, and Partial stays visible. Search and filter state is stored per tab and per Sitemaps view.
5. **Results** — grows to fill the window: a full-bleed table and optional 320px details drawer. Both page and sitemap details have a bounded scroll region.
6. **Status footer** — 30px: the crawl outcome and elapsed time, update status and version. Preserve the actual stopped/limit/error reason; truncate long text with the complete message available on hover. Do not replace coverage information with a generic completion message.

Tables never sit inside a padded card. Header and cell text begin on the same 20px gutter as the section header, so every region shares one left edge. Columns use flex for content and fixed widths for counts and statuses; numeric columns are right-aligned. Row height is 30px, header height 32px. At 880×560 the layout holds: results and details scroll independently rather than reflowing into cards.

### Named Rules
**The Nameless Header Rule.** The header begins with the first tab after the traffic-light inset. No logo, wordmark, or product name sits in the window, so the name can change without touching the layout.

**The Full-Bleed Rule.** Tables touch the window edge. The 20px gutter aligns the first and last cell; it does not become container padding.

**The One Column Rule.** No sidebars, no split panes, no floating panels. The crawl is one vertical reading order.

## Elevation & Depth

There are no shadows and no raised surfaces. Depth is expressed with three tools only: a one-step tonal lift for fields, selected rows, and the active tab; 1px hairlines that separate regions; and spacing that groups. The kit's neutral button "shadow" is a 1px spread ring used as an outline, not as elevation. The primary button's elevation is cleared, so it is perfectly flat.

**The Flat-By-Default Rule.** If a surface appears to float, remove the shadow. Reach for a hairline or a tonal step first.

## Shapes

Corners are nearly square. Fields, the command bar, and the primary action use a 4px radius; tabs use 2px. Status dots are perfect circles at 5px. The only silhouettes that recur are the rectangle and the hairline. There are no pills, no capsules, and no decorative geometry.

There is no logo, wordmark, or monogram. The first element in the header is the Overview tab, aligned to the gutter.

**The Near-Square Rule.** Radii stay at 4px or below. If a shape needs to feel softer, it is probably too large for this system.

## Components

### Buttons
- **Shape:** 4px radius for the command action; 2px for tabs. No pill buttons.
- **Primary — CRAWL:** bone ink fill on carbon ground (inverted in light), ground-colored mono label at 10.5px with 1.2px tracking, 40px tall. It is the only filled control on a resting screen.
- **Crawling — STOP:** the same geometry, but a field ground with signal red text and a hairline ring. The action changes role, not size.
- **Active tab:** a neutral fill one step above the page, ink label.
- **Idle tab:** transparent ground, muted label, no ring until hover.
- **Hover / Focus:** a 1px ring in hairline or hairline strong. No glow, no lift, no scale.

### Rendering Picker
A two-segment HTML/Browser control inside the command strip, fitted to its labels rather than filling the bar. The selected segment is a raised hairline surface in the field tone; the other is transparent muted ink. It sits between Depth and Sitemaps and carries the accessible name "Page rendering". It is visible in both modes, so HTML stays the obvious default and Browser is one click away. While a crawl runs it is disabled, because the run's capture mode was fixed when Crawl was pressed.

### Browser Setup Strip
Only in Browser mode, directly under the command bar: a small neutral dot, a readable short browser name and **Retry**, **Use HTML**, **Choose browser…**, **Details** as appropriate. No LOCAL BROWSER label or permanently displayed filesystem path. The chosen application path, version and raw diagnostics stay behind **Details** with copy access. Checking says “Finding a browser…”. Unavailable gives a short truthful summary, including when the controlled launch failed after discovery; the disabled Crawl action and one-click HTML recovery remain available, while retained results stay inspectable and exportable. There is no install wizard or repeated disclaimer.
### Chips
Not used. Status is a dot plus a mono code, not a badge.

### Cards / Containers
Not used. The command bar is the only enclosed rectangle, and it is a control group, not a card. The detail strip is separated by a top hairline and no background fill.

### Inputs / Fields
- **Style:** transparent within the 40px command strip, 12.5px sans for the URL and 11.5px mono for the numeric limits. The group carries the 1px hairline and 4px radius.
- **Focus:** a 1px ring on the whole command bar (focus-within), not a glow on the individual field.
- **Disabled:** the text dims to muted while preparing, crawling or stopping; the strip keeps its geometry.
- **Validation:** URL, Pages and Depth are validated together by the shared start action, including keyboard submit. Invalid values mark the field, show one concise line beneath the strip and keep the previous results; the first invalid field takes focus. Intermediate text is allowed while editing.
- **Placeholder:** faint ink at reading contrast, never lighter than necessary.

### Navigation
Ten tabs in one row: Overview, Internal, Titles, Meta, H1, Response, Canonical, Images, Links, and Sitemaps. Each is a ghost button containing only an uppercase mono label; the old numeric suffixes mixed crawled URLs, findings and listed URLs under one unexplained style. The active tab alone gets a 2px-radius neutral fill. Tabs never wrap; at narrow widths the row compresses rather than becoming a menu. Tab labels are authored in normal case and uppercased visually.

### Link evidence

Links keeps its existing Pages table and adds a compact **Pages / Link evidence** switch in the results toolbar. Link evidence is a flat occurrence table: source, destination, anchor, HTTP and state, with the same All/Findings/Incomplete filter, search, sort, bounded rows, CSV export and copy behavior as other tables. Its inspector shows stage, raw href, fragment, truncation, check reason, redirect chain and source/destination actions; long values stay copyable. The counts are occurrence counts, not unique destinations. A URL bounded for evidence is labeled and never presented as a request that happened.

### Inventory

Overview and Internal expose a compact **Export inventory…** action. A temporary scope row offers **All captured URLs** (default) or **Visible Internal URLs**, then a notice says the immutable snapshot is being prepared and the native save dialog follows when it is ready. The export may run during a crawl; its snapshot column says **in-progress** and records the capture time. The inventory is one repeated-context row per captured URL; it is not a second table or a permanent export toolbar.

### Duplicate content

Internal adds a compact **URLs / Duplicate content** switch in the results toolbar; it is not a new top-level tab. Duplicate content has **Same text / Similar / Not compared** controls with counts, a short coverage/status summary, its own search, sort, selection and export. Same-text rows show a representative URL, matching URL count, selected-text length and capture stage; select a row to inspect the hash, scope and a bounded member list with pagination and page actions. Similar rows show two URLs, verified overlap and exact-member counts; the inspector shows stage, scope, token counts, side-by-side excerpts, expandable exact memberships and the approximate-candidate caveat. Not compared explains the URL-specific eligibility or limit reason. The table stays full-bleed and 30px rows at 880px; long URLs truncate but copy/open retain the full value.

### Custom extraction

**Custom extraction…** opens a compact scrolling rule editor under the command bar: name, selector, Text/Attribute and First/All, with an attribute field when needed, errors beside the rule, and a one-page **Test on URL** preview with Stop. It is a disclosure, not a new tab or modal. A completed preview names the mode actually captured and the rule definitions actually used; editing drafts afterward marks the old preview stale rather than changing its labels. The Internal inspector shows a **Custom extraction** disclosure with retained rule values, stage and state. Values are monospaced measurements only where numeric; extracted page text remains sans.

### Overview

The default view is a compact summary, not a URL table. A site/run line names the host, captured URL count, actual capture mode and live or terminal outcome. Below it, a plain list shows only nonzero finding groups — response/fetch, indexing and canonical review, titles, descriptions, headings, image alt and broken links — each with an explicit affected-URL count and a **Show** action that opens the matching table with Findings selected and any hidden query cleared. A visible note says counts are per check and may overlap; they are never summed into a unique-page claim.

Coverage is separate: incomplete checks, incomplete sitemap URL checks, sitemap file findings, partial sitemap discovery, and stopped/limited/failed states stay visible. Each count uses the same existing filter its Show action opens; guessed absent sitemap roots stay informational. **No findings in completed checks** is never presented as an all-clear when coverage is missing. With no run, Overview invites a URL and Crawl. **Open URL table** and **Export CSV** keep the table and its export obvious; Cmd/Ctrl+F moves to Internal search. Overview uses cached count facts and does not rescan the dataset per frame.

Sitemaps carries URLs and Files views in the section header rather than as extra tabs. The URLs view lists each address, its state, status, source file, and issue; the Files view lists each document, its origin (robots.txt, index, or guessed), state, status, URL count, and issue. Both use the same table geometry, sorting, selection, per-view search and All/Findings/Incomplete filter, and CSV export as the other tabs.

Internal uses an **INDEXABILITY** column instead of a yes/no answer. The primary label is one of Fetch failed, HTTP error, Redirect, Noindex, Review canonical, Canonical elsewhere, Not assessed, or No indexing block detected. The full reasons and raw evidence live in the drawer and the Internal CSV; the row keeps only the label so the address stays readable at 880px. The Internal CSV adds **CAPTURE** after **INDEXABILITY**, and the Sitemaps URLs CSV adds **CAPTURE** after **SOURCE**, using the drawer's state wording and appending the renderer's stop reason when a Browser capture is incomplete.

### Table
- **Header:** 32px tall, 10px mono uppercase labels in muted ink, sortable with an arrow on the active column.
- **Rows:** 30px, no zebra striping, a hairline rule between rows only. The selected row uses an 8% ink wash in dark mode and 6% in light.
- **Cells:** sans for content, mono and right-aligned for status codes and counts. Long URLs and titles truncate with an ellipsis rather than wrapping.
- **Default order:** Internal sorts by indexability severity, with fetch failures, HTTP errors, and noindex first. Other tabs sort rows with findings first, incomplete checks next, and clean rows last; sortable headers let users choose a different order.
- **Issues:** empty issues read "OK" in muted ink; hard failures are red; soft issues are amber. Findings may include a confirmed noindex, error or broken link even when the same row is also incomplete; completed content checks remain withheld. A non-auditable page shows a faint em dash, because a 404 has no title to judge. Indexability follows the same three-level signal: Fetch failed, HTTP error, and Noindex are red; Redirect, Review canonical, and Canonical elsewhere are amber; Not assessed is muted; No indexing block detected is green. Missing canonicals and other informational absences stay amber or muted, never red. A successful non-HTML response is informational context: it reads Not assessed in muted ink and does not join Findings or sitemap issue counts.

### Contextual Inspector
The 320px inspector starts hidden and opens when a visible result is selected or the details control is pressed. Its fixed header names the active view, with Previous/Next visible-result controls, a close control, and copy/open actions for the selected URL. The scrollable body leads with the selected tab's finding, not generic indexability:

- Internal leads with the evidence-backed assessment and its primary reason, keeping staged evidence and capture provenance in disclosures.
- Titles and Meta show the full value, measured character count, the existing Duplicate/Too long/Too short result, and clickable duplicate peers. The length ranges are named as this tool's heuristic.
- H1 lists every captured H1 in order and duplicate peers; a Multiple finding is never reduced to the joined row text.
- Response shows the actual status or error, redirect destination and known chain, with fetched targets inspectable and unchecked targets openable/copyable.
- Canonical shows the assessed declaration, staged changes, and the target's observed status when fetched.
- Images shows observed counts only when the capture completed; element-level image data is explicitly left for a future data change.
- Links lists observed incoming sources, outgoing targets and broken targets/statuses, bounded with totals and labeled as crawl-observed rather than a site-wide backlink map.
- Sitemaps keeps state/reason and next actions first: inspect the fetched page, inspect the source sitemap, or open/copy the target.

Related in-dataset URLs are selected in the table and update the inspector without a network request; if the active search or filter would hide one, the inspector clears only the conflicting state and says so. Previous/Next follow the same visible order as the table and CSV. Long H1, duplicate and inbound/outbound relation lists mount only one 25-row window at a time, with Previous/Show next controls and a visible Showing X–Y of N label; every retained item remains reachable without cumulative mounting. Outgoing targets keep broken destinations first, and full-value copy actions remain available. Each new selection starts at the top of the inspector. Full values and evidence have copy actions; a disclosure collapsed with the triangle can also be closed with Escape, but Escape never stops a crawl. Ordinary page fields follow below one hairline, with an em dash for unknown counts rather than an invented zero. Sitemaps keeps its state, source, response, evidence and target fields in a separate bounded scroll region.
Completed-run reporting reads the saved run snapshot, not the next-run controls: changing Sitemaps, URL or limits, or rejecting a next-run start, never rewrites the coverage, settings, units or terminal reason of the results already shown.

**The Honest Capture Rule.** A browser capture that did not finish never reads as a clean page. Its capture state, missing document, and reason stay visible in the drawer, row label, sitemap entry, and CSV, and a timed-out or blocked render is Not assessed rather than “No indexing block detected”. A source restriction removed by script remains visible as source evidence; a rule still present after rendering is labeled rendered. A required script that fails to load also makes the capture incomplete: the shell may be valid HTML while the content that script would have hydrated is missing.

### Status Footer
30px tall with a hairline above. The 5px status dot and 10.5px mono message stay quiet. The actual crawl outcome, including a stop or page-limit reason, remains beside elapsed time. Long messages truncate with full hover help. Update status and the installed version sit at the right; neither is repeated in the tab bar.
### Empty State
Centered in the results area. A 13px uppercase mono title ("Crawl a site", "Preparing", "Fetching pages", "Couldn't crawl", "Blocked", "Page limit", "Stopped", "No matches", "No findings in this view") over one line of 12.5px sans copy. Filtered-empty states offer Clear search and Show all. No illustration, no oversized icon. Zero findings is described as this view's result, not proof that the site is clean.

### Progress
A 2px full-width track in hairline weak with an ink fill for pages found against the cap. It appears only while preparing, crawling or stopping and disappears without animation when the crawl ends.

### Keyboard
`Cmd/Ctrl+F` focuses the results search (from Overview it opens Internal search), `Cmd/Ctrl+L` focuses the site URL, and Escape closes the inspector, run details or an open evidence disclosure. `Cmd/Ctrl+F` and `Cmd/Ctrl+L` are menu equivalents marked keys-first so typing and text editing keep their normal behavior. Table arrow keys move the visible selection and the inspector follows it.

## Do's and Don'ts

### Do:
- **Do** align every region to the same 20px gutter.
- **Do** use mono for anything the tool counts, names, or measures, and sans for anything the crawled page says.
- **Do** let healthy rows stay muted; reserve green, amber, and red for state that changes a decision.
- **Do** express grouping with a 1px hairline and spacing before considering a fill.
- **Do** keep one filled action on screen: CRAWL at rest, STOP while crawling.
- **Do** keep row and header heights fixed at 30px and 32px so the table reads as a ledger.
- **Do** use an em dash in faint ink for missing values instead of inventing a zero or a blank cell.
- **Do** derive the light theme from the same seed so the window follows macOS without becoming a different product.

### Don't:
- **Don't** add cards, shadows, glows, gradients, or floating panels.
- **Don't** use a display face, an oversized heading, or a fluid type scale.
- **Don't** use pills, large radii, or rounded containers.
- **Don't** fill a second button, tint a whole row for status, or use green/amber/red as decoration.
- **Don't** wrap a table in padding or center it inside a max-width container.
- **Don't** set URLs or page content in mono just to look technical.
- **Don't** add icons, illustrations, empty-state art, or a wordmark. The header is nameless while the product name is provisional.
- **Don't** animate page-load sequences or add motion that does not report state.
