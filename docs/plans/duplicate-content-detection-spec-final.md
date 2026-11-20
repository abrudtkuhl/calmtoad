# Duplicate content — final UI/export verification

Reviewed the final three-fix correction in the working tree against `5a61a9266db1d62e02f80e491f4d63841033b28f`, the original brief, third review and `duplicate-content-detection-fixes-round3.md`. No implementation edits or release actions. Root runs the suites; this is independent source verification of the export lifecycle correction.

**No substantive remaining finding in the reviewed UI/export/search scope.**

## Verified correction

The prior inventory/duplicate-export overlap is resolved:

- `export_lifecycle_busy` (`src/main.rs:1942`) includes inventory preparation/export, duplicate preparation, dialog and writer state. Both `start_inventory_export` (`5926`) and `start_content_export` (`7225`) enforce it before starting work. `export_csv` (`7336`) and the relevant buttons also gate preparation. Both previously reachable start orders are rejected while the other operation owns the lifecycle.
- Normal completion is not blocked by its own preparation job: both `poll_inventory_export` (`1446`) and `poll_content_export` (`7294`) remove their job from State before handing a prepared result to `save_prepared_csv`. The latter enforces the common gate (`7513`), then marks the background dialog owned before scheduling it. Thus an idle completed export can proceed normally, while a conflicting completion cannot open another background save dialog.
- On destination selection, the callback clears its completed dialog state and checks `export_writer_slot_available` before creating a temp file/assigning `inventory_write` (`7535–7555`). A live writer receiver/cancel handle is no longer unconditionally replaced; conflict returns an error without touching the destination.
- The ordinary successful path still uses the atomic temporary-file writer, retains its cancel handle, and reports completion through its receiver. Dialog cancel/error leaves no writer. Updater and new-crawl predicates continue to include prepare/dialog/write states, and close cancels the tracked jobs.

The new both-start-orders/live-writer regression is relevant to this defect. It tests start rejection, slot unavailability, updater gating and close cancellation. Native callback behavior was additionally checked through the source path; the test itself does not simulate two native dialogs or a slow disk.

## Preserved fixes and limits

The prior frozen/generation-tagged export snapshot, background projection/encoding, member-specific scope, folded-key multi-group search, compact row metadata and cached affected counts remain present. No unrelated scope expansion or standalone standards issue found. Core memory/coverage corrections are independently handled by root/core review. No native GUI/slow-disk overlap run or cross-platform claim is made here; this scoped result is not a substitute for root’s final tests and release checks.
