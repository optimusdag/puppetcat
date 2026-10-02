# Markers attached to clips

**Goal:** markers can attach to a clip and move with it. Right-click clip -> "Add clip marker at playhead"; right-click timeline (empty lanes or ruler) -> "Add timeline marker at playhead".

## Where the work is
Worktree `/Volumes/IntSSD/Projects/video-editor/.claude/worktrees/clip-markers`, branch `worktree-clip-markers` (another agent works in the main checkout). **Changes are uncommitted** in the worktree. Do not use bare git stash.

## Done (builds; `cargo test --workspace` 261 pass)
- Model: `Marker { clip_id: Option<String>, offset: f64 }` (serde default and skip when empty, so old docs load). `time` stays the absolute time, kept in sync for attached markers.
- `commands.rs`: `apply` is now a wrapper over `apply_command` that runs `sync_markers` after every command. Attached marker time = clip.start + offset, clamped to the clip's duration. A marker whose clip is gone is detached where it stood.
- `AddMarker` gained `clip_id`. New `AttachMarker { marker_id, clip_id: Option }` (None detaches). `MoveMarker` on an attached marker slides along the clip.
- TrimClip Start: the marker keeps its place on the footage, or detaches if the head passes it. SplitClips: markers past the cut move to the tail clip. MergeClips: markers move to the survivor with an adjusted offset. RemoveClips: markers detach in place (decision confirmed by sync).
- `doc.rs` reads `clipId`/`offset`.
- MCP: `add_marker` takes `clipId`; new `attach_marker` tool; clipId appears in state via serde.
- UI (Slint + studio.rs/main.rs): clip menu row "Add clip marker at playhead" (enabled when the playhead is inside the clip); marker menu "Detach from clip" for attached markers; a new third `timeline-menu` ContextMenu (token pattern like the marker menu) on right-click of empty lanes and the ruler, with "Add timeline marker at playhead". Wired through lanes.slint, timeline-pane.slint, seat.slint and editor.slint.
- Tests in `puppetcat-project/src/lib.rs`: follow on move, round-trip, drag clamp, detach, remove clip, split, head trim.
- Only my own hunks were rustfmt-clean; the repo has pre-existing fmt drift (do not mass-reformat).

## Not done / next steps
- Not run in the real app. Manually check that the right-click menus open at the pointer, on the ruler and in empty lanes, and that the clip menu works.
- No visual indicator for attached markers (planned link glyph in `lanes.slint`'s `Marker` component). `MarkerData` would need an `attached` bool.
- The UI has no gesture for attaching an existing free marker to a clip (only the agent tool `attach_marker`); consider a marker menu row "Attach to clip under playhead".
- Commit when the user asks; then merge/PR from `worktree-clip-markers`.


## Update: clip markers drawn on the clip
- `MarkerData.row` (model.slint; -1 = free) is filled in studio.rs from the attached clip's lane row. In lanes.slint, free markers stay on the ruler with their thread line. Attached markers are drawn as a second `Marker` loop after the clips, at the clip's top edge (`row-y(row)+4px`). The ruler loop and thread hide them.
- `marker_dragged` clamps an attached marker's echo time to its clip's span.
- Builds, 261 tests pass. Still not eyeballed in the running app. The "link" indicator and the UI attach gesture are now moot or optional (the clip placement itself marks attachment).


## Status: shipped
User verified it in the app ("works perfect"), committed on `worktree-clip-markers` and pushed it to `origin/main` as a fast-forward (`git push origin worktree-clip-markers:main`). A local merge was blocked by the other agent's uncommitted `studio.rs` in the main checkout. TODO for whoever is in the main checkout: `git pull` once that work is committed (expect conflicts only in `studio.rs`). Then the worktree `.claude/worktrees/clip-markers` and branch `worktree-clip-markers` can be deleted. Optional follow-ups: a UI gesture to attach an existing free marker to a clip.
