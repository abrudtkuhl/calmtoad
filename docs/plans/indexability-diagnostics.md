# Clear indexability diagnostics

Status: ready to build. User authorized a plan and implementation handoff to the existing OpenCode agent. Desktop quality comes first; this is a local feature with no account or Cloud dependency.

## Outcome

For an inspected URL, answer: **What did this crawl observe that may prevent or discourage indexing, and where did that signal come from?**

Replace the ambiguous INDEXABLE Yes/No-style result in the Internal tab with an evidence-backed assessment. Clicking the row reveals every relevant reason, source and a conditional next step. The result is not a claim that Google has indexed the URL or will choose its declared canonical.

Keep the existing tabs. Do not add a tenth tab, site health score, setup wizard, external API integration or JavaScript rendering. No Cloud work, saved projects, ranking prediction, soft-404/content-quality classifier, automatic fixes, robots simulator for arbitrary agents, or historical comparison.

## Current implementation and gaps

- src/crawl.rs Page::indexable currently means auditable HTML 200 without Page.noindex. index_issue collapses other HTTP results into “Not indexable” and treats non-HTML as an exclusion.
- Page.noindex combines markup/header into one bool; raw values, source, multiplicity and agent scope are lost.
- parse_html stops after the first robots/googlebot meta tag; combining the tags can depend on their order. Repeated declarations can be missed.
- from_response reads one X-Robots-Tag value. Reading errors/truncated bodies can be hidden, so missing evidence can look healthy.
- The first HTML canonical is resolved to one String; missing, malformed, multiple and conflicting declarations are indistinguishable.
- src/sitemap.rs has its own page_findings string builder. Internal, drawer, sitemap results, CSV and counts must agree about the same evidence.

## Assessment model

Implement a small, pure indexability module (for example src/indexability.rs) with structured assessments and reason codes, rather than strings computed independently in each view. Collect bounded raw evidence during fetching/parsing; derive labels, severity, explanations and counts from one assessment. Preserve existing Page/Fetched callers through deliberate updates/defaults where useful; update sample fixtures and benchmarks consistently.

Two separate concepts must remain visible in the model:

1. **Visit state:** fetched, pending, blocked by crawl policy, skipped, or not checked (page/depth cap or Stop), using existing sitemap/discovery evidence where available.
2. **Observed indexing assessment:** hard directives, HTTP response facts, advisory canonical signals, incomplete/unsupported assessment, or no blocking signal found by the implemented checks.

Do not manufacture Page records for unfetched URLs. Existing sitemap entries can display Not crawled / Not checked plus their reason. This feature does not require a new exhaustive inventory of every skipped ordinary link. A robots.txt block describes the actual Calm Toad crawl policy; it is not proof that Googlebot is blocked, that noindex exists, or that Google has excluded a URL.

## Primary labels and supporting reasons

Choose one concise primary label with deterministic precedence, retain ALL supporting findings, and test the precedence:

- **Fetch failed**: transport/body-read failure, including a 200 whose body could not be read. Preserve any response/header evidence that was obtained.
- **HTTP error**: actual 4xx/5xx, with exact status (including authentication failures). Do not call transient 5xx permanent removal.
- **Redirect**: observed redirect response and target. Handle 304/other non-content responses explicitly instead of claiming every 3xx is a usable redirect.
- **Noindex**: an applicable observed noindex/none directive. This can also be observed on a non-HTML response via headers.
- **Canonical elsewhere**: one valid HTML canonical points elsewhere. Advisory canonical preference, not a guaranteed indexing prohibition or Google's chosen canonical.
- **Review canonical**: invalid/empty href, conflicting/multiple canonical declarations. Preserve raw/resolved values and state what was found. Repeated identical declarations can be a distinct advisory rather than treated as competing targets.
- **Not assessed / Assessment incomplete**: successful non-HTML content without a blocking directive, unsupported response status, truncated body, or other explicitly incomplete evidence. Non-HTML content can be indexed; this crawler does not audit it as HTML.
- **No block found**: supported, adequately read HTML 200 with no applicable noindex or canonical concern. Missing canonical alone does not prevent this result. Explain once in details that this means no block found in supported crawl checks, not confirmation of indexing.

If an incomplete response has a known noindex/canonical finding, keep that finding and the incompleteness notice. Establish and document precedence so row status never hides the richer evidence. Preserve distinctions between issue, advisory and unassessed state; do not paint every unknown/intentional configuration red.

## Robots directive evidence

- Capture all generic robots meta and googlebot meta declarations in server-returned HTML, irrespective of order. Record their source, raw content and agent scope. Preserve other agent-specific declarations as evidence if collected, but do not apply bingbot/googlebot-news/unknown-bot restrictions as generic or Google Web Search rules.
- Collect all X-Robots-Tag header occurrences using the response library's multi-value support. Correctly interpret generic and agent-scoped rules; keep unrelated agent rules scoped. Do not treat every colon (for example in max-snippet/unavailable_after) as an agent prefix, or every noindex substring as an applicable directive.
- Match directive tokens case-insensitively with appropriate separators. none implies noindex and nofollow; all/index does not cancel an applicable restrictive noindex elsewhere. Unknown directives can be shown without inventing their effect.
- nofollow is separate information; it does not by itself prohibit indexing. This feature does not change crawl traversal behavior based on nofollow.
- For the primary assessment, apply generic and Googlebot Web Search noindex evidence, showing the exact source/scope (for example “Googlebot meta: noindex”). Do not impersonate Googlebot or alter the HTTP User-Agent. No claim of complete Googlebot robots.txt simulation.
- Preserve Page.noindex only as a compatibility derivative of the correctly combined relevant evidence, never as a conflicting second truth.

## Canonical evidence

- Inspect all HTML rel=canonical declarations and retain raw href, resolved normalized URL, multiplicity and validity. Resolve relative URLs using existing base/URL behavior. Do not discard invalid declarations and relabel them “missing.”
- Compare normalized URLs without dropping query parameters or pretending different hosts/schemes are equivalent. Self canonical is normal. Missing canonical is an informational absence, not proof of non-indexability. A canonical to another URL may be intentional.
- If a declared target was already fetched, show its observed HTTP/noindex status as supporting evidence; if not, say target not checked. Do not request additional canonical targets, follow outside the crawl scope, or consume extra page budget for this feature.
- V1 covers HTML canonical declarations. HTTP Link canonical parsing, rendered-JavaScript canonical changes, and Google's selected canonical are outside this build; keep the UI label specific enough to avoid claiming those were checked.
- Keep existing Canonical-tab checks where appropriate, but clearly distinguish a missing-canonical advisory from an indexing blocker. Avoid contradictory row/drawer/sitemap wording.

## Read completeness and resource bounds

Track body-read failures and whether the existing body limit truncated the response. Don't silently convert a failure/partial body into “No block found.” Avoid broad HTTP transport changes; retain existing timeouts, retries, size caps, robots policy and cancellation.

Bound the number and size of captured directive/canonical evidence entries per page; indicate when evidence was truncated. Do not retain entire response bodies or arbitrary headers after parsing. Preserve the current single-pass DOM traversal and avoid per-frame reclassification/string allocation for the full crawl. Cache derived assessments or build them incrementally as appropriate.

## UI and CSV

- In Internal, rename INDEXABLE to INDEXABILITY and replace Yes/generic failure text with the primary label. Show a concise reason column only if it remains useful at 880px; otherwise the primary label plus the details drawer is enough. Keep URL space usable and long targets truncated in rows, fully available in details/CSV.
- Extend the existing selected-page drawer with the assessment, all findings, source evidence, and short conditional guidance: e.g. “If this page should appear in search, remove the applicable noindex rule.” Avoid instructions to remove intentional redirects/canonicals blindly.
- Show raw meta/header directives, exact HTTP status, declared HTML canonical(s), resolved target and target-check state. Display absent evidence as Not observed rather than declaring unknown values clean.
- Internal Issues only, sorting and tab counts must use the same structured assessment. No block found is excluded; known findings/advisories included. Plain successful non-HTML exclusion from this HTML assessment is not an SEO defect; unknown states must not inflate confirmed-error counts. Document explicit count semantics.
- Preserve the existing Internal CSV columns for address/status/words; replace ambiguous INDEXABLE with INDEXABILITY and add REASONS/evidence columns needed to explain it. Export all reasons/source values in deterministic order; retain visible filter/sort and existing formula/CSV escaping. Other tab exports retain their meanings.
- Reuse the classifier for fetched sitemap URL findings, while retaining sitemap-specific visit state and findings/provenance. Pending/blocked/page-limit/Stop states stay truthful. Don't call a listed but unfetched URL healthy because no evidence exists.
- Follow DESIGN.md, existing drawer/table patterns and light/dark themes. No page-wide disclaimers; one brief explanation of assessment limits in the relevant details is enough.

## Build sequence

1. Add bounded evidence types, token/scoped-header parsing and pure assessment rules, with focused fixtures/tests.
2. Integrate evidence collection and read-completeness into crawl transport/parser without changing crawl scope/budget/robots semantics. Update Page/Fetched fixtures, sample data and library callers.
3. Wire Internal status, details, filters/counts/sorting and CSV, then reuse fetched-page assessment in Sitemaps and relevant Canonical details.
4. Add representative offline scenes, update README/PRODUCT and CHANGELOG Unreleased, and validate the complete feature.

## Acceptance and checks

Test at least: clean HTML200; self/missing/different/relative/malformed/empty/multiple identical/conflicting canonicals; fetched vs unchecked canonical target; generic/Googlebot directives in both orders and repeated tags; generic vs other-bot scoped headers; multiple X-Robots-Tag header lines; mixed case/whitespace; none/all; nofollow-only; index vs noindex conflict; noindex substring false positives; PDF200 with/without header noindex; 301/302 with/without target; 304; 404/410; 401/403; 429/503; network/read failure; body/evidence cap; sitemap blocked/unchecked/Stop entries.

Add a local HTTP integration fixture for repeated headers and body-read/truncation behavior, not only hand-built Page tests. Assert model/UI/CSV/sitemap agreement for a mixed-result crawl. Use existing test seams; don't disable Windows tests or normalize signed updater fixtures.

Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, cargo test. Keep benchmarks local: run relevant parsing/audit/counting checks; don't reset baselines to hide changes or restore perf gating in CI. Render normal/minimum window sizes in light/dark with long evidence and mixed states. Verify existing sitemap discovery, updater, CSV and crawl cancellation remain intact. Report tests/screenshots plus unsupported/unverified scenarios. No new network/Google-account dependency.

## Coordination and delivery

Work in /Users/andybrudtkuhl/Dev/calmtoad/app. As of planning, tree was clean and Cargo version 0.3.0. Recheck before edits, preserve other agents' work, and keep website changes out of scope.

User authorized implementation: build through a complete local review-ready result, not just a skeleton or another plan. No commit/push/version bump/tag/deploy until Codex review and a separate shipping instruction. Report exact provider/permission blockers promptly. Use the existing configured OpenCode session/model; don't switch providers or require paid credentials.

## Sources

- https://developers.google.com/search/docs/crawling-indexing/robots-meta-tag — multiple/scoped header/meta directives, supported agents, restrictive combinations and non-HTML noindex.
- https://developers.google.com/search/docs/crawling-indexing/robots/intro — robots.txt controls crawling; a disallowed URL can still appear in search.
- https://developers.google.com/search/docs/crawling-indexing/consolidate-duplicate-urls — canonical preferences are signals, not required indexing prerequisites.
- Current src/crawl.rs, src/main.rs, src/sitemap.rs, src/export.rs, DESIGN.md and PRODUCT.md.
