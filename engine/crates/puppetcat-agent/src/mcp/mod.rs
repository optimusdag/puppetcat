// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The one module that knows MCP exists: tool schemas and the stdio server.
//!
//! `rmcp` is tokio-based; the rest of the engine is deliberately thread-based
//! (see `engine/README.md`'s "Threads, not async" - reasoning scoped to the
//! CPU-bound render path, which this isn't). Scoping tokio to this module
//! alone means that convention elsewhere is untouched. The one call heavy
//! enough to matter, `render_snapshot`, runs on `spawn_blocking` rather than
//! tying up an async worker thread for the length of a render.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::AgentEngine;
use crate::dto::{
    ClipMoveArg, ClipPatchArg, TextStyleArg, TrackFlagArg, TrimEdgeArg, parse_raw_command,
};
use puppetcat_project::Command;
use puppetcat_project::commands::{ClipMove, TrackFlag, TrimEdge};

fn to_mcp_err(message: String) -> McpError {
    McpError::internal_error(message, None)
}

/// Serializes anything `Serialize` into one text content block - every
/// tool here that isn't `render_snapshot` returns state this way, the same
/// `EditorView`/`ProjectInfo` JSON shape the window itself reads.
fn ok_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string(value).map_err(|error| to_mcp_err(error.to_string()))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn ok_state(
    result: Result<puppetcat_host::EditorView, String>,
) -> Result<CallToolResult, McpError> {
    result.map_err(to_mcp_err).and_then(|view| ok_json(&view))
}

// ── tool parameter shapes ───────────────────────────────────────────────

/// Parameters for `create_project`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectParams {
    /// Folder the project's own folder is created inside.
    pub location: String,
    /// The project's name; also the name of the folder created for it.
    pub name: String,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Frame rate numerator - an exact fraction, e.g. 30/1, or 30000/1001
    /// for 29.97.
    pub rate_num: i64,
    /// Frame rate denominator.
    pub rate_den: i64,
}

/// Parameters for `open_project`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenProjectParams {
    /// The project's folder, as returned by `create_project` or
    /// `list_recent_projects`.
    pub path: String,
}

/// Parameters for `import_media`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportMediaParams {
    /// Absolute path to a media file on disk.
    pub path: String,
}

/// Parameters for `add_fade_to_black`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddFadeToBlackParams {
    /// Visual track to bookend. Omit to choose the earliest visible video
    /// track automatically.
    pub track_id: Option<String>,
    /// Seconds of black at each end. Defaults to one second.
    pub duration: Option<f64>,
    /// Seconds of cross-fade at each cut. Defaults to half a second.
    pub transition_duration: Option<f64>,
    /// Preserve sync by shifting every lane. Defaults to true.
    pub shift_all_tracks: Option<bool>,
    /// Move free markers with the content. Attached markers always follow
    /// their clips. Defaults to true.
    pub shift_free_markers: Option<bool>,
}

/// Parameters for `analyze_audio`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeAudioParams {
    /// Analyze this bin item. Omit with `clipId` to analyze the active
    /// timeline's audible mix.
    pub media_id: Option<String>,
    /// Analyze this active-timeline clip. Mutually exclusive with `mediaId`.
    pub clip_id: Option<String>,
    /// Beginning in target coordinates: source seconds for media, timeline
    /// seconds for a clip or timeline.
    pub start: Option<f64>,
    /// Exclusive end in the same coordinates as `start`.
    pub end: Option<f64>,
    /// Analysis resolution in milliseconds, 20..=2000. Defaults to 100.
    pub bucket_ms: Option<u32>,
    /// Buckets at or below this RMS dBFS count as silence. Defaults to -35.
    pub silence_threshold_db: Option<f64>,
    /// Drop silence runs shorter than this many seconds. Defaults to one.
    pub min_silence: Option<f64>,
    /// Include every analysis bucket. False by default to keep results small.
    pub include_buckets: Option<bool>,
}

/// Parameters for `remove_silence`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveSilenceParams {
    /// Only look at this active-timeline clip's stretch of the timeline.
    /// Omit to analyse the whole audible mix.
    pub clip_id: Option<String>,
    /// Beginning of the stretch to clean, in timeline seconds.
    pub start: Option<f64>,
    /// End of the stretch, in timeline seconds.
    pub end: Option<f64>,
    /// Audio at or below this RMS dBFS is silence. Defaults to -35.
    pub silence_threshold_db: Option<f64>,
    /// Only silences at least this many seconds long are cut. Defaults to 0.5.
    pub min_silence: Option<f64>,
    /// Seconds of silence kept at each side of a cut, so speech is not
    /// clipped. Defaults to 0.1.
    pub padding: Option<f64>,
    /// Lanes to cut. Empty or omitted cuts every lane together.
    pub track_ids: Option<Vec<String>>,
    /// Report what would be cut and commit nothing. Default false.
    pub dry_run: Option<bool>,
}

/// Parameters for `detect_scenes`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DetectScenesParams {
    /// Analyse this bin item's video; times come back in source seconds.
    pub media_id: Option<String>,
    /// Analyse the stretch of video this active-timeline clip shows; times
    /// come back in timeline seconds. Mutually exclusive with `mediaId`.
    pub clip_id: Option<String>,
    /// Beginning of the stretch, in the same coordinates as the result.
    pub start: Option<f64>,
    /// End of the stretch.
    pub end: Option<f64>,
    /// Mean brightness change between frames, 0..1, that counts as a cut.
    /// Defaults to 0.15; lower finds softer cuts and more false ones.
    pub threshold: Option<f64>,
    /// Cuts closer than this many seconds to the one before are ignored.
    /// Defaults to 0.5.
    pub min_scene: Option<f64>,
    /// Drop a marker at each cut of a `clipId`, as one undoable edit.
    pub add_markers: Option<bool>,
}

/// Parameters for `list_catalogue`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListCatalogueParams {
    /// One of `effects`, `filters`, `audio` or `transitions`. Omit for all.
    pub kind: Option<String>,
    /// Include each entry's parameters (key, range, default, unit). Default true.
    pub include_params: Option<bool>,
}

/// Parameters for `download_speech_model`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSpeechModelParams {
    /// `transcriber` (whisper, for captions) or `voice` (Kokoro, for speech).
    pub kind: String,
    /// A model id from `speech_status`.
    pub id: String,
}

/// Parameters for `caption_clip`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaptionClipParams {
    /// The active-timeline clip whose sound is transcribed.
    pub clip_id: String,
    /// A whisper language code such as `en`, or `auto` (the default).
    pub language: Option<String>,
    /// A transcriber model id; defaults to the first one downloaded.
    pub model_id: Option<String>,
}

/// Parameters for `generate_voice`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerateVoiceParams {
    /// A title clip whose words are spoken; the voice starts where it starts.
    pub clip_id: Option<String>,
    /// Or the words themselves. Give one of `clipId` and `text`.
    pub text: Option<String>,
    /// Timeline seconds where the voice starts when `text` is given.
    /// Defaults to 0.
    pub start: Option<f64>,
    /// A Kokoro speaker id from `speech_status`. Defaults to `af_heart`.
    pub voice: Option<i32>,
    /// Speaking rate; 1 is natural. Defaults to 1.
    pub speed: Option<f32>,
    /// A voice model id; defaults to the first one downloaded.
    pub model_id: Option<String>,
}

/// Filters and inclusion controls for a compact timeline summary.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimelineSummaryParams {
    /// Timeline id; omit for the active timeline.
    pub timeline_id: Option<String>,
    /// Include matching clips, sorted by start. Defaults to false.
    pub include_clips: Option<bool>,
    /// Include markers. Defaults to false.
    pub include_markers: Option<bool>,
    /// Restrict clips and track summaries to this track.
    pub track_id: Option<String>,
    /// Restrict clips to those intersecting this time or later.
    pub start: Option<f64>,
    /// Restrict clips to those starting before this time.
    pub end: Option<f64>,
    /// Maximum clips and markers returned, 1..=1000. Defaults to 200.
    pub limit: Option<usize>,
}

/// Selects one clip for structured lookup.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetClipParams {
    /// Clip identifier.
    pub clip_id: String,
    /// Search only this timeline; omit to search the whole project.
    pub timeline_id: Option<String>,
}

/// Selects one media-bin item for structured lookup.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetMediaParams {
    /// Media identifier.
    pub media_id: String,
}

/// Scope for project invariant validation.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidateTimelineParams {
    /// Validate only this timeline; omit to validate the whole project.
    pub timeline_id: Option<String>,
}

/// Parameters for `add_clip`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddClipParams {
    /// The bin item to cut from - a media id from `get_state`'s `media`
    /// list, or the id `import_media` just created.
    pub media_id: String,
    /// The lane to place it on - a track id from `get_state`.
    pub track_id: String,
    /// Timeline position in seconds, floored at 0.
    pub start: f64,
}

/// Parameters for `add_clip_at_first_free`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddClipAtFirstFreeParams {
    /// The bin item to cut from.
    pub media_id: String,
    /// Timeline position in seconds, floored at 0.
    pub start: f64,
}

/// Parameters for `add_text_clip`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddTextClipParams {
    /// The lane to place it on. Omit to pick the first free track.
    pub track_id: Option<String>,
    /// Timeline position in seconds, floored at 0.
    pub start: f64,
    /// The words and their look. Omit for plain default styling.
    pub style: Option<TextStyleArg>,
    /// Seconds on the timeline. Omit for the editorial default (four
    /// seconds).
    pub duration: Option<f64>,
    /// Vertical placement as a frame-height fraction. Lower thirds are made
    /// of this.
    pub offset_y: Option<f64>,
}

/// Parameters for `move_clips`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveClipsParams {
    /// Where each clip is going. Unknown clips or tracks are tolerated -
    /// see each field's own doc on `ClipMoveArg`.
    pub moves: Vec<ClipMoveArg>,
}

/// Parameters for `trim_clip`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrimClipParams {
    /// The clip to trim.
    pub clip_id: String,
    /// Which edge is being dragged.
    pub edge: TrimEdgeArg,
    /// Signed seconds the edge moves: positive drags the head right
    /// (shortening) or the tail right (lengthening).
    pub delta: f64,
}

/// Parameters for `split_clips`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SplitClipsParams {
    /// The clips under the cut point - normally the current selection.
    pub clip_ids: Vec<String>,
    /// The cut point, in timeline seconds.
    pub time: f64,
}

/// Parameters for `merge_clips`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MergeClipsParams {
    /// The pieces to rejoin, any order. Must sit on one track, come from one
    /// file at one speed, touch within a microsecond, and stay in source
    /// order, or the tool returns the exact reason they cannot merge.
    pub clip_ids: Vec<String>,
}

/// Parameters for `remove_clips`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveClipsParams {
    /// The clips to delete. Unknown ids are ignored.
    pub clip_ids: Vec<String>,
}

/// Parameters for `update_clip`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClipParams {
    /// The clip to patch.
    pub clip_id: String,
    /// Only the fields present in `patch` change.
    pub patch: ClipPatchArg,
}

/// Parameters for `set_clip_speed`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetClipSpeedParams {
    /// The clip to retime.
    pub clip_id: String,
    /// Clamped into 0.0625..=16.
    pub speed: f64,
}

/// Parameters for `set_clip_transform`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetClipTransformParams {
    /// The clip to place.
    pub clip_id: String,
    /// New scale, clamped into 0.05..=8. Omit to leave it.
    pub scale: Option<f64>,
    /// New horizontal offset as a frame-width fraction, clamped to +/-3.
    pub offset_x: Option<f64>,
    /// New vertical offset as a frame-height fraction, clamped to +/-3.
    pub offset_y: Option<f64>,
    /// New rotation in degrees.
    pub rotation: Option<f64>,
}

/// Parameters for `set_track_flag`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetTrackFlagParams {
    /// The lane to change.
    pub track_id: String,
    /// Which toggle: visibility or mute.
    pub flag: TrackFlagArg,
    /// The new setting.
    pub value: bool,
}

/// Parameters for `add_marker`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddMarkerParams {
    /// Seconds from the start of the active timeline, floored at 0.
    pub time: f64,
    /// Label. Omit for "Marker N" after the highest already in use.
    pub name: Option<String>,
    /// Attach the marker to this clip so it moves with it. Omit for a
    /// free-floating marker.
    pub clip_id: Option<String>,
}

/// Parameters for `attach_marker`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachMarkerParams {
    /// The marker to attach or detach.
    pub marker_id: String,
    /// The clip to attach to. Omit to detach the marker.
    pub clip_id: Option<String>,
}

/// Parameters for `rename_marker`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenameMarkerParams {
    /// The marker to rename - an id from `get_state`'s active timeline, or
    /// the id `add_marker` just created.
    pub marker_id: String,
    /// The new label; trimmed before it lands. Whitespace-only is ignored.
    pub name: String,
}

/// Parameters for `move_marker`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveMarkerParams {
    /// The marker to move.
    pub marker_id: String,
    /// New time in seconds, floored at 0.
    pub time: f64,
}

/// Parameters for `remove_marker`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveMarkerParams {
    /// The marker to delete. An unknown id is ignored.
    pub marker_id: String,
}

/// Parameters for `set_export_directory`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetExportDirectoryParams {
    /// Absolute folder path for exports.
    pub directory: String,
    /// Create the folder (and parents) if missing. Default false.
    pub create: Option<bool>,
}

/// Parameters for `rename_project`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RenameProjectParams {
    /// The new display name.
    pub name: String,
}

/// Parameters for `move_project`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MoveProjectParams {
    /// Exact new absolute path of the project folder. Must not exist; its
    /// parent must.
    pub destination: String,
}

/// Parameters for `relink_media`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RelinkMediaParams {
    /// Explicit item-to-file replacements.
    pub entries: Option<Vec<crate::engine::RelinkEntry>>,
    /// Bulk mode: every media path under this folder is rewritten to the same
    /// relative path under `newRoot`.
    pub old_root: Option<String>,
    /// The folder that replaces `oldRoot`.
    pub new_root: Option<String>,
    /// Probe and report without changing anything. Default false.
    pub dry_run: Option<bool>,
}

/// Parameters for `move_output`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveOutputParams {
    /// The rendered file to move.
    pub source: String,
    /// Exact destination file path.
    pub destination: String,
    /// Replace an existing destination. Default false.
    pub overwrite: Option<bool>,
    /// Compare bytes before deleting the source. Default true.
    pub verify: Option<bool>,
}

/// Parameters for `start_batch_export`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartBatchExportParams {
    /// Stretches to export, each to its own file. Omit with `pairByName`.
    pub ranges: Option<Vec<crate::engine::BatchRangeArg>>,
    /// Derive ranges from markers named "<label> start" / "<label> end"
    /// (or "in" / "out").
    pub pair_by_name: Option<bool>,
    /// Filename without extension. Fields: {name}, {index} (two digits),
    /// {project}. Default "{name}".
    pub filename_template: Option<String>,
    /// Absolute folder for the files; created if missing.
    pub output_dir: String,
    /// x264 CRF, default 18.
    pub crf: Option<u8>,
    /// x264 preset, default "medium".
    pub preset: Option<String>,
    /// What to do when a file exists: "fail" (default), "skip", "overwrite"
    /// or "suffix". Repeat a failed batch with "skip" to resume it.
    pub collision: Option<String>,
    /// Seconds of handle before each start.
    pub head_seconds: Option<f64>,
    /// Seconds of handle after each end.
    pub tail_seconds: Option<f64>,
    /// Return the preflighted plan without rendering.
    pub dry_run: Option<bool>,
}

/// Parameters for `save_layout_template`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SaveLayoutParams {
    /// Layout name; saving an existing name replaces it.
    pub name: String,
    /// Copy the starting values from this clip on the active timeline.
    pub from_clip_id: Option<String>,
    /// Values to set (override the source clip's).
    pub values: Option<crate::engine::LayoutValues>,
}

/// Parameters for `remove_layout_template`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoveLayoutParams {
    /// Layout name.
    pub name: String,
}

/// Parameters for `apply_layout_template`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyLayoutParams {
    /// Layout name.
    pub name: String,
    /// Clips to restyle.
    pub clip_ids: Option<Vec<String>>,
    /// Or every visual clip on this track...
    pub track_id: Option<String>,
    /// ...ending after this time (seconds)...
    pub from: Option<f64>,
    /// ...and starting before this one.
    pub to: Option<f64>,
    /// Report which clips would change without changing them.
    pub dry_run: Option<bool>,
}

/// Parameters for layout export/import.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutFileParams {
    /// Absolute path of the JSON file.
    pub path: String,
    /// Replace an existing file (export) or same-named layouts (import).
    pub overwrite: Option<bool>,
}

/// Parameters for `apply_edit_plan`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyEditPlanParams {
    /// The `revision` from the latest state response. A mismatch rejects the
    /// plan before anything is touched.
    pub expected_revision: u64,
    /// Commands in order, each shaped like `apply_raw_command`'s `command`.
    /// An operation may carry `"ref": "name"`; if it mints an id, later
    /// operations can write `"$name"` anywhere a string id is expected.
    pub operations: Vec<serde_json::Value>,
    /// Stage and report without committing. Default false.
    pub dry_run: Option<bool>,
    /// Reject plans that introduce validation errors. Default true.
    pub validate: Option<bool>,
}

/// Parameters for `apply_raw_command`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRawCommandParams {
    /// One `puppetcat_project::Command`, exactly as the window's own edits are
    /// shaped on the wire: a JSON object tagged by an `op` field, e.g.
    /// `{"op": "addClip", "mediaId": "m1", "trackId": "t1", "start": 2.0}`.
    /// Covers every operation, including the ones with no dedicated tool:
    /// `removeMedia`, `setMediaPlaceholder`, `fillSlot`, `setClipSpeedCurve`,
    /// `setClipAnimation`, `detachAudio`, `reattachAudio`, `addLayerClip`,
    /// `removeTrack`, `renameTrack`, `addTimeline`, `removeTimeline`,
    /// `renameTimeline`, `selectTimeline`, `moveTimeline`, `addFont`,
    /// `removeFont`, and `batch` (`{"op": "batch", "commands": [...]}`, any
    /// of the above nested, applied atomically as one undo step).
    pub command: serde_json::Value,
}

/// Parameters for `render_snapshot`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenderSnapshotParams {
    /// The timeline instant to render, in seconds.
    pub time: f64,
    /// Output width in pixels. Omit to use the project's own width.
    pub width: Option<u32>,
    /// Output height in pixels. Omit to use the project's own height.
    pub height: Option<u32>,
}

/// Parameters for `render_contact_sheet`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenderContactSheetParams {
    /// Timeline instants to render, in seconds, in cell order. Limited to 64.
    pub times: Vec<f64>,
    /// Width of every frame. Omit to use the project width.
    pub width: Option<u32>,
    /// Height of every frame. Omit to use the project height.
    pub height: Option<u32>,
    /// Number of columns. Omit for an approximately square grid.
    pub columns: Option<u32>,
    /// Optional PNG path where the composed sheet is written atomically.
    pub output: Option<String>,
    /// Optional directory receiving every rendered frame as its own PNG.
    pub frames_output_dir: Option<String>,
}

/// Parameters for `start_export`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartExportParams {
    /// The file to write, e.g. "/Users/me/Movies/out.mp4".
    pub output: String,
    /// Constant rate factor: lower is better quality and a bigger file.
    /// Omit for a reasonable default (20).
    pub crf: Option<u8>,
    /// The x264 speed/size preset name, e.g. "medium". Omit for "medium".
    pub preset: Option<String>,
    /// Export only part of the timeline, beginning here, in seconds. The
    /// result is an extract of the finished edit - transitions and fades at
    /// the edges look as they do in the full export. Omit with `end`,
    /// `fromMarker` and `toMarker` for the whole timeline.
    pub start: Option<f64>,
    /// Where the extract ends, in seconds. Omit to run to the end.
    pub end: Option<f64>,
    /// Begin at this marker of the active timeline (its id or its name)
    /// instead of at `start`.
    pub from_marker: Option<String>,
    /// End at this marker of the active timeline (its id or its name)
    /// instead of at `end`.
    pub to_marker: Option<String>,
}

// ── the server ──────────────────────────────────────────────────────────

/// The MCP server: one [`AgentEngine`] behind the tool router `rmcp`
/// generates from the methods below.
#[derive(Clone)]
pub struct ConcatAgentServer {
    engine: Arc<AgentEngine>,
    // Read by the `#[tool_handler]`-generated `ServerHandler` methods below,
    // not by anything in this file - the dead_code pass doesn't see across
    // the two macro-expanded impl blocks, hence the `allow`.
    #[allow(dead_code)]
    tool_router: ToolRouter<ConcatAgentServer>,
}

#[tool_router]
impl ConcatAgentServer {
    /// Wraps `engine` with the tool router `rmcp` generates from the
    /// `#[tool]` methods below.
    pub fn new(engine: Arc<AgentEngine>) -> Self {
        Self {
            engine,
            tool_router: Self::tool_router(),
        }
    }

    // ── introspection ──

    #[tool(
        description = "What this server supports: version, schema version, every tool name, feature flags and limits. Call it once to check compatibility instead of probing for tools."
    )]
    fn get_capabilities(&self) -> Result<CallToolResult, McpError> {
        let mut tools: Vec<String> = self
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        tools.sort();
        ok_json(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "schemaVersion": 1,
            "tools": tools,
            "features": {
                "revisionTokens": true,
                "editPlans": true,
                "editPlanClientRefs": true,
                "editPlanDryRun": true,
                "editPlanDiff": true,
                "rippleDelete": true,
                "rippleTrim": true,
                "rippleInsert": true,
                "insertGap": true,
                "rippleDeleteRange": true,
                "removeSilence": true,
                "detectScenes": true,
                "listCatalogue": true,
                "speech": true,
                "batchExport": true,
                "layoutTemplates": true,
                "mediaRelink": true,
            },
            "limits": {
                "contactSheetFrames": crate::snapshot::MAX_CONTACT_SHEET_FRAMES,
            },
        }))
    }

    #[tool(
        description = "Run diagnostics: config and scratch directories writable, and whether a project is open. Each check has a stable `code` and, on failure, a `remediation`."
    )]
    fn health_check(&self) -> Result<CallToolResult, McpError> {
        ok_json(&self.engine.health_check())
    }

    // ── project lifecycle ──

    #[tool(
        description = "Create a new project folder and open it as the active session. Opening a project here blocks the PuppetCat window on it, if one has it open, until close_project is called - call close_project as soon as the current change or set of changes is done."
    )]
    fn create_project(
        &self,
        Parameters(params): Parameters<CreateProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .create_project(
                &params.location,
                &params.name,
                params.width,
                params.height,
                params.rate_num,
                params.rate_den,
            )
            .map_err(to_mcp_err)
            .and_then(|info| ok_json(&info))
    }

    #[tool(
        description = "Open an existing project folder as the active session, replacing whatever was open. Unsaved edits in the previous session are lost - call save_project first if they matter. Opening a project here blocks the PuppetCat window on it, if one has it open, until close_project is called - call close_project as soon as the current change or set of changes is done."
    )]
    fn open_project(
        &self,
        Parameters(params): Parameters<OpenProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .open_project(&params.path)
            .map_err(to_mcp_err)
            .and_then(|info| ok_json(&info))
    }

    #[tool(
        description = "Save the active session, then close it - releasing the PuppetCat window (if it has this project open) to resume. Call this as soon as the current change or set of changes is done, not only once the whole task is finished; reopen the project later to continue."
    )]
    fn close_project(&self) -> Result<CallToolResult, McpError> {
        self.engine.close_project().map_err(to_mcp_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text("closed")]))
    }

    #[tool(
        description = "Save the active session in place, without closing it. This does not release the project - the PuppetCat window, if it has this project open, stays blocked until close_project is called."
    )]
    fn save_project(&self) -> Result<CallToolResult, McpError> {
        self.engine.save_project().map_err(to_mcp_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text("saved")]))
    }

    #[tool(description = "List every project this machine has opened before, most recent first.")]
    fn list_recent_projects(&self) -> Result<CallToolResult, McpError> {
        ok_json(&self.engine.list_recent_projects())
    }

    // ── query ──

    #[tool(
        description = "The active session's current state: the whole project (timelines, tracks, clips, media), and whether undo/redo are available."
    )]
    fn get_state(&self) -> Result<CallToolResult, McpError> {
        self.engine
            .get_state()
            .map_err(to_mcp_err)
            .and_then(|view| ok_json(&view))
    }

    #[tool(
        description = "Analyze a media item, one clip, or the active timeline's audible mix without external ffmpeg commands. Returns loudness, silence/dead-air intervals, activity, and explicitly heuristic speech/music likelihoods. Omit mediaId and clipId for the composed timeline."
    )]
    async fn analyze_audio(
        &self,
        Parameters(params): Parameters<AnalyzeAudioParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        let result = tokio::task::spawn_blocking(move || {
            engine.analyze_audio(
                params.media_id,
                params.clip_id,
                params.start,
                params.end,
                f64::from(params.bucket_ms.unwrap_or(100).clamp(20, 2000)) / 1000.0,
                params
                    .silence_threshold_db
                    .unwrap_or(-35.0)
                    .clamp(-120.0, 0.0),
                params.min_silence.unwrap_or(1.0).max(0.0),
                params.include_buckets.unwrap_or(false),
            )
        })
        .await
        .map_err(|error| to_mcp_err(format!("audio analysis task panicked: {error}")))?
        .map_err(to_mcp_err)?;
        ok_json(&result)
    }

    #[tool(
        description = "Remove silence from the active timeline in one undoable edit. Analyses the audible mix (or one clip's stretch via `clipId`, or a `start`/`end` range), shrinks each silent run by `padding` so speech keeps room, and ripple-deletes the rest across every lane so picture, sound and linked audio stay in sync; free markers shift. Anything with no sound under it counts as silent, so scope picture-only stretches out with `clipId`, `start`/`end` or `trackIds`. Use `dryRun` first: returns the cuts, total seconds removed and the plan diff. Built on the `rippleDeleteRange` {start, end, trackIds, markerPolicy} edit-plan op, which cuts any span of time."
    )]
    async fn remove_silence(
        &self,
        Parameters(params): Parameters<RemoveSilenceParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        let result = tokio::task::spawn_blocking(move || {
            engine.remove_silence(
                params.clip_id,
                params.start,
                params.end,
                params
                    .silence_threshold_db
                    .unwrap_or(-35.0)
                    .clamp(-120.0, 0.0),
                params.min_silence.unwrap_or(0.5).max(0.05),
                params.padding.unwrap_or(0.1).clamp(0.0, 5.0),
                params.track_ids.unwrap_or_default(),
                params.dry_run.unwrap_or(false),
            )
        })
        .await
        .map_err(|error| to_mcp_err(format!("silence removal task panicked: {error}")))?
        .map_err(to_mcp_err)?;
        ok_json(&result)
    }

    #[tool(
        description = "Detect shot changes in a video, by comparing each frame with the one before. Give a `mediaId` (times in source seconds) or a `clipId` (times in timeline seconds, mapped through the clip's trim and speed). Returns the cuts with a difference `score`. With `addMarkers` and a `clipId`, drops a marker at each cut as one undoable edit; follow with `split_clips` at those times to cut the clip apart. Tune `threshold` (default 0.15) if cuts are missed or invented."
    )]
    async fn detect_scenes(
        &self,
        Parameters(params): Parameters<DetectScenesParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        let result = tokio::task::spawn_blocking(move || {
            engine.detect_scenes(
                params.media_id,
                params.clip_id,
                params.start,
                params.end,
                params.threshold.unwrap_or(0.15).clamp(0.001, 1.0),
                params.min_scene.unwrap_or(0.5).max(0.0),
                params.add_markers.unwrap_or(false),
            )
        })
        .await
        .map_err(|error| to_mcp_err(format!("scene detection task panicked: {error}")))?
        .map_err(to_mcp_err)?;
        ok_json(&result)
    }

    #[tool(
        description = "List what can be applied to a clip: video `effects` (blur, key, distortion), colour `filters`, `audio` effects (voices, tone, space, noise removal, loudness) and `transitions`, each with its id, name, category and - for the first three - its parameters (key, range, default, unit). Pass the id to `update_clip`: `patch.videoEffects`, `patch.filters` or `patch.transitionIn`; the result's `usage` field shows the exact shapes."
    )]
    fn list_catalogue(
        &self,
        Parameters(params): Parameters<ListCatalogueParams>,
    ) -> Result<CallToolResult, McpError> {
        crate::catalogue::list(
            params.kind.as_deref(),
            params.include_params.unwrap_or(true),
        )
        .map_err(to_mcp_err)
        .and_then(|value| ok_json(&value))
    }

    #[tool(
        description = "List the speech models and what each is for: `transcriber` (whisper, used by `caption_clip`) and `voice` (Kokoro, used by `generate_voice`, with its speaker list), whether each is downloaded, and its size. Downloads are on demand via `download_speech_model`."
    )]
    fn speech_status(&self) -> Result<CallToolResult, McpError> {
        self.engine
            .speech_status()
            .map_err(to_mcp_err)
            .and_then(|value| ok_json(&value))
    }

    #[tool(
        description = "Download one speech model and wait until it is on disk. `kind` is `transcriber` or `voice`; pick the id from `speech_status`. Models are tens to hundreds of megabytes, so this can take minutes."
    )]
    async fn download_speech_model(
        &self,
        Parameters(params): Parameters<DownloadSpeechModelParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        tokio::task::spawn_blocking(move || engine.download_speech_model(&params.kind, &params.id))
            .await
            .map_err(|error| to_mcp_err(format!("download task panicked: {error}")))?
            .map_err(to_mcp_err)?;
        ok_json(&serde_json::json!({ "downloaded": true }))
    }

    #[tool(
        description = "Auto captions: transcribe one clip's sound and add the words to the timeline as title clips, one per spoken segment, as one undoable edit. Needs a downloaded transcriber model (see `speech_status`). Returns the segments (seconds from the start of the clip's source window) and how many captions were added."
    )]
    async fn caption_clip(
        &self,
        Parameters(params): Parameters<CaptionClipParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        let result = tokio::task::spawn_blocking(move || {
            engine.caption_clip(&params.clip_id, params.language, params.model_id)
        })
        .await
        .map_err(|error| to_mcp_err(format!("transcription task panicked: {error}")))?
        .map_err(to_mcp_err)?;
        ok_json(&result)
    }

    #[tool(
        description = "Generate a voice: speak a title clip's words (`clipId`) or some `text`, write the WAV into the project, and put it on the timeline at the title's start (or at `start`). Needs a downloaded voice model (see `speech_status`). Returns the file, its length and the bin item it became."
    )]
    async fn generate_voice(
        &self,
        Parameters(params): Parameters<GenerateVoiceParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        let result = tokio::task::spawn_blocking(move || {
            engine.generate_voice(crate::engine::VoiceRequest {
                clip_id: params.clip_id,
                text: params.text,
                start: params.start,
                voice: params.voice,
                speed: params.speed,
                model_id: params.model_id,
            })
        })
        .await
        .map_err(|error| to_mcp_err(format!("speech task panicked: {error}")))?
        .map_err(to_mcp_err)?;
        ok_json(&result)
    }

    #[tool(
        description = "Return a compact active-or-named timeline summary with optional filtered clips and markers. Prefer this over get_state when the whole project is unnecessary."
    )]
    fn get_timeline_summary(
        &self,
        Parameters(params): Parameters<TimelineSummaryParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.engine.timeline_summary(
            params.timeline_id.as_deref(),
            params.include_clips.unwrap_or(false),
            params.include_markers.unwrap_or(false),
            params.track_id.as_deref(),
            params.start,
            params.end,
            params.limit.unwrap_or(200).clamp(1, 1000),
        );
        result.map_err(to_mcp_err).and_then(|value| ok_json(&value))
    }

    #[tool(description = "Return one clip by id without returning the rest of the project.")]
    fn get_clip(
        &self,
        Parameters(params): Parameters<GetClipParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .get_clip(&params.clip_id, params.timeline_id.as_deref())
            .map_err(to_mcp_err)
            .and_then(|value| ok_json(&value))
    }

    #[tool(
        description = "Return one media-bin item by id without returning the rest of the project."
    )]
    fn get_media(
        &self,
        Parameters(params): Parameters<GetMediaParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .get_media(&params.media_id)
            .map_err(to_mcp_err)
            .and_then(|value| ok_json(&value))
    }

    #[tool(
        description = "Validate project/timeline invariants and return structured errors and warnings for missing references, invalid times, missing files, and bad transitions."
    )]
    fn validate_timeline(
        &self,
        Parameters(params): Parameters<ValidateTimelineParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .validate_timeline(params.timeline_id.as_deref())
            .map_err(to_mcp_err)
            .and_then(|value| ok_json(&value))
    }

    // ── lifecycle ──

    #[tool(
        description = "Set the folder exports go to and save the project. The folder must exist unless `create` is true."
    )]
    fn set_export_directory(
        &self,
        Parameters(params): Parameters<SetExportDirectoryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .set_export_directory(&params.directory, params.create.unwrap_or(false))
            .map_err(to_mcp_err)
            .and_then(|path| ok_json(&serde_json::json!({ "exportDirectory": path })))
    }

    #[tool(
        description = "Rename the open project's display name and manifest. The folder is not renamed; use move_project for that."
    )]
    fn rename_project(
        &self,
        Parameters(params): Parameters<RenameProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .rename_project(&params.name)
            .map_err(to_mcp_err)
            .and_then(|name| ok_json(&serde_json::json!({ "name": name })))
    }

    #[tool(
        description = "Move the open project's folder to an exact new path and reopen it there. Refuses an existing destination and cross-volume moves. Saves first, updates recents, and repoints project-owned media. Returns the project info at its new location."
    )]
    fn move_project(
        &self,
        Parameters(params): Parameters<MoveProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .move_project(&params.destination)
            .map_err(to_mcp_err)
            .and_then(|info| ok_json(&info))
    }

    #[tool(
        description = "Repoint media at replacement files, one by one (`entries`) or by swapping a folder prefix (`oldRoot` -> `newRoot`). Each replacement is probed and checked for kind, audio and duration; if any item fails, nothing changes. Use `dryRun` to see per-item verdicts first. One undo step."
    )]
    fn relink_media(
        &self,
        Parameters(params): Parameters<RelinkMediaParams>,
    ) -> Result<CallToolResult, McpError> {
        let roots = match (params.old_root, params.new_root) {
            (Some(old), Some(new)) => Some((old, new)),
            (None, None) => None,
            _ => {
                return Err(to_mcp_err(
                    "oldRoot and newRoot must be given together".to_owned(),
                ));
            }
        };
        self.engine
            .relink_media(
                params.entries.unwrap_or_default(),
                roots,
                params.dry_run.unwrap_or(false),
            )
            .map_err(to_mcp_err)
            .and_then(|report| ok_json(&report))
    }

    #[tool(
        description = "Move a rendered file to an exact destination. Never overwrites unless `overwrite`. Copies to a temp name, verifies, renames, and only then deletes the source."
    )]
    fn move_output(
        &self,
        Parameters(params): Parameters<MoveOutputParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .move_output(
                &params.source,
                &params.destination,
                params.overwrite.unwrap_or(false),
                params.verify.unwrap_or(true),
            )
            .map_err(to_mcp_err)
            .and_then(|report| ok_json(&report))
    }

    // ── batch export ──

    #[tool(
        description = "Export several stretches of the timeline, each to its own mp4, from one preflighted plan. Ranges come from `ranges` (seconds or markers) or, with `pairByName`, from markers named \"<label> start\"/\"<label> end\". Rejects empty, reversed or overlapping ranges, clashing filenames and (by default) existing files before rendering. Runs sequentially in the background; poll get_batch_export_status. A failed item does not stop the rest; resume by repeating the call with collision \"skip\". `dryRun` returns the plan only."
    )]
    fn start_batch_export(
        &self,
        Parameters(params): Parameters<StartBatchExportParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .start_batch_export(crate::engine::BatchRequest {
                ranges: params.ranges.unwrap_or_default(),
                pair_by_name: params.pair_by_name.unwrap_or(false),
                template: params
                    .filename_template
                    .unwrap_or_else(|| "{name}".to_owned()),
                output_dir: params.output_dir,
                crf: params.crf.unwrap_or(18),
                preset: params.preset.unwrap_or_else(|| "medium".to_owned()),
                collision: params.collision.unwrap_or_else(|| "fail".to_owned()),
                head: params.head_seconds.unwrap_or(0.0),
                tail: params.tail_seconds.unwrap_or(0.0),
                dry_run: params.dry_run.unwrap_or(false),
            })
            .map_err(to_mcp_err)
            .and_then(|plan| ok_json(&plan))
    }

    #[tool(
        description = "Per-item progress of the latest batch export: pending, running, done, skipped, failed (with error) or cancelled."
    )]
    fn get_batch_export_status(&self) -> Result<CallToolResult, McpError> {
        ok_json(&self.engine.batch_export_status())
    }

    #[tool(
        description = "Stop the running export or batch export at the next frame. Remaining batch items are marked cancelled."
    )]
    fn cancel_export(&self) -> Result<CallToolResult, McpError> {
        self.engine.cancel_export();
        ok_json(&serde_json::json!({ "cancelRequested": true }))
    }

    // ── layouts ──

    #[tool(
        description = "List the project's saved picture layouts (scale, offset, rotation, opacity, flips, blend, crop)."
    )]
    fn list_layout_templates(&self) -> Result<CallToolResult, McpError> {
        self.engine
            .list_layout_templates()
            .map_err(to_mcp_err)
            .and_then(|layouts| ok_json(&layouts))
    }

    #[tool(
        description = "Save a named layout, copied from a clip (`fromClipId`) and/or given as `values`. Offsets are fractions of the frame, so a layout survives an output-size change. Replaces a layout of the same name. Stored in the project."
    )]
    fn save_layout_template(
        &self,
        Parameters(params): Parameters<SaveLayoutParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .save_layout_template(
                &params.name,
                params.from_clip_id.as_deref(),
                params.values.unwrap_or_default(),
            )
            .map_err(to_mcp_err)
            .and_then(|layout| ok_json(&layout))
    }

    #[tool(description = "Delete a saved layout by name.")]
    fn remove_layout_template(
        &self,
        Parameters(params): Parameters<RemoveLayoutParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .remove_layout_template(&params.name)
            .map_err(to_mcp_err)?;
        ok_json(&serde_json::json!({ "removed": params.name }))
    }

    #[tool(
        description = "Apply a saved layout to clips chosen by `clipIds` and/or a track (optionally within a time span). One undo step; audio clips are skipped. `dryRun` reports which clips would change."
    )]
    fn apply_layout_template(
        &self,
        Parameters(params): Parameters<ApplyLayoutParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .apply_layout_template(
                &params.name,
                crate::engine::ClipSelector {
                    clip_ids: params.clip_ids.unwrap_or_default(),
                    track_id: params.track_id,
                    from: params.from,
                    to: params.to,
                },
                params.dry_run.unwrap_or(false),
            )
            .map_err(to_mcp_err)
            .and_then(|report| ok_json(&report))
    }

    #[tool(
        description = "Write all saved layouts to a JSON file so another project can import them. Refuses to replace a file unless `overwrite`."
    )]
    fn export_layout_templates(
        &self,
        Parameters(params): Parameters<LayoutFileParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .export_layout_templates(&params.path, params.overwrite.unwrap_or(false))
            .map_err(to_mcp_err)
            .and_then(|count| ok_json(&serde_json::json!({ "exported": count })))
    }

    #[tool(
        description = "Import layouts from a file written by export_layout_templates. Same-named layouts are kept unless `overwrite`. One undo step."
    )]
    fn import_layout_templates(
        &self,
        Parameters(params): Parameters<LayoutFileParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .import_layout_templates(&params.path, params.overwrite.unwrap_or(false))
            .map_err(to_mcp_err)
            .and_then(|names| ok_json(&serde_json::json!({ "imported": names })))
    }

    // ── media ──

    #[tool(
        description = "Probe a media file and add it to the project's bin in one step. Returns the resulting state; the new item's id is `createdId`."
    )]
    fn import_media(
        &self,
        Parameters(params): Parameters<ImportMediaParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.import_media(&params.path))
    }

    #[tool(
        description = "Add standard fade-from-black and fade-to-black bookends as one workflow. PuppetCat creates and reuses its own black generator asset, shifts synchronized lanes and markers by policy, places both bookends, and applies cross-fades. Repeating the call is a no-op when the track already has these bookends."
    )]
    fn add_fade_to_black(
        &self,
        Parameters(params): Parameters<AddFadeToBlackParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.add_fade_to_black(
            params.track_id,
            params.duration.unwrap_or(1.0),
            params.transition_duration.unwrap_or(0.5),
            params.shift_all_tracks.unwrap_or(true),
            params.shift_free_markers.unwrap_or(true),
        ))
    }

    // ── clip placement ──

    #[tool(
        description = "Place a clip of one bin item on a named track. Returns the resulting state; the new clip's id is `createdId`."
    )]
    fn add_clip(
        &self,
        Parameters(params): Parameters<AddClipParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::AddClip {
            media_id: params.media_id,
            track_id: params.track_id,
            start: params.start,
        }))
    }

    #[tool(
        description = "Place a clip without naming a track: lands on the lowest track with nothing in the clip's span. Returns the resulting state; the new clip's id is `createdId`."
    )]
    fn add_clip_at_first_free(
        &self,
        Parameters(params): Parameters<AddClipAtFirstFreeParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::AddClipAtFirstFree {
            media_id: params.media_id,
            start: params.start,
        }))
    }

    #[tool(
        description = "Place a title: a clip with no media behind it, styled text rendered over whatever is beneath. Returns the resulting state; the new clip's id is `createdId`."
    )]
    fn add_text_clip(
        &self,
        Parameters(params): Parameters<AddTextClipParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::AddTextClip {
            track_id: params.track_id,
            start: params.start,
            style: params.style.map(Into::into),
            duration: params.duration,
            offset_y: params.offset_y,
        }))
    }

    // ── clip editing ──

    #[tool(
        description = "Reposition any number of clips in one edit - one undo step for a whole multi-clip move."
    )]
    fn move_clips(
        &self,
        Parameters(params): Parameters<MoveClipsParams>,
    ) -> Result<CallToolResult, McpError> {
        let moves: Vec<ClipMove> = params.moves.into_iter().map(Into::into).collect();
        ok_state(self.engine.apply(Command::MoveClips { moves }))
    }

    #[tool(
        description = "Drag one edge of a clip. A head trim moves the in-point with the edge so the remaining frames stay put; either edge stops at a sixtieth of a second."
    )]
    fn trim_clip(
        &self,
        Parameters(params): Parameters<TrimClipParams>,
    ) -> Result<CallToolResult, McpError> {
        let edge: TrimEdge = params.edge.into();
        ok_state(self.engine.apply(Command::TrimClip {
            clip_id: params.clip_id,
            edge,
            delta: params.delta,
        }))
    }

    #[tool(
        description = "Cut each named clip in two at one timeline instant. The head keeps its id; the tail is minted fresh."
    )]
    fn split_clips(
        &self,
        Parameters(params): Parameters<SplitClipsParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::SplitClips {
            clip_ids: params.clip_ids,
            time: params.time,
        }))
    }

    #[tool(
        description = "Rejoin split pieces into the earliest piece, which keeps its id. Errs with the exact reason if the pieces don't qualify (different track/file/speed, a gap between them, or out of source order)."
    )]
    fn merge_clips(
        &self,
        Parameters(params): Parameters<MergeClipsParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::MergeClips {
            clip_ids: params.clip_ids,
        }))
    }

    #[tool(description = "Delete clips from the active timeline. Unknown ids are ignored.")]
    fn remove_clips(
        &self,
        Parameters(params): Parameters<RemoveClipsParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::RemoveClips {
            clip_ids: params.clip_ids,
        }))
    }

    #[tool(
        description = "Patch one clip's properties: name, volume, fades, opacity, pitch, reverse, flips, blend mode, crop, filter chains, transition, or title styling. Only the fields present in `patch` change."
    )]
    fn update_clip(
        &self,
        Parameters(params): Parameters<UpdateClipParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::UpdateClip {
            clip_id: params.clip_id,
            patch: params.patch.into(),
        }))
    }

    #[tool(
        description = "Change a clip's playback rate, holding the amount of source covered constant - the clip's timeline length is what stretches."
    )]
    fn set_clip_speed(
        &self,
        Parameters(params): Parameters<SetClipSpeedParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::SetClipSpeed {
            clip_id: params.clip_id,
            speed: params.speed,
        }))
    }

    #[tool(
        description = "Adjust a clip's picture placement: scale, offset, rotation. Each field is optional; absent fields stay put."
    )]
    fn set_clip_transform(
        &self,
        Parameters(params): Parameters<SetClipTransformParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::SetClipTransform {
            clip_id: params.clip_id,
            scale: params.scale,
            offset_x: params.offset_x,
            offset_y: params.offset_y,
            rotation: params.rotation,
        }))
    }

    // ── tracks ──

    #[tool(
        description = "Append a new lane to the active timeline. Returns the resulting state; the new track's id is `createdId`."
    )]
    fn add_track(&self) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::AddTrack))
    }

    #[tool(description = "Flip a track's visibility or mute toggle.")]
    fn set_track_flag(
        &self,
        Parameters(params): Parameters<SetTrackFlagParams>,
    ) -> Result<CallToolResult, McpError> {
        let flag: TrackFlag = params.flag.into();
        ok_state(self.engine.apply(Command::SetTrackFlag {
            track_id: params.track_id,
            flag,
            value: params.value,
        }))
    }

    // ── markers ──

    #[tool(
        description = "Drop a named marker on the active timeline at a given time - an address an agent (or the returned state) can use for that moment instead of a timestamp. Returns the resulting state; the new marker's id is `createdId`."
    )]
    fn add_marker(
        &self,
        Parameters(params): Parameters<AddMarkerParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::AddMarker {
            time: params.time,
            name: params.name,
            clip_id: params.clip_id,
        }))
    }

    #[tool(
        description = "Attach a marker to a clip so it moves with that clip, or detach it by omitting clipId. The marker keeps its current time when attached."
    )]
    fn attach_marker(
        &self,
        Parameters(params): Parameters<AttachMarkerParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::AttachMarker {
            marker_id: params.marker_id,
            clip_id: params.clip_id,
        }))
    }

    #[tool(
        description = "Rename a marker. Whitespace-only names are ignored so a marker can never end up blank."
    )]
    fn rename_marker(
        &self,
        Parameters(params): Parameters<RenameMarkerParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::RenameMarker {
            marker_id: params.marker_id,
            name: params.name,
        }))
    }

    #[tool(description = "Move a marker to a new time on the active timeline, floored at 0.")]
    fn move_marker(
        &self,
        Parameters(params): Parameters<MoveMarkerParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::MoveMarker {
            marker_id: params.marker_id,
            time: params.time,
        }))
    }

    #[tool(description = "Delete a marker from the active timeline. An unknown id is ignored.")]
    fn remove_marker(
        &self,
        Parameters(params): Parameters<RemoveMarkerParams>,
    ) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.apply(Command::RemoveMarker {
            marker_id: params.marker_id,
        }))
    }

    // ── escape hatch ──

    #[tool(
        description = "Apply any Command directly as JSON - the same shape the window's own edits take. Use this for operations with no dedicated tool (see the command field's own description for the full list)."
    )]
    fn apply_raw_command(
        &self,
        Parameters(params): Parameters<ApplyRawCommandParams>,
    ) -> Result<CallToolResult, McpError> {
        let command = parse_raw_command(params.command).map_err(to_mcp_err)?;
        ok_state(self.engine.apply(command))
    }

    #[tool(
        description = "Apply several commands as one transaction. Requires `expectedRevision` (the `revision` of the latest state); a stale value is rejected. The plan is staged on a copy, validated, then committed as a single undo step - any failure commits nothing. Use `dryRun` to preview: returns old/new revision, per-operation `createdIds`, `refs`, clip/marker/media deltas, duration change, the validation report and a field-level `diff`. Includes ripple ops sharing ripplePolicy (none|selectedTracks|allTracks), linkPolicy (preserve|detach|explicit) and markerPolicy (attachedOnly|shiftFree|fixed): `rippleDelete` {clipIds}, `rippleTrim` {clipId, edge: start|end, delta}, `rippleInsert` {mediaId, trackId, start} and `insertGap` {at, duration, trackIds} and `rippleDeleteRange` {start, end, trackIds}, which cuts a span of time out of every (or the named) lane and closes it. Defaults: selectedTracks, preserve, shiftFree."
    )]
    fn apply_edit_plan(
        &self,
        Parameters(params): Parameters<ApplyEditPlanParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .apply_edit_plan(
                params.expected_revision,
                params.operations,
                params.dry_run.unwrap_or(false),
                params.validate.unwrap_or(true),
            )
            .map_err(to_mcp_err)
            .and_then(|result| ok_json(&result))
    }

    // ── history ──

    #[tool(description = "Undo the last applied edit.")]
    fn undo(&self) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.undo())
    }

    #[tool(description = "Redo the last undone edit.")]
    fn redo(&self) -> Result<CallToolResult, McpError> {
        ok_state(self.engine.redo())
    }

    // ── vision ──

    #[tool(
        description = "Render the timeline at one instant and return it as a PNG image, so you can see what the edit actually looks like instead of only reading its data."
    )]
    async fn render_snapshot(
        &self,
        Parameters(params): Parameters<RenderSnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        let png = tokio::task::spawn_blocking(move || {
            engine.render_snapshot(params.time, params.width, params.height)
        })
        .await
        .map_err(|error| to_mcp_err(format!("render task panicked: {error}")))?
        .map_err(to_mcp_err)?;
        let encoded = STANDARD.encode(png);
        Ok(CallToolResult::success(vec![ContentBlock::image(
            encoded,
            "image/png",
        )]))
    }

    #[tool(
        description = "Render up to 64 timeline instants through the exact preview/export compositor. Returns a PNG contact sheet plus cell/time metadata and visual-change scores. Optionally writes the sheet and every full-resolution frame to explicit paths."
    )]
    async fn render_contact_sheet(
        &self,
        Parameters(params): Parameters<RenderContactSheetParams>,
    ) -> Result<CallToolResult, McpError> {
        let engine = self.engine.clone();
        let result = tokio::task::spawn_blocking(move || {
            engine.render_contact_sheet(
                params.times,
                params.width,
                params.height,
                params.columns,
                params.output,
                params.frames_output_dir,
            )
        })
        .await
        .map_err(|error| to_mcp_err(format!("contact-sheet task panicked: {error}")))?
        .map_err(to_mcp_err)?;
        let metadata = serde_json::to_string(&result.metadata())
            .map_err(|error| to_mcp_err(error.to_string()))?;
        Ok(CallToolResult::success(vec![
            ContentBlock::image(STANDARD.encode(result.png), "image/png"),
            ContentBlock::text(metadata),
        ]))
    }

    // ── export ──

    #[tool(
        description = "Start exporting the active session to a video file - all of it, or only a stretch given by start/end seconds or fromMarker/toMarker. Errs immediately if an export is already running; poll get_export_status for progress."
    )]
    fn start_export(
        &self,
        Parameters(params): Parameters<StartExportParams>,
    ) -> Result<CallToolResult, McpError> {
        self.engine
            .start_export(
                &params.output,
                params.crf.unwrap_or(20),
                &params.preset.unwrap_or_else(|| "medium".to_owned()),
                &crate::engine::ExportRangeArgs {
                    start: params.start,
                    end: params.end,
                    from_marker: params.from_marker,
                    to_marker: params.to_marker,
                },
            )
            .map_err(to_mcp_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            "export started",
        )]))
    }

    #[tool(description = "Poll the running (or most recently finished) export's progress.")]
    fn get_export_status(&self) -> Result<CallToolResult, McpError> {
        ok_json(&self.engine.export_status())
    }
}

#[tool_handler]
impl ServerHandler for ConcatAgentServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Drives a full PuppetCat editing session with no window: create or open a \
             project, edit its timeline through curated tools or the apply_raw_command \
             escape hatch (which covers every operation), render_snapshot to see the \
             result, and export. One project is open at a time; create_project or \
             open_project must be called before any editing tool.\n\n\
             Opening or creating a project here takes an exclusive claim on it: if the \
             PuppetCat window has that same project open, it blocks all input there for \
             as long as this session keeps the project open - not only while a command \
             is actually running. Call close_project as soon as the current change or \
             set of changes is done, even if the same project will be opened again soon, \
             so the person at the window can resume; save_project alone does not release \
             the claim, only close_project does."
                    .to_owned(),
            )
    }
}
