# Final review and v0.7.0 integration

November 19, 2026. User authorized reviewing the final corrections, integrating the shipped v0.6.1 hotfix, validating and releasing the three desktop features. Root owns integration/release. calmtoad-eng performs the Standards pass; a separate reviewer checked Spec and implemented three narrowly scoped residual network fixes, independently replayed by root.

## Spec

The five round-two network/coverage findings are corrected: oversize robots policy remains unknown/blocked, Retry-After delays later origin requests, shared redirects reuse observations, final-check cancellation stops the run, and source/rendered evidence shares the per-page cap.

The final pass found three additional defects and corrected them before integration:

- Parse HTTP-date Retry-After as well as seconds, using httpdate 1.0.3. Future dates create origin cooldowns; past dates do not invent a delay.
- Use checked monotonic-clock arithmetic for unrepresentable server delays. Such a cooldown produces an explicit Limited result without requesting the destination, rather than panicking. A cooldown beyond the per-check deadline does not extend the deadline.
- Cache intermediate redirect destinations with their own remaining chain suffix, so later sources reuse the response while retaining the correct source-specific chain.

Root also used checked addition for extreme robots Crawl-delay values, preserving the same deadline-limited/no-request policy across platform clock ranges.

The Spec reviewer ran 12 maintained integration regressions and two targeted Retry-After unit tests, all passing, and reported no remaining concrete Spec blocker in the reviewed scope.

## Standards

The final Standards pass removed the remaining synchronous inventory scan/sort and full audit/check-map copies from the UI trigger. calmtoad-eng implemented immutable Arc leases, bounded 512-page preparation steps, and worker-side audit/check-context copies and sorting. UI crawl-event consumption pauses during preparation so saved facts cannot mix generations; leases are dropped before events resume, avoiding a UI-thread copy-on-write clone. A new regression verifies queued events wait and resume without loss. The captured Internal query/filter/sort stays frozen. Focused inventory checks passed (7 tests, one ignored measurement); preview checks passed (4 tests).

The actual 50k-page trigger measurement used 250k retained links, 100k extracted values and 1k optional checks. Across the observed runs the trigger stayed below 0.15 ms for All and 0.04 ms for a 1k-row Visible subset; maximum preparation steps stayed below 1.6 ms. The explicit measurement ran one test and passed. Both original Standards issues are closed in the reviewed scope, including writer lifecycle ownership.

Root removed the CSV writer's manual delete-then-persist fallback. Pinned tempfile 3.27 already calls MoveFileExW with MOVEFILE_REPLACE_EXISTING on Windows; a failed replacement now preserves the existing destination. The existing writer regression exercises replacement, cancellation, temporary cleanup and install gating on each CI platform.

## Independent root reproductions

The original five reproductions all passed against the final corrections. The three second-round loopback probes all passed (oversize robots, Retry-After: 7, shared terminal redirect), and the stronger final-optional-check Stop assertion passed.

The older stop probe initially failed to compile after evidence moved to Arc-backed storage; root changed only its iteration spelling from `&page.link_evidence` to `page.link_evidence.iter()`, preserving the original behavioral assertion, then reran it successfully. All temporary repository test files were removed in finally blocks; source copies remain under /private/tmp for audit history.

## Integration

A local safety checkpoint (`b89b733`) preserved all three features and review artifacts. Released v0.6.1 (`ff1a03e`, including PR #1) merged cleanly at `cbc69c9`. This retains the corrected versioned user agent, Ubuntu 22.04 Linux build, glibc ceiling check and distro smoke checks. No unfinished feature work was included in v0.6.1.

v0.7.0 has a November 19 release heading and the three user-facing additions. Release-note extraction and all five release-note unit tests passed.

## Local performance

`scripts/perf-gate.sh --json` passed against the existing committed baseline, without modifying the baseline or adding a CI gate:

- Typical parse: +9.7% calibrated.
- Large parse: +5.4%.
- Audit: +0.9%; incremental audit: +4.2%; counts: -3.4%.
- Synthetic whole-crawl cost is informational (about 19.7 microseconds/page versus 10.1 previously) because the crawler now retains link evidence; it is not a gated scenario. Do not describe this result as no additional crawl work.

The clean v0.6.1 merge exposed a user-agent integration fixture that constructed the old CrawlOptions shape. Root added default values for the new optional features, preserving that fixture’s original behavior. Integrated test results and exact-commit CI/release verification follow once complete.

## Integrated candidate validation

- `cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings`: passed on v0.7.0 after the hotfix fixture adaptation.
- Library: 121 passed, one ignored measurement. UI binary: 83 passed, two ignored measurements.
- Non-browser integration: extraction 1 passed, link evidence 2 passed, maintained review regressions 12 passed, versioned user-agent 1 passed.
- With `CALMTOAD_REQUIRE_BROWSER=1`, the full local browser run completed with 29 passes and three Chrome startup endpoint timeouts. The failures occurred before capture assertions. Root reran only those three exact tests with the same requirement and timeouts; all three passed (rendered extraction, hung-DOM bound, prompt cancellation/process cleanup). Thus every browser case was exercised successfully, but this is not described as a clean full local run. Exact-commit platform CI must pass before tagging.
- Release-notes extraction and its five unit tests passed; diff whitespace checks passed. The committed performance baseline is unchanged.

Standards and Spec: no remaining concrete blocker in the reviewed and corrected scope. The documented local Chrome startup timing flake remains a validation risk; no skip, larger timeout or weakened assertion was introduced to mask it. Required CI includes real-browser tests and the packaged Linux smoke matrix. Publishing is conditional on those jobs passing for the exact release commit.
