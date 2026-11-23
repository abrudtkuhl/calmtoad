# Accessibility checks — implementation report

November 22, 2026. Opt-in Browser-mode accessibility auditing for the work after the v0.9.0
release. This work is **not** part of v0.9.0 and has not been committed, pushed, tagged or
released.

## Baseline and release boundary

- Implementation baseline: `5f2226b7e88f8185f7734a6f76fd6b826a20554c`, `Release v0.9.0:
  structured data diagnostics`, which is also `origin/main` at the time of this work.
- Root opened the coordination gate after verifying the immutable pushed commit. No tag existed
  when implementation began; packaging remains owned by calmtoad-eng.
- All accessibility edits are local and uncommitted. The v0.9.0 release must build the pinned
  remote commit and must not include these edits.
- The v0.9.0 structured-data corrections remain intact. The existing structured-data focused
  suite passes unchanged.

## Dependency provenance

The locally bundled engine is **axe-core 4.14.1**, MPL-2.0.

| Artifact | Value |
| --- | --- |
| Pinned source tag | `https://github.com/dequelabs/axe-core/tree/v4.14.1` |
| Official npm tarball | `https://registry.npmjs.org/axe-core/-/axe-core-4.14.1.tgz` |
| Tarball SHA-256 | `e6b0ce02b84d892e2e33e403485e7f3d6c29e2be6df65daf44d89ba564139745` |
| npm integrity | `sha512-FHfa+lmNjQZfJz1q8wKqIDwC6DExcHt/pkjNQb09a3NPjCKg2rhu5qb2S4r76ASyFNNtuFooJpZe37OgudVGtQ==` |
| `axe.min.js` bytes | 653,780 |
| `axe.min.js` SHA-256 | `29348999450cdf2849a8e70b7912fc2da85a849da39c0d0d5394f51d4a9ed95c` |
| `LICENSE` SHA-256 | `af175b9d96ee93c21a036152e1b905b0b95304d4ae8c2c921c7609100ba8df7e` |
| `LICENSE-3RD-PARTY.txt` SHA-256 | `4f8563870d0fca38bbc3e00b6f670cb7fa9f380ba9f26a7f7d1184a6b18b1653` |

`axe.min.js` is a build artifact and is not committed at the source tag root, so the official npm
distributable was used. Its license files are byte-identical to the tagged source. The exact
reproduction and update steps are in `assets/axe/4.14.1/PROVENANCE.md`.

The bundle, MPL-2.0 license and third-party notices are embedded with `include_str!`; there is no
runtime CDN, npm, Node.js or download. `src/accessibility.rs` carries the version/hash/source
constants, and the crawl-option help names the pinned engine, license and source URL. An offline
provenance test checks the embedded bytes and notices.

## Profile, scope and limits

### Profile

`wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa`, `wcag22aa`, persisted as
`wcag2a+wcag2aa+wcag21a+wcag21aa+wcag22aa`.

- `runOnly: { type: 'tag', values: PROFILE }`
- `reporter: 'v2'`
- `resultTypes: ['violations', 'incomplete']`
- `iframes: false`, `preload: false`, `selectors: true`, `elementRef: false`

The union deliberately selects matching tagged rules even when a rule's default `enabled` value
is false (for example `target-size`). The selected-rule inventory is persisted separately from
the returned/executed rule IDs across violations, incomplete, passes and inapplicable results.
Rule-level counts are retained for all four outcomes; passed-node totals are not inferred from
reduced nodes. Rule identity/description/help-URL and tag-list truncation carry explicit flags.

### Declared scope

- Browser only, off by default.
- The same live rendered main document used for Browser capture, including supported open shadow
  roots.
- Frame contents, closed shadow roots, responsive states, interactive journeys, login flows and
  consent interactions are excluded. The permanent scope statement names frame exclusion even
  when no frame is observed.
- One fixed desktop viewport per run, recorded with the engine and profile.
- No second navigation, cloud service, AI remediation, PDF audit, user JavaScript, screenshot,
  automatic fix, browser download or plugin framework.

### Execution and retention limits

| Limit | Value |
| --- | --- |
| Existing Browser render deadline | 15 seconds |
| Additional audit budget | 10 seconds |
| Absolute per-job ceiling | 25 seconds plus bounded cleanup |
| Preflight elements, including open-shadow descendants | 20,000 |
| Retained rule/result groups per page | 128 |
| Retained affected-node occurrences per page | 200 |
| Retained occurrences per run | 10,000 |
| Accessibility evidence, caches and exports per run | 64 MiB |
| Projected browser-to-Rust payload per page | 512 KiB |
| Target selector component | 2 KiB |
| Target aggregate per occurrence | 8 KiB |
| Element HTML snippet | 2 KiB |
| Failure/help description | 4 KiB |
| Identifier | 256 bytes |
| Help URL | 2 KiB |

The 64 MiB ledger covers dynamic evidence, index copies, caches and exports. If the ledger is
exhausted, each page keeps only its bounded scalar execution/outcome/coverage summary so known
findings remain classified; rule, node, URL and other variable evidence is discarded and marked
limited. This is the per-page thin state allowed by the spec.
| Selected-rule inventory | 256 IDs |

The 64 MiB ledger is separate from the structured-data ledger. The host independently rejects an
oversized or invalid payload after the browser projection. The projection caps are not a claim
that axe's browser-process internals are limited to 64 MiB.

## Architecture

- `src/accessibility.rs` — typed execution/outcome evidence, limits, payload decode/validation,
  field truncation, conservative JSON/CSV size helpers, run budget and report leases, grouping
  search and page classification.
- `src/render.rs` — preflight, `Page.createIsolatedWorld` on the main frame with universal access
  disabled, embedded engine installation, `axe.run`, in-browser projection, cancellation,
  timeout and post-audit document identity checks. Audit failure never changes the SEO capture
  state.
- `src/crawl.rs` — `CrawlOptions.accessibility` / `accessibility_budget`, `Page.accessibility`,
  and Browser/HTML/off policy. HTML mode records `Requires Browser mode` without launching a
  browser. Page reports are built from the bounded scan and charged to the run ledger.
- `src/main.rs` — crawl-option checkbox, Internal Accessibility Pages/Rules views, independent
  search/filter/sort/selection, bounded background grouping, inspectors, Overview finding and
  coverage rows, offline scenes, and two CSV exports with frozen snapshots and shared leases.
- `assets/axe/4.14.1/` — bundle, MPL-2.0 license, third-party notices and provenance.

### Lifecycle

- A retained page report owns an `Arc<Budget>` lease; dropping the report releases its bytes and
  occurrences. The background Pages/Rules snapshot holds its own lease shared with the worker.
- Export snapshots, the preparation worker, the native save dialog and the background writer
  share an accessibility lease. Cancellation or a disconnected receiver drops only the UI
  owner; the last owner releases the reservation.
- The audit runs after settling, DOM capture and the second identity re-check, before page close.
  It gets no fresh render deadline. A third identity check and the isolated-world `document.URL`
  comparison discard stale findings if the main document changed.
- Audit cancellation/timeout is host-enforced. Existing page/context disposal and browser
  escalation remain authoritative; dropping a Rust future is not treated as proof that page work
  stopped.

## Verification actually run

Focused regressions only; independent broad validation remains with calmtoad-eng.

### Repository checks

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test --lib accessibility::` — **11 passed, 0 failed**.
- `cargo test --bins` — **110 passed, 0 failed, 7 ignored**.
- `cargo test --test structured_data` — **5 passed, 0 failed, 2 ignored** (existing structured
  corrections preserved).
- `cargo test --test browser_render accessibility` — **6 passed, 0 failed** against real Chrome
  and loopback fixtures.

### Real-browser coverage in the focused run

- The pinned engine reports `4.14.1` and the exact profile.
- A page-owned fake `window.axe` is never used; the isolated world succeeds.
- Known `image-alt` and `label` violations are retained, with open-shadow targets preserved
  structurally.
- A restrictive CSP page still audits through the DevTools-created isolated world.
- Frame contents are excluded: an alt-less image inside `srcdoc` is not retained.
- The audit does not navigate again; the fixture path is requested once.
- A 20,100-element document is declined as **Limited** with `preflight_limited`, while the
  rendered SEO capture remains successful.
- End-to-end Browser crawl charges typed evidence to the supplied run budget and occurrence
  counter.
- HTML mode with the option requested records **Requires Browser mode** and makes no audit or
  browser request.

### Unit/UI/export regressions

- Budget reserve/release, occurrence cap and report-drop release.
- Oversized, wrong-engine and structurally over-cap payload rejection; UTF-8-safe field caps;
  explicit rule-field and tag-list truncation flags; returned/executed rule IDs.
- Violation versus needs-review classification and maximum impact.
- Zero-violation all-clear only for completed, un-gapped scans.
- Field-wise search across later selector/HTML/failure fields.
- Pages filter keeps violation, needs-review, Requires Browser and off scans distinct.
- Rules grouping keeps unique affected URLs separate from repeated occurrences.
- Pages and Rules views fit 880×560, including export/copy/details.
- Native scenes for Pages, Rules, partial and off states render at 880×560 and 1180×740 in light
  and dark themes.
- Coverage CSV includes URLs with no retained node and preserves frozen order.
- Rules CSV expands every retained occurrence, preserves nested target arrays, JSON-escapes
  control characters in selectors and keeps CSV bytes within the reserved estimate.
- A runtime ceiling never returns a silently partial CSV.
- A paused accessibility export worker keeps the ledger charged after the UI job is dropped and
  releases only when the worker drops.

### Performance gate

`./scripts/perf-gate.sh` — **passed** (threshold 15%, baseline unchanged):

| Scenario | Delta | Status |
| --- | ---: | --- |
| crawl | +121.8% | info (`crawl` is informational in this gate) |
| audit | +1.4% | ok |
| audit_incremental | +6.9% | ok |
| counts | −3.2% | ok |
| parse_typical | +11.8% | ok |
| parse_large | +6.0% | ok |

Machine calibration was 0.99× baseline. No scenario was slower by more than 15%. The off path
adds one boolean check in render setup and an `Arc` field to `Page`; it does not launch a browser,
inject the engine, reserve the audit buffer or iterate result structures.

## Deferred / limitations

- The full Cargo and real-browser suites, screenshots and final performance sign-off belong to
  calmtoad-eng and were not run here to avoid competing with release validation.
- No fixture asserts a host-enforced audit timeout or a navigation that begins during the audit.
  The preflight limit, cancellation paths, document-URL guard and existing page disposal are
  covered; a deliberately wedged-engine timeout remains for independent validation.
- No request-level assertion proves every audit-originated network request is absent. The engine
  is embedded, `preload:false` and `iframes:false` are set, the audit script performs no fetch,
  and tests assert no second document navigation. Existing page scripts may still make their own
  requests under the normal interception policy.
- The 512 KiB payload cap is a projection/transport bound, not a browser-process memory cap. The
  preflight, deadline and page disposal are the containment for axe's internal allocations.
- Passed-element totals are not reported because `passes` nodes are reduced; only rule-level
  outcome counts are retained.
- One fixed desktop viewport is audited. Responsive states and interactive journeys are outside
  v1 scope.
- Native scene inspection was headless layout/theme coverage; no screenshots or image history
  were produced.

## Current state

All changes are in the working tree only. No commit, push, tag, version, workflow, website or
release action was performed. Unreleased documentation is updated; v0.9.0 remains the pinned
release baseline.
