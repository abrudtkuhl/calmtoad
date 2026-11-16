# Desktop workflow final polish

November 15, 2026. Local changes based on `9031fe8`; no release or deployment.

## Corrections

- Completed-run sitemap coverage, saved settings and terminal reason are independent of next-run controls and validation notices. Sitemap Files uses file units only while that view is active.
- Skipped sitemap files remain visible in Findings, with counts, CSV and visible-URL copying sharing the same selection.
- Inspector relation lists mount one 25-row window. Selection changes reset their paging; page bounds clamp when result sizes shrink. Full evidence remains accessible through navigation and Copy all.
- Relation lookup and list construction are cached against retained-data revision and selection, with streaming and replacement invalidation. Full clipboard text is built on activation. Collapsed evidence does not construct hidden assessment details.
- Browser termination is serialized through a shared owned child handle across normal shutdown, startup failure and emergency cleanup. Already-collected children are not signalled; stale registry removal cannot remove a newer owner.
- Real-browser process assertions use Unix process inspection or Windows CIM, select the owned root PID, and fail if inspection fails. Cancellation allows a bounded window for process exit instead of silently skipping cleanup verification.

## Review

Source/spec and standards reviews were performed by local review agents. The original tagged calmtoad-eng2 task remains waiting on approval from an earlier turn; these reviews do not claim its signoff. Prior review documents retain their historical reproductions.

## Verification

- macOS: 96 library tests and 52 UI tests pass; one synthetic measurement is intentionally ignored by the ordinary suite.
- Linux: cached Debian/Rust container passes 96 library and 52 UI tests; the same synthetic measurement is ignored.
- Windows: all-target cross-compilation passes using the installed rustup Windows target.
- Formatting, all-target Clippy with warnings denied, and whitespace checks pass.
- macOS real-browser integration: all 25 tests pass with `CALMTOAD_REQUIRE_BROWSER=1`, including cancellation, startup failure, app-close cleanup, launch switches and isolated local fixture capture. The suite completed in 231 seconds.
- The spec follow-up confirms dataset resets clear pagination even when the selected URL stays the same. No findings remain in that scoped review.

Native Windows execution and OS menu shortcut delivery require their native environments; cross-compilation does not establish runtime behavior.

## Large-crawl measurement

Offline synthetic fixtures; x86_64 macOS 15.8.1, Rust 1.99.1 Homebrew, release test binary. Final run took 3.83 seconds after all other validation tasks stopped. These are UI/runtime measurements, not parser throughput or a live-site crawl. Single representative samples establish the observed improvement, not latency guarantees.

| Inspector | Retained URLs | Before first open | Final first open | Final next window | Final repeated frame |
| --- | ---: | ---: | ---: | ---: | ---: |
| Duplicate peers | 10,000 | 23.33 ms | 17.90 ms | 5.36 ms | 3.66 ms |
| Incoming sources | 10,000 | 23.57 ms | 17.10 ms | 6.52 ms | 4.59 ms |
| Duplicate peers | 50,000 | 59.19 ms | 21.43 ms | 7.46 ms | 6.13 ms |
| Incoming sources | 50,000 | 168.00 ms | 57.87 ms | 9.89 ms | 7.83 ms |
| Outgoing targets | 50,000 | Not measured | 45.78 ms | 10.34 ms | Not measured |

The outgoing fixture retains 50,000 targets; no before/after speedup is claimed for it. The earlier samples predate the relation cache, lazy evidence, and borrowed row IDs; final samples include those changes.

At 50,000 rows, selection synchronization measured 3.38 microseconds versus 5.72 milliseconds before. Separate attribution probes measured a URL-index rebuild at 5.72 ms, raw audit incoming scan at 6.55 ms, and complete incoming relation rebuild with a warm URL index at 12.33 ms. These independently timed probes are not additive phase profiling. Initial framework/layout work remains: the first runtime frame measured 34.96 ms. First-time relation construction still scans retained data, while cached repaint/paging avoids that scan and full clipboard construction. Native/runtime work and first-open cost remain practical performance limits; this pass does not claim every 50,000-row interaction stays under one 16 ms frame.
