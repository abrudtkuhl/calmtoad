# Accessibility checks research

Research date: 2026-11-22. Proposed scope: an opt-in Browser-mode audit of the current rendered page using a locally bundled, pinned axe-core. This note recommends a bounded v1; it does not implement it.

## Dependency and distribution

The official latest release is **axe-core 4.14.1**, released 16 November; its tagged package manifest confirms version `4.14.1` and **MPL-2.0**. Pin the exact package and record a SHA-256 for the vendored artifact. Bundle `axe.min.js`, its license and third-party notices, and provide the matching source/version link in About/notices. Do not load a CDN or auto-update the engine at runtime. [Official release](https://github.com/dequelabs/axe-core/releases/tag/v4.14.1), [tagged package manifest](https://github.com/dequelabs/axe-core/blob/v4.14.1/package.json).

MPL §§3.1–3.4 address source/executable distribution and retaining notices; combining it with other files permits a larger work under other terms while preserving obligations for covered code. Preserve the shipped license and source availability information; modifications need matching source availability. The packaged third-party notice file includes MIT and ISC components. [Upstream MPL text](https://github.com/dequelabs/axe-core/blob/v4.14.1/LICENSE), [third-party notices](https://github.com/dequelabs/axe-core/blob/v4.14.1/LICENSE-3RD-PARTY.txt).

## API choices

The pinned API documents all five tags: `wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa`, `wcag22aa`. Select their union explicitly; `wcag22aa` alone does not select earlier levels. Use `axe.run(document, options)` and await its Promise. Recommended options:

```js
{
  runOnly: {
    type: 'tag',
    values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']
  },
  reporter: 'v2',
  resultTypes: ['violations', 'incomplete'],
  iframes: false,
  preload: false,
  selectors: true,
  elementRef: false
}
```

`resultTypes` retains full nodes for selected outcomes; omitted outcomes still have at most one node/rule. `preload` otherwise defaults true and can request cross-origin CSS. Frame waits default to 60 seconds and are not total-audit deadlines. [Pinned API: tags, options and results](https://github.com/dequelabs/axe-core/blob/v4.14.1/doc/API.md).

Important source-level detail: tag matching can select a rule even when its default `enabled` is false. `target-size` is tagged `wcag22aa` but disabled by default; matching tags bypass the final default-enabled decision. Thus the above deliberately selects matching tagged rules rather than promising only default-enabled rules. Persist actual rule IDs and policy version; if default-disabled rules should remain disabled, build explicit `rules[id].enabled=false` overrides from the pinned rule inventory. Do not silently filter all disabled rules and then imply complete WCAG 2.2 coverage. [Selection source](https://github.com/dequelabs/axe-core/blob/v4.14.1/lib/core/utils/rule-should-run.js), [target-size definition](https://github.com/dequelabs/axe-core/blob/v4.14.1/lib/rules/target-size.json).

## Outcomes and evidence

Preserve four distinct outcomes: `violations` are failures, `incomplete` needs manual review, `inapplicable` means no matching content, and `passes` covers tested elements that passed. A thrown/rejected audit is separate from all four. Preserve nested target arrays for frame/shadow paths. [Pinned result contract](https://github.com/dequelabs/axe-core/blob/v4.14.1/doc/API.md#results-object).

Recommended retained evidence: engine version, selected tags, policy, timestamp, page/final URL, browser version and viewport; per rule, ID/help/description/impact/tags/help URL; for violations/review items, target path, bounded HTML snippet and failure summary. Retain rule counts for all outcomes but do not report passed-element totals from reduced `passes` nodes. Keep engine outcome separate from local storage truncation and page execution state.

Render snippets as escaped text. Treat help URLs as external links and keep findings searchable by rule/impact/status. Label a successful zero-violation result “No automated violations found”; never “accessible,” “WCAG compliant,” or a percentage score. Axe itself explains that automation covers only part of WCAG and that incomplete results need manual review. Hidden/inactive UI requires exposing it and rerunning; do not interact with menus, consent dialogs or forms automatically in v1. [Upstream scope and manual review](https://github.com/dequelabs/axe-core/blob/v4.14.1/README.md#the-accessibility-rules).

## Frames, shadow DOM and injection

Axe requires its script in each audited frame. Its API supports open shadow DOM paths and recommends `runPartial`/`finishRun` for browser-driver frame orchestration. Recommend v1 audit only the main document and its open shadow roots, with `iframes:false`; display “Frame contents excluded” as a permanent scope statement, including same-origin frames. Closed shadow roots are outside the promised scope. [Pinned API frame/shadow contract](https://github.com/dequelabs/axe-core/blob/v4.14.1/doc/API.md#api-name-axerunpartial--axefinishrun).

Inject the bundled bytes using CDP into a named isolated execution world for the main frame, then invoke that world's bundled instance. Never reuse a website's `window.axe`, insert a remote script, call a website-provided wrapper, or expose a binding/native bridge. Keep universal cross-origin access disabled. CDP supplies a frame-specific execution-context ID and supports evaluation in that context with `awaitPromise` and `returnByValue`. This is an integration recommendation requiring browser fixtures, especially CSP pages; protocol capabilities alone do not prove axe compatibility in every isolated-world scenario. [Page protocol](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/domains/Page.pdl), [Runtime protocol](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/js_protocol.pdl).

## Deadlines and bounded v1

The public run implementation exposes no AbortSignal/cancel method and prevents simultaneous runs on one instance. Do not use `axe.cleanup` or `teardown` as cancellation. A Rust future timeout or JavaScript `Promise.race` stops waiting without establishing that the audit stopped. On Stop/deadline/navigation/context loss, invalidate the result and dispose the owned page/browser context with bounded cleanup; retain existing process-kill escalation for a wedged browser. CDP's experimental evaluation timeout/`terminateExecution` can supplement this but should not replace host cancellation. [Run implementation](https://github.com/dequelabs/axe-core/blob/v4.14.1/lib/core/public/run.js), [Runtime termination contract](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/js_protocol.pdl).

Proposed initial caps to benchmark: one audit at a time, 10-second audit budget inside the existing page lifecycle, 20,000 traversed main/open-shadow elements before running, 200 detailed nodes/rule and 2,000/page, 2 KiB/snippet, 2 MiB serialized result. Enforce projection and byte bounds inside the isolated world before transport; never transfer unrestricted results and only truncate afterward. These retention caps do not limit axe's internal allocations; the preflight/deadline/disposal remain necessary. Report omitted counts and partial details explicitly. Preserve successful crawl/render evidence if auditing fails.

Use independent states: disabled, unavailable outside Browser mode, queued, running, completed, timed out, cancelled, failed, and skipped by limit. Browser render failure is unavailable evidence, not zero violations. Store only bounded outputs; do not conflate accessibility outcomes with indexability or general SEO severity.
