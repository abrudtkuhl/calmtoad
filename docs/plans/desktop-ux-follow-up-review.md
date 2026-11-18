# Desktop UX follow-up release review

November 17, 2026. Working tree against shipped `b487b89` / v0.5.0. User authorized reviewing, correcting and shipping the current UX pass.

## Standards/correctness review

### Browser launch failure never reaches the new recovery controls

Filesystem discovery now marks an application Ready without launching it (render.rs, usable_browser). This is appropriate for quick discovery, but a subsequent controlled BrowserRenderer launch failure ends the run as Failed without changing browser.status (main.rs poll_crawl Done branch around1035). The new Retry / Use HTML controls only render for Unavailable (browser_strip around2687). Thus an unusable installed application remains presented by its name without the intended recovery actions, and the raw startup error can appear in the primary Overview outcome.

Route a real browser-setup failure into a typed/explicit UI recovery state, separate from ordinary page/domain failures. Show a short application-level message, keep the raw diagnostic in Details with copy access, and preserve Retry / Choose / Use HTML. Keep ownership cleanup and saved-run reporting correct. Add a regression using the actual failed Browser preparation path; do not misclassify all failed Browser crawls as installation failures.

## Spec review

### Sitemap coverage count differs from its Show destination

main.rs sitemap_unchecked counts only state != Checked. The Overview row opens Sitemaps URLs with Incomplete, whose predicate also includes checked entries with incomplete capture/read evidence. One unchecked entry plus one checked-but-truncated entry advertises1 but opens2.

Make the displayed label/count and actual navigated filter express the same set. Prefer the existing filter vocabulary over introducing a hidden extra filter: an accurately named incomplete-sitemap-check row counted with the existing predicate is acceptable. Do not call a checked-but-incomplete result simply unvisited. Regression should click the Overview action and compare count, visible rows, copy and CSV scope on a mixed report.

### Successful Browser non-HTML responses become incomplete checks

main.rs page_incomplete uses !capture.is_complete unconditionally. CaptureInfo::is_complete requires a rendered DOM. A successful Browser PDF/non-HTML response legitimately has source_state NotApplicable and no rendered DOM, so the new Overview counts it as incomplete despite its valid Not assessed context. Sitemap URL filtering shares the problem.

Make completeness appropriate to the document/response type while preserving truthful failures, truncation, cancellation, missing HTML and essential-script errors. A complete non-HTML observation is informational, not a failed check. Test Browser PDF/non-HTML alongside HTML missing DOM and actual truncated captures across Overview, filters, row and CSV semantics.

### Sitemap file failures are absent from Overview coverage

Overview coverage only considers incomplete pages, unchecked listed URLs, report.partial and terminal run phase. A declared sitemap file that fails, or a skipped file, can have no listed URL and no report.partial. Its file issue is then invisible in the default view, while a positive captured-URL coverage message can be the only coverage line.

Expose known sitemap file findings in Coverage with a count and Show action that opens Files with the matching existing filter. Keep absent guessed roots informational and pending discovery distinct. Cache/update this small report fact consistently while streaming and after completion. Test file failures without listed URLs or report.partial.

## Native picker validation completed by root

The actual review build was launched as an isolated temporary app with CALMTOAD_DATA_DIR under /private/tmp. Using native computer control, root selected Browser, opened Choose browser, observed Applications selected as the initial directory, clicked Google Chrome.app directly, and confirmed Open returned to a Google Chrome setup strip. No package contents or executable paths were needed. The isolated updates.json contains browser_path=/Applications/Google Chrome.app. This establishes real native .app selection and persisted writing; it does not claim an interactive restart or native Windows validation.

Temporary review app: /private/tmp/calmtoad-ui-review-8hap22b6/Calm Toad Review.app. Current visible product build was before the corrections in this report.

## Implementation handoff

OpenCode owns corrections to this report. Keep the compact UI and all existing changes. Read the complete original brief. Add meaningful regression tests, inspect changed states at880x560 and1180x740 in both themes, run focused checks and relevant browser tests. Do not commit/push/tag/release; root will verify and perform the already-authorized release. No unrelated website edits.

## Root reproduction

Two isolated tests in a temporary copied main binary reproduced both coverage findings before corrections: successful Browser PDF failed the expected not-incomplete assertion, and the mixed sitemap report advertised1 while its destination predicate matched2. Command: `cargo test --bin release_review_probe root_release_probes -- --nocapture`. Both tests ran and failed on these exact assertions; initial temporary harness path/private-field compile errors were corrected before that reproduction. The copied probe and src/bin directory were removed in cleanup. No production source was edited by root's probes.

## Follow-up independent reviews of corrections

Both read-only reviewers found the original four paths addressed. Neither repeated the full suite.

### Standards

- P2: Stop during Browser preparation is still classified as Failed/setup_failure by crawl_site_with, even though BrowserRenderer::launch explicitly returns cancellation errors. This then marks a healthy browser Unavailable. Handle user cancellation as Stopped with no setup_failure and cover actual preparation cancellation. This violates the documented truthful stopped-outcome behavior.
- P3: Browser Details copies the path but not the raw diagnostic text. DESIGN.md's browser strip requires raw diagnostics behind Details with copy access. Add a diagnostic copy action, including automatic-discovery failures with no explicit selected path.

### Spec

- P2: The brief's safe cancellation / truthful stopped-state requirements are not met when a cancelled launch returns Failed/setup_failure. The deterministic pre-cancelled Browser crawl and actual Stop-during-Preparing paths need Stopped, not an installation error.
- P3: The brief explicitly requires raw diagnostic copy access; the current control only copies a path. Copy the diagnostic itself.

Standards:2 findings, worstP2 cancellation. Spec:2 findings, worstP2 cancellation. These are the same two remaining behaviors identified independently; do not count them as four unique defects. OpenCode is to correct them before final validation.


## Final local hand-back

The original four findings and the two follow-up behaviors are corrected. Both independent reviewers confirmed the remaining cancellation and diagnostic-copy changes; no residual concrete blocker was found in their scoped follow-up.

OpenCode's completed local summaries report97 library tests,68 UI tests (one measurement ignored), and30 browser integration tests passing. Root independently passed the final memory-clipboard diagnostic-copy and stopped-run/browser-readiness Runtime regressions on v0.6.0, all-target Clippy with warnings denied, formatting, whitespace and release-note validation. Root additionally required a real installed Chrome launch with CALMTOAD_REQUIRE_BROWSER=1 for successful_browser_non_html_response_is_complete_context; it passed with one test and no skip.

Root's isolated preference directories prevented these final checks from writing to the installed app's settings. The fresh v0.6.0 Overview screenshot is assets/overview.png and was visually inspected. The native Mac picker check is described above. At this local hand-back, exact-commit macOS/Windows/Linux CI remains the release gate; no local cross-compile result is described as Windows runtime verification.
