# Export selection/section — IMPLEMENTED (uncommitted, not visually verified)

## Task
Export only part of the timeline: Shift+drag range on the ruler, between two markers, or typed times. UI first, plus agent/MCP.

## Decisions
- The export is an extract of the final edit. The full edit is resolved first and only the frame range is cut out, so cross-fades, fades and speed changes at the edges match the full export frame for frame.
- Do not use the selected-clips span as a range.

## Done (all in the working tree on main, after pulling origin/main 85732e5 with --autostash; nothing committed)
- **puppetcat-export/src/lib.rs**
  - `ExportRange{start,end}`, `ExportRequest.range: Option<ExportRange>` (serde default None).
  - `frame_span()` snaps to the frame grid, clamps, and errors on an empty range.
  - `render()` resolves transitions on the full list, then renders frames `first_frame..end_frame`.
  - Decoders opened mid-clip start at the clip's in-point and discard the frames before the range, so frame-counted fade filters stay right (`BuiltTimeline.origins`).
  - The audio mix is built to the range end, then the head is cut.
  - Tests: `frame_span_*`, `an_export_range_renders_only_that_stretch_of_the_edit` (colour-coded frames), `an_export_range_cuts_the_sound_to_the_same_stretch` (needs the ffmpeg CLI).
- **puppetcat-media/src/audio.rs**: `mix_graph_from` / `mix_to_file_from(skip)` append `atrim=start=skip,asetpts=PTS-STARTPTS`; skip 0 is the unchanged graph. Test added.
- **puppetcat-host/src/export.rs**: `ExportSpec.range: Option<(f64,f64)>`, copied into the request.
- **puppetcat-agent**
  - `start_export` takes `ExportRangeArgs` (engine.rs).
  - MCP `StartExportParams` gains `start`, `end`, `fromMarker`, `toMarker` (marker id or name; a marker or seconds for each end; an open end runs to the edit's end).
  - Test: `an_export_range_resolves_markers_and_seconds`.
- **UI (puppetcat crate)**
  - lanes.slint: Shift+drag on the ruler sets the range (`range-dragged`); Shift+click without a drag clears it (`range-cleared`). A translucent band is drawn over the lanes with a bar on the ruler. Plumbed through timeline-pane, seat and the Editor global (`range-start`/`range-end`).
  - studio.rs: `Studio.range`, `ExportState` range fields, `ExportRangeMode`, `parse_time`, `clock`, `export_range()`, `export_markers()`, `export_seconds()` (size and duration reflect the range). Timeline right-click menu gains "Export selected range…" and "Clear range selection" when a range exists.
  - The export dialog has a new "Range" section: Whole timeline / Selected range / Between markers (two selects, time-sorted) / Custom times. A note shows the span or the reason it can't be exported, and Export is disabled when invalid.
  - main.rs handlers added. The range is cleared when a project is loaded or closed.
  - Test: `typed_times_parse_as_clock_or_seconds`.

## Verification
`cargo test -p puppetcat-export -p puppetcat-media -p puppetcat-agent -p puppetcat-host -p puppetcat` passes. `cargo check` is clean and clippy shows no warnings on the touched crates. The GUI has NOT been launched, so the Shift+drag and the Range section have not been looked at.

## Environment notes
- The `/Volumes/IntSSD` volume was 100% full. I deleted `engine/target/debug/incremental` (50 GB of regenerable cache). Use `CARGO_INCREMENTAL=0` or free space before big builds.
- The stale ffmpeg-sys build script pointed at Homebrew ffmpeg 9.0.1_1. I fixed it with `cargo clean -p ffmpeg-sys-the-third`.
- Pre-existing uncommitted changes remain from before this task (audio.rs mix hardening, studio.rs peaks/thumbs clearing, Cargo version bump, .aicc files). They are mixed into the same files.

## Next steps
1. Launch the app and check the Shift+drag band, the right-click menu, the Range section in the export dialog, and a real export of a range.
2. Possible follow-ups: handle (drag) the range edges after creating it; the range band on the minimap; keyboard shortcut to clear it.
3. Commit when the user asks. Separate the pre-existing changes from this feature.
