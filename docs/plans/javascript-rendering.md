# Optional JavaScript rendering

Status: ready to build. User authorized a plan and implementation handoff to OpenCode. Build a better local desktop crawler; no Cloud/account/monetization dependency.

## Product outcome

Choose **HTML** or **Browser** before crawling. HTML remains the fast default and requires no browser installation. Browser uses a local headless Chromium browser to execute page JavaScript, extract the rendered document, discover its links and run the existing audits. Results clearly identify the capture mode and any render failure or incomplete capture.

This is one desktop rendering mode, not a browser automation product. No login flows, user profiles, clicking, form submission, consent handling, scrolling/infinite-scroll crawling, mobile emulation, custom JavaScript injection by users, screenshots as a new product feature, source-vs-rendered comparison dashboard, accessibility auditing, or Cloud execution in this build. Browser resources may naturally include requests to CDNs/API hosts; those are not additional crawl pages.

## Browser setup and implementation choice

- V1 uses a locally installed supported Chrome or Chromium. Detect common platform installations, and support an explicit browser executable path. If none is usable, explain that Browser mode needs Chrome/Chromium and offer a native path chooser/retry. On macOS accept/resolve a browser app bundle if the chooser permits it. Validate the selection and show failures plainly.
- Do not silently download a browser, require Node/Python/ChromeDriver from end users, alter OS security settings, or launch the user's normal browsing profile. HTML mode works even if Browser mode is unavailable. Preserve existing portable release assets and updater behavior.
- Use a Rust browser automation library rather than implementing a WebSocket/CDP client from scratch. Start by evaluating chromiumoxide against the proof below; headless_chrome is an alternative if verified build/runtime constraints make it a better fit. Pin the selected dependency and record the actual decision. Keep async/runtime details inside a focused renderer module; do not migrate the whole app to async or build a plugin framework.
- First make a real local-browser proof: launch with an isolated temporary profile, capture main-document status/repeated headers and initial body evidence, execute a delayed DOM change, intercept a cross-host navigation before it happens, and cancel/close the process. Confirm library capabilities and cross-platform build feasibility before extensive UI work. A mock-only renderer is not a completed implementation.

## Runtime and ownership

- One managed browser process per Browser-mode crawl, with at most two concurrent page render jobs initially (not the HTML pool's sixteen). Reuse the process, use a fresh isolated context per page to prevent cookies/cache/storage leakage between URL checks, and cleanly close pages/contexts after each job.
- Use a temporary application-owned profile; never attach to a user's live browser/debugging session. Keep browser sandboxing and TLS verification enabled. Bind any required debug endpoint to loopback with library-managed lifecycle; avoid exposing it on the network. Deny downloads, permissions, popups/new windows, non-HTTP(S) navigation and arbitrary local file access.
- Keep browser ownership explicit so Stop, normal completion, panic, failed startup, timeout, browser crash and app shutdown all close the crawl-owned browser process tree/profile. Never terminate unrelated user browsers. Stop must interrupt a hanging navigation/evaluation rather than wait through every queued timeout.
- Renderer work runs off the UI thread. Render/browser settings are fixed for a run, and old events cannot update a newer crawl. In-app installation must remain unavailable while either rendering or HTML crawl work is active.

## Shared crawler integration

Extend CrawlOptions with an explicit render mode/configuration; preserve HTML behavior for existing callers/benchmarks. Introduce a small renderer interface and structured output/error/evidence. Keep production transport behind the existing crawl scheduling policy; HTML and Browser should share normalization, deduplication, scope, page/depth limits, sitemap seeding, audits and progress events.

- Robots.txt and sitemap files remain ordinary bounded HTTP metadata fetches, not browser pages. They don't require JavaScript or consume the page budget.
- Browser page jobs share the same total page budget as HTML jobs. Subresources do not count as page results; document hops that become crawl results do. Preserve retry/backoff and per-origin crawl-delay behavior for admitted document requests. Do not send a second independent HTML request merely to obtain status/headers and pretend it was the rendered navigation's response.
- Honor the crawl's existing page scope and robots policy BEFORE navigation, not only when processing the resulting URL. Recheck before every HTTP redirect and script-triggered top-level navigation; use interception. Never load an out-of-scope/robots-blocked top-level destination and discard it afterward.
- Keep same-host HTML crawl semantics and the tighter existing sitemap-origin policy; don't broaden them for rendering. Subresources needed to render may use other HTTP(S) hosts, but those hosts' pages are not enqueued. Enforce bounded resource loading; handle blocked/failed resources as capture evidence rather than silently calling the result complete.
- Use a consistent documented crawler identity for outgoing document requests and robots evaluation; no Googlebot impersonation. Record actual browser version and mode for diagnosis.

## Navigation, response facts and URL identity

Browser automation follows redirects by default; the adapter must integrate with Calm Toad's explicit redirect records and page budget instead of hiding hops. Record source 3xx status/location and schedule an allowed target through the normal queue. Inspect response headers/intercept the redirect before the next document request. Avoid counting a redirect's final rendered 200 as though it belonged to the original URL.

For client-side location changes, meta refresh or history URL changes, preserve requested URL versus observed document URL. Do not invent HTTP 301/302 for a script redirect. Record the transition/capture interruption truthfully, and admit any target under scope/robots/dedup/budget rules. Do not audit the destination's DOM as the original URL. Cap navigation loops (five document transitions/hops per job is a reasonable initial guard) and surface a bounded failure.

Only main-document response events determine the page's HTTP status/content type/headers. A successful subresource must never overwrite a 404/500 main response. Identify frame/loader/request IDs correctly across navigation; handle duplicate/reordered browser events. Preserve every applicable X-Robots-Tag occurrence/scoped directive and existing evidence caps/omitted restrictions. Distinguish initial response metadata from final DOM metadata.

## Readiness and capture limits

Use a defined, bounded readiness policy rather than unbounded network-idle waiting:

- Fixed desktop viewport, initially 1365x768 CSS pixels; no device/mobile presets yet.
- Wait for main-document DOMContentLoaded, then a short 1-second settle window for app hydration. A continuously polling/streaming page must still finish.
- Hard 15-second total per-page deadline, covering navigation, settling and extraction. Keep these defaults in a documented render configuration; advanced user tuning can wait.
- Retain the current 1,000,000-byte HTML/DOM evidence cap; capture one-byte-over/cap metadata where possible and report truncation. Don't fetch or serialize arbitrarily large bodies into Rust just to truncate afterward.
- Bound resource requests and transferred bytes per page (start with 300 HTTP(S) requests and 50 MiB transferred), with documented handling when a limit is reached. Stop loaders/work when capped. These are guardrails on metadata/resource work, not a new maximum site page count.
- Page scripts run in Chromium and can consume browser resources; enforce deadlines and process cleanup instead of promising a precise portable hard memory limit. Browser crash/process failure should stop or fail remaining Browser jobs explicitly, not fall back silently or strand the crawl.

Capture document HTML after the readiness policy and feed it into the existing single-pass parser. Do not parse a headless command's output as a substitute for network/readiness/provenance control. V1 covers the top-level document's captured DOM; it does not promise all lazy-loaded states, iframe documents or shadow-root content. State actual capture limitations succinctly in contextual help/docs.

## Source/rendered evidence and audits

- Preserve bounded initial main-response HTML evidence for indexing directives and distinguish it from rendered observations. A source noindex removed by JavaScript must remain visible as an observed source restriction; don't erase it just because the final DOM differs. Render-added noindex is also relevant. HTTP directives always retain their HTTP source.
- Preserve the existing policy for repeated/agent-scoped directives and restrictions beyond the raw evidence cap. Never merge source and rendered copies as if duplicated tags appeared twice in one document; label capture stage.
- Use rendered title, description, headings, links and image alt metadata for Browser-mode content audits. Record whether a rendered canonical differs from the source, retaining both observations. A source canonical replaced by one rendered canonical is a change across captures, not automatically two simultaneous conflicting declarations. Reuse the shared indexability model with deliberate capture-stage support rather than forked string logic.
- Keep response status/headers authoritative and audit only appropriate HTML content. Rendering a branded HTML error document does not make its HTTP404 auditable as healthy HTML200. PDFs/non-HTML remain informational Not assessed unless actual header directives/response findings say otherwise.
- Introduce structured render states such as captured, timed out, failed, blocked/navigation interrupted, and truncated/resource-limited. Incomplete render/source evidence must not become No block found. A partial DOM may be inspectable, but its incompleteness must propagate into indexability, sitemap state/details and exports. Never silently retry a failed Browser job as HTML and label it rendered.
- Rendering does not prove Google has indexed a URL, sees identical content, or has selected the declared canonical. A defined settle policy captures one observed state.

## Desktop controls and results

- Add a compact HTML/Browser mode selector near the existing crawl controls, default HTML. Browser-only setup/status can use a secondary compact row/popover; do not crush the URL field at the 880px minimum width or add a new tab.
- Disable mode/config changes during a crawl. Keep Stop, page/depth limits, Use sitemaps, sorting, Issues only, selection and CSV behavior familiar.
- In Browser mode show enough run context to identify the mode and browser availability. Provide clear setup failure, launching, rendering, stopping and error states without an install wizard.
- Extend page details with requested/observed URL when different, capture mode/state, elapsed render time, source-vs-rendered directive/canonical evidence and relevant failure/cap reason. Keep raw evidence readable/copyable with scrolling.
- CSV and checked sitemap rows retain rendering provenance and incomplete reasons. Add mode/state fields deliberately to relevant existing exports; other columns/escaping/filter/sort remain intact. Unknown/render failures must not inflate healthy-page counts.
- Follow DESIGN.md and current light/dark styles. Add offline scenes for Browser selected/unavailable and mixed render outcomes; inspect minimum and normal window sizes in both themes.

## Implementation order

1. Complete the actual-browser capability proof and dependency decision; document supported browser discovery/configuration and missing-browser behavior.
2. Build managed browser lifecycle, isolated contexts, navigation interception, bounded readiness and structured capture output with deterministic local fixtures.
3. Integrate the shared scheduling/robots/redirect/page budget/cancellation path and source/rendered indexability evidence. Preserve HTML mode performance and no-browser operation.
4. Add mode/setup UI, result provenance/details/CSV, offline scenes and documentation.
5. Run complete validation and return a review-ready implementation. Do not stop after a backend prototype or mock tests.

## Acceptance and verification

Use local HTTP fixtures and an actual browser for integration tests:

- JavaScript adds title, description, heading, canonical, image and a same-host link after a short timer; Browser sees them and follows the link, HTML does not.
- HTTP404 with a rich JavaScript DOM remains404; repeated X-Robots-Tag and source noindex removed after rendering remain observed. Render-added noindex and source/rendered canonical changes have correct stage labels and shared assessment/CSV agreement.
- HTTP redirects, JavaScript redirects, meta refresh, same-document URL changes, loops and a cross-host/robots-blocked destination are handled without forbidden navigation or budget bypass. Assert forbidden endpoints were never requested; do not only inspect the final results.
- Polling/never-idle page finishes by the defined policy; slow/hanging navigation, evaluation hang, oversized DOM/resources, browser startup failure, missing executable and browser crash terminate predictably. No fake200/No block found or implicit HTML fallback.
- Stop during startup/navigation/settle/extraction closes owned resources within bounded time; cancellation leaves no stray owned process/profile, and normal user browser sessions are unaffected. Repeat start/stop and multiple crawls.
- Page cap, depth, sitemaps, deduplication, backoff, concurrency-two and source provenance agree across both modes. HTML tests/benchmarks do not launch or require Chromium.

Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, cargo test. Keep browser integration tests deterministic and separate from pure tests, with explicit missing-browser skip locally, but configure actual-browser smoke tests in platform CI so Browser support isn't only tested through mocks. Document browser installation/path for CI and ensure all four release targets compile. No silent --no-sandbox workaround; surface actual CI/platform limitations.

Keep performance checks local. Compare HTML mode against the existing synthetic benchmark, and measure a separate rendered local fixture without claiming network/browser timings equal parser microbenchmarks. Do not reset baselines to hide regressions. Report browser/version/viewport/readiness in browser measurements.

## Coordination and handoff

Work only in /Users/andybrudtkuhl/Dev/calmtoad/app. At planning, Cargo is0.4.0 and app tree was clean; recheck and preserve others' work. Website polish belongs to calmtoad-eng; no marketing-site edits. Preserve signed-fixture .gitattributes, updater security and local-only perf policy.

User authorized implementation now. Update README, PRODUCT, relevant DESIGN details and CHANGELOG Unreleased with implemented behavior and honest limitations. No version bump, commit, push, tag, release or deployment until review and a separate shipping instruction. Use the existing configured OpenCode session/model. If browser-library/runtime/provider permissions prevent completion, report the precise blocker and evidence rather than substituting mock-only support.

## Primary references

- https://developer.chrome.com/docs/chromium/headless — current headless browser model.
- https://github.com/mattsse/chromiumoxide — Rust async browser automation and generated command/event support.
- https://github.com/rust-headless-chrome/rust-headless-chrome — alternative Rust automation library/interception support.
- https://chromedevtools.github.io/devtools-protocol/tot/Network/ and /Fetch/ and /Page/ — document response/navigation/interception semantics; use through the selected library.
- Current src/crawl.rs, src/indexability.rs, src/sitemap.rs, src/main.rs, src/export.rs, DESIGN.md and PRODUCT.md.
