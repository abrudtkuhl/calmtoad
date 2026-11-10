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
  topbar: "52px"
  command: "40px"
  table-row: "30px"
  table-header: "32px"
  footer: "34px"
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

1. **Top bar** — 52px: eight tabs, then the crawl state at the far right. The tab row starts after the platform's titlebar inset so the macOS traffic lights never overlap a control; in full screen the inset is zero and the tabs return to the gutter. A hairline closes the bar.
2. **Progress line** — 2px, only while crawling: a hairline track with an ink fill for pages found against the cap.
3. **Command bar** — 14px vertical padding around a 40px strip: URL field, a vertical hairline, Pages, a vertical hairline, Depth, and the CRAWL/STOP action. The strip is one continuous outlined field group, not four separate inputs.
4. **Section header** — 12px top and 14px bottom: the tab's name in mono, a plain-language summary, and Issues only at the right.
5. **Results** — grows to fill the window: a full-bleed table, then a four-column detail strip pinned under it.
6. **Status footer** — 34px: one status dot, the crawl message in mono, and SAME HOST ONLY at the right.

Tables never sit inside a padded card. Header and cell text begin on the same 20px gutter as the section header, so every region shares one left edge. Columns use flex for content and fixed widths (96–180px) for counts and statuses; numeric columns are right-aligned. Row height is 30px, header height 32px. At 880×560 the layout holds: the table scrolls under the fixed detail strip rather than reflowing into cards.

### Named Rules
**The Nameless Header Rule.** The header begins with the first tab after the traffic-light inset. No logo, wordmark, or product name sits in the window, so the name can change without touching the layout.

**The Full-Bleed Rule.** Tables touch the window edge. The 20px gutter aligns the first and last cell; it does not become container padding.

**The One Column Rule.** No sidebars, no split panes, no floating panels. The crawl is one vertical reading order.

## Elevation & Depth

There are no shadows and no raised surfaces. Depth is expressed with three tools only: a one-step tonal lift for fields, selected rows, and the active tab; 1px hairlines that separate regions; and spacing that groups. The kit's neutral button "shadow" is a 1px spread ring used as an outline, not as elevation. The primary button's elevation is cleared, so it is perfectly flat.

**The Flat-By-Default Rule.** If a surface appears to float, remove the shadow. Reach for a hairline or a tonal step first.

## Shapes

Corners are nearly square. Fields, the command bar, and the primary action use a 4px radius; tabs use 2px. Status dots are perfect circles at 5px. The only silhouettes that recur are the rectangle and the hairline. There are no pills, no capsules, and no decorative geometry.

There is no logo, wordmark, or monogram. The first element in the header is the Internal tab, aligned to the gutter.

**The Near-Square Rule.** Radii stay at 4px or below. If a shape needs to feel softer, it is probably too large for this system.

## Components

### Buttons
- **Shape:** 4px radius for the command action; 2px for tabs. No pill buttons.
- **Primary — CRAWL:** bone ink fill on carbon ground (inverted in light), ground-colored mono label at 10.5px with 1.2px tracking, 40px tall. It is the only filled control on a resting screen.
- **Crawling — STOP:** the same geometry, but a field ground with signal red text and a hairline ring. The action changes role, not size.
- **Active tab:** a neutral fill one step above the page, ink label, muted count.
- **Idle tab:** transparent ground, muted label and count, no ring until hover.
- **Hover / Focus:** a 1px ring in hairline or hairline strong. No glow, no lift, no scale.

### Chips
Not used. Status is a dot plus a mono code, not a badge.

### Cards / Containers
Not used. The command bar is the only enclosed rectangle, and it is a control group, not a card. The detail strip is separated by a top hairline and no background fill.

### Inputs / Fields
- **Style:** transparent within the 40px command strip, 12.5px sans for the URL and 11.5px mono for the numeric limits. The group carries the 1px hairline and 4px radius.
- **Focus:** a 1px ring on the whole command bar (focus-within), not a glow on the individual field.
- **Disabled:** the text dims to muted while crawling; the strip keeps its geometry.
- **Placeholder:** faint ink at reading contrast, never lighter than necessary.

### Navigation
Eight tabs in one row: Internal, Titles, Meta, H1, Response, Canonical, Images, Links. Each is a ghost button containing an uppercase mono label and a separate muted count with a 6px gap. The active tab alone gets a 2px-radius neutral fill. Tabs never wrap; at narrow widths the row compresses rather than becoming a menu. Tab labels are authored in normal case and uppercased visually.

### Table
- **Header:** 32px tall, 10px mono uppercase labels in muted ink, sortable with an arrow on the active column.
- **Rows:** 30px, no zebra striping, a hairline rule between rows only. The selected row uses an 8% ink wash in dark mode and 6% in light.
- **Cells:** sans for content, mono and right-aligned for status codes and counts. Long URLs and titles truncate with an ellipsis rather than wrapping.
- **Issues:** empty issues read "OK" in muted ink; hard failures are red; soft issues are amber. A non-auditable page shows a faint em dash, because a 404 has no title to judge.

### Detail Strip
Pinned under the table, separated by a top hairline. The selected URL sits on its own line in mono muted ink. Below it, four equal fields — Title, Meta description, H1, Canonical — each with a 10px mono uppercase label and a 12.5px sans value. Missing values are an em dash in faint ink. The strip never becomes a modal or a second table.

### Status Footer
34px tall, hairline above. A 5px dot carries the state; the message is 10.5px mono at muted ink; SAME HOST ONLY is the right-hand 10px mono label. The footer is the only place the crawl result is stated in full, including when the page cap stopped it.

### Empty State
Centered in the results area. A 13px uppercase mono title ("Crawl a site", "Fetching pages", "Couldn't crawl") over one line of 12.5px sans copy. No illustration, no oversized icon.

### Progress
A 2px full-width track in hairline weak with an ink fill for pages found against the cap. It appears only while crawling and disappears without animation when the crawl ends.

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
