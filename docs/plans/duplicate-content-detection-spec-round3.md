# Duplicate content — spec review, round 3

Reviewed the second correction pass against `5a61a9266db1d62e02f80e491f4d63841033b28f`, the original brief, `duplicate-content-detection-review-round2.md`, and `duplicate-content-detection-fixes-round2.md`. Focus: UI/export/search and lifecycle. No feature edits, commits or releases; root owns Cargo validation. This report is source-traced, not a cross-platform GUI walkthrough.

## Resolved in this scope

- **Round-two finding 3, underestimated export fields:** preflight now accounts for full row schemas, context repeated per output row, escaping and simultaneous projected/encoded copies (`src/main.rs:7115–7208`). Production projection uses a remaining-byte limit and cancellation checks; encoding stays in the worker. The old `member.url +256`/fixed-pair estimate is gone. Core retained-ledger accuracy remains under root/core review; this is not an allocator/RSS certification.
- **Finding 4, folded member index:** `HashMap<String, Vec<String>>` retains all folded URL/group associations (`src/content.rs:1041–1054`); Unicode lowercasing now agrees with query/row normalization. `matching_member_groups` flattens all matching group IDs, so case-colliding nonrepresentative paths no longer overwrite one another.
- **Finding 5, giant exact-member search text:** `content_row_search` now uses representative/group metadata without concatenating members. Empty queries skip row-search-string construction; member matching uses the prepared index. The original giant-membership copying is removed. Nonempty filtering still recomputes compact row text after caching it; this contradicts the report's narrow claim that the cache is consumed, but I have not established a material new defect from that duplication alone and do not count it as a blocker.
- Previously corrected cached affected counts, actual member scope/stage in CSV, frozen export snapshots and generation rejection remain present.

## Actionable finding

### P2 — Inventory and duplicate export preparation can overlap and lose shared lifecycle ownership

The new duplicate-content export is not mutually gated with the existing inventory exporter:

- `src/main.rs:6773–6778`: the Duplicate Export button ignores `inventory_prepare` and `inventory_export_job`.
- `src/main.rs:7211–7218`: `start_content_export` checks only its own job, so it can start during inventory preparation.
- Reverse direction: Overview’s inventory button (`5624–5628`) and `start_inventory_export` (`5912–5918`) ignore `content_export_job`.

Reproduction path from the call graph: start a large inventory export, switch to Internal → Duplicate content before preparation finishes, then start Duplicate Export. Both preparation jobs coexist. `poll_inventory_export` and `poll_content_export` independently invoke `save_prepared_csv`, which does not guard a pending dialog/write (`7483–7505`). Both callbacks assign the same single `state.inventory_write` slot (`7522`) without checking whether another writer exists. If the first write remains active when the second destination is chosen, its receiver/cancel handle is replaced. Its result/cancellation is lost; after the tracked writer finishes, new-crawl/update-install gates can clear while the untracked writer still runs. Native-dialog serialization would not by itself prevent this: the first writer runs in the background after its dialog closes.

Spec: “Shutdown/update installation must respect or cancel/join owned workers”; “Add dedicated ... atomic cancellable writer”; “never ... overwrite an existing file on failure/cancel.” The per-export atomic writer remains intact, but shared job ownership no longer covers all concurrent workers.

Use one exclusive export lifecycle predicate/state across both preparation paths, dialogs and writers; enforce it in start functions as well as button presentation. Alternatively, explicitly track every job/dialog and gate shutdown/install against all of them. Regression tests should attempt both start orders and deliver overlapping preparation completions; verify no existing writer receiver/cancel handle can be replaced. No slow-disk/native-dialog runtime reproduction was attempted in this review.

## Limits

The expanded export preflight still iterates the chosen row/member scope on UI, but the supplied 50k measurement is below the 8ms target and it inspects lengths rather than copying long field text; I found no new measured responsiveness defect there. Root/core review independently assesses allocations/reservations and coverage. No unrelated scope expansion or standalone standards violation found.
