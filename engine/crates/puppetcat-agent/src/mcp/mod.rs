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
    let text =
        serde_json::to_string(value).map_err(|error| to_mcp_err(error.to_string()))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn ok_state(result: Result<puppetcat_host::EditorView, String>) -> Result<CallToolResult, McpError> {
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
        self.engine.get_state().map_err(to_mcp_err).and_then(|view| ok_json(&view))
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

    // ── export ──

    #[tool(
        description = "Start exporting the active session to a video file. Errs immediately if an export is already running; poll get_export_status for progress."
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
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
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
