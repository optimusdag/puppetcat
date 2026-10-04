// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Every edit operation, as data.
//!
//! A [`Command`] is what the window sends; [`apply`] is the one place its
//! meaning lives. The clamps and tolerances documented on each variant are
//! the contract the window's gesture echo mirrors and tests against.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{
    AnimationSlot, AppliedFilter, Clip, ClipAnimation, ClipKind, Crop, CustomFont, Marker,
    MediaItem, MediaKind, Project, SpeedPoint, TextStyle, Timeline, Track, Transition,
};

/// Fallback length for media whose container reports no duration.
const UNKNOWN_DURATION: f64 = 5.0;
/// How long a still lasts when first placed. Editorial default, not a fact.
const DEFAULT_IMAGE_DURATION: f64 = 5.0;
/// How long a title lasts when first placed.
const DEFAULT_TEXT_DURATION: f64 = 4.0;
/// How long a layer covers when first placed. Editorial default, not a fact.
const DEFAULT_LAYER_DURATION: f64 = 5.0;
const MIN_CLIP_DURATION: f64 = 1.0 / 60.0;
/// The engine's speed range (puppetcat-media `SPEED_RANGE`), verbatim.
const MIN_SPEED: f64 = 0.0625;
const MAX_SPEED: f64 = 16.0;
const MIN_SCALE: f64 = 0.05;
const MAX_SCALE: f64 = 8.0;
const MAX_OFFSET: f64 = 3.0;
/// How far apart two clips may sit and still count as touching, in seconds.
const JOIN_EPSILON: f64 = 1e-6;

/// Which end of a clip a trim drags. The two are not symmetric: see
/// [`Command::TrimClip`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrimEdge {
    /// The head. Trimming here moves the in-point with the edge, so the
    /// remaining frames stay where they were on the timeline.
    Start,
    /// The tail. Trimming here only lengthens or shortens the clip.
    End,
}

/// Which per-track toggle a [`Command::SetTrackFlag`] flips.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackFlag {
    /// [`Track::visible`]: whether the track's video reaches the composite.
    Visible,
    /// [`Track::muted`]: whether the track's audio is silenced.
    Muted,
}

/// Which lanes close up after a ripple delete.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RipplePolicy {
    /// Leave a gap.
    None,
    /// Close the gap on the lanes the deleted clips sat on.
    #[default]
    SelectedTracks,
    /// Close it on every lane.
    AllTracks,
}

/// What happens to a video clip's detached audio (and the reverse).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkPolicy {
    /// Linked partners are deleted and shifted together.
    #[default]
    Preserve,
    /// Partners of deleted clips stay, unlinked.
    Detach,
    /// Only the named clips are touched; partners are left alone.
    Explicit,
}

/// Where markers go when the gap closes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkerPolicy {
    /// Markers attached to clips ride with them; free markers stay put.
    AttachedOnly,
    /// Attached markers ride with clips and free markers shift too.
    #[default]
    ShiftFree,
    /// No marker changes its time: attached markers are let go first.
    Fixed,
}

/// Where one clip is going, in a multi-clip move.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipMove {
    /// The clip to move. An unknown id is skipped, not an error - the rest
    /// of the batch of moves still lands.
    pub clip_id: String,
    /// New timeline position in seconds, floored at 0.
    pub start: f64,
    /// Destination track. An unknown id moves the clip in time but leaves it
    /// on its current track.
    pub track_id: String,
}

/// A partial update to one clip. Every field optional; `transition_in` and
/// `text` are double-optional so "clear it" and "leave it alone" stay
/// distinct on the wire (absent = untouched, null = cleared).
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipPatch {
    /// New display name, taken verbatim. A later `text` patch overwrites it
    /// with the title's first line.
    pub name: Option<String>,
    /// New gain; floored at 0, deliberately not capped at 1 - boosting quiet
    /// footage is legitimate.
    pub volume: Option<f64>,
    /// New fade-in length in seconds, floored at 0.
    pub fade_in: Option<f64>,
    /// New fade-out length in seconds, floored at 0.
    pub fade_out: Option<f64>,
    /// New opacity, clamped into 0..=1.
    pub opacity: Option<f64>,
    /// New pitch-preservation setting, taken as sent.
    pub preserve_pitch: Option<bool>,
    /// Play backwards.
    #[serde(default)]
    pub reverse: Option<bool>,
    /// Mirror left to right.
    #[serde(default)]
    pub flip_h: Option<bool>,
    /// Mirror top to bottom.
    #[serde(default)]
    pub flip_v: Option<bool>,
    /// The blend mode's name; empty is normal.
    #[serde(default)]
    pub blend: Option<String>,
    /// The crop; `Some(None)` takes it off.
    #[serde(default)]
    pub crop: Option<Option<Crop>>,
    /// Wholesale replacement of the audio filter chain - the UI sends the
    /// full list, not a diff.
    pub filters: Option<Vec<AppliedFilter>>,
    /// Wholesale replacement of the video effect chain, like `filters`.
    pub video_effects: Option<Vec<AppliedFilter>>,
    /// The transition on the cut into the clip: absent leaves it alone,
    /// null clears it, a value replaces it.
    #[serde(
        default,
        with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub transition_in: Option<Option<Transition>>,
    /// The title styling, same three-way wire semantics as `transition_in`.
    /// Setting a style also renames the clip after its first line.
    #[serde(
        default,
        with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub text: Option<Option<TextStyle>>,
}

/// `Option<Option<T>>` over JSON: absent → None, null → Some(None).
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

/// A media item as probed by the host, before the model mints its id.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMedia {
    /// Absolute path on disk. Adding a path already in the bin is a no-op,
    /// so re-imports cannot duplicate media.
    pub path: String,
    /// Display name for the bin, normally the file's basename.
    pub name: String,
    /// Seconds, or None when the container did not report one - clips of
    /// such media get a five-second fallback length.
    pub duration: Option<f64>,
    /// What the host's probe decided the file is.
    pub kind: MediaKind,
    /// Pixel width, when the probe found one.
    pub width: Option<u32>,
    /// Pixel height, same terms as `width`.
    pub height: Option<u32>,
    /// Frames per second as a decimal, for display.
    pub frame_rate: Option<f64>,
    /// The exact rate fraction the engine works in, e.g. "30000/1001".
    pub frame_rate_fraction: Option<String>,
    /// Codec name as probed, e.g. "h264". Informational.
    pub video_codec: Option<String>,
    /// Codec of the embedded audio, when there is any.
    pub audio_codec: Option<String>,
    /// Whether the file carries an audio stream.
    pub has_audio: bool,
}

/// Every edit, as the window sends it: a tagged `op` plus camelCase
/// fields. [`apply`] is the single place each variant's meaning lives; the
/// notes here state the contract - clamps, tolerances, what gets minted -
/// so a caller need not read `apply` to know what a command will do.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Command {
    /// Imports a file into the bin, minting an "m" id. A path already
    /// present is a tolerated no-op that mints nothing.
    AddMedia {
        /// The probed file, as described by the host.
        item: NewMedia,
    },
    /// Removes a bin item and every clip referencing it, on *all* timelines
    /// - a clip whose media is gone would linger as a dead reference.
    RemoveMedia {
        /// The bin item to remove. An unknown id is a no-op.
        media_id: String,
    },
    /// Marks a bin item as a template slot (or back to ordinary media),
    /// which is what [`Command::FillSlot`] requires of its target.
    SetMediaPlaceholder {
        /// The bin item to mark. An unknown id is a no-op.
        media_id: String,
        /// True to make it a slot, false to make it ordinary media again.
        placeholder: bool,
    },
    /// Swaps the user's file into a template slot in place. The slot keeps
    /// its id so clips keep working; start, duration and speed stay the
    /// template's, while the in-point resets and each clip's kind and name
    /// follow the new file, across all timelines. Errs if the id is unknown
    /// or the item is not a placeholder.
    FillSlot {
        /// The slot being filled - must have `placeholder` set.
        media_id: String,
        /// The user's file that takes the slot's place.
        item: NewMedia,
    },
    /// Several commands as one atomic edit: applied to a staged copy and
    /// committed only if every one succeeds, then recorded as a single undo
    /// step. The outcome carries the last id minted inside.
    Batch {
        /// The commands, applied in order. Nesting is legal.
        commands: Vec<Command>,
    },
    /// Places a clip of `media_id` on a named track, minting a "c" id.
    /// Duration comes from the media (five seconds for a still or unknown
    /// length); errs if the media or track no longer exists.
    AddClip {
        /// The bin item to cut from.
        media_id: String,
        /// The lane to place it on.
        track_id: String,
        /// Timeline position in seconds, floored at 0.
        start: f64,
    },
    /// [`Command::AddClip`] without naming a lane: lands on the lowest
    /// track with nothing in the clip's span, falling back to the bottom
    /// track (overlap and all) rather than refusing.
    AddClipAtFirstFree {
        /// The bin item to cut from.
        media_id: String,
        /// Timeline position in seconds, floored at 0.
        start: f64,
    },
    /// Adds the conventional fade-from-black and fade-to-black bookends in
    /// one edit. `media_id` names an agent-managed black still (or video),
    /// all selected clips move forward to make room, free markers follow by
    /// policy, and both cuts receive a cross-fade transition.
    AddFadeToBlack {
        /// Black media already present in the bin.
        media_id: String,
        /// The visual lane whose first and last clips receive the fades.
        track_id: String,
        /// Length of each black bookend and the amount shifted at the head.
        duration: f64,
        /// Length of each cross-fade.
        transition_duration: f64,
        /// Shift every lane to preserve sync; false shifts only `track_id`.
        shift_all_tracks: bool,
        /// Move free-floating markers with the edit. Attached markers always
        /// follow their clips.
        shift_free_markers: bool,
    },
    /// Places a title: a clip with no media behind it, named after the
    /// text's first line, minting a "c" id.
    AddTextClip {
        /// The lane to place it on. None picks the first free track like
        /// [`Command::AddClipAtFirstFree`]; naming a vanished track errs.
        track_id: Option<String>,
        /// Timeline position in seconds, floored at 0.
        start: f64,
        /// The words and their look. None means [`TextStyle::default`].
        style: Option<TextStyle>,
        /// Seconds on the timeline; the editorial default when absent. Here
        /// so a caption run can land as one batch instead of add-then-trim
        /// per clip - a batch cannot trim a clip whose id it cannot know yet.
        #[serde(default)]
        duration: Option<f64>,
        /// Vertical placement as a frame-height fraction, clamped like
        /// SetClipTransform. Lower thirds are made of this.
        #[serde(default)]
        offset_y: Option<f64>,
    },
    /// Places a layer: a look or an effect over a span of the timeline that
    /// treats everything beneath it. No media; the chain starts as the one
    /// package named, at its defaults, and the clip's opacity is how hard it
    /// is applied.
    AddLayerClip {
        /// The lane to place it on; None picks the first free track.
        track_id: Option<String>,
        /// Timeline position in seconds, floored at 0.
        start: f64,
        /// Seconds on the timeline; the editorial default when absent.
        #[serde(default)]
        duration: Option<f64>,
        /// The package the layer applies, e.g. "puppetcat.warm".
        effect_id: String,
        /// What the lane calls it.
        name: String,
    },
    /// Gives a clip a speed curve, or takes it away. The source covered is
    /// held constant, as a speed change does, so the clip's timeline length
    /// follows the curve's mean; `speed` is kept at that mean.
    SetClipSpeedCurve {
        /// The clip to retime.
        clip_id: String,
        /// The curve, or None for a constant rate at the current mean.
        curve: Option<Vec<SpeedPoint>>,
    },
    /// Sets or clears the animation on one slot of a clip.
    SetClipAnimation {
        /// The clip.
        clip_id: String,
        /// Which end, or the whole.
        slot: AnimationSlot,
        /// The shape and its seconds, or None to take it off.
        animation: Option<ClipAnimation>,
    },
    /// Repositions any number of clips in one edit - one undo step for a
    /// whole multi-selection drag. Unknown clips and tracks are tolerated
    /// per [`ClipMove`].
    MoveClips {
        /// Where each clip is going.
        moves: Vec<ClipMove>,
    },
    /// Drags one edge of a clip. A head trim moves the in-point with the
    /// edge (scaled by speed) so the remaining pixels do not slide; either
    /// edge stops at the sixtieth-of-a-second minimum duration. An unknown
    /// clip is a no-op.
    TrimClip {
        /// The clip to trim.
        clip_id: String,
        /// Which edge is being dragged.
        edge: TrimEdge,
        /// Signed seconds of timeline the edge moves: positive drags the
        /// head right (shortening) or the tail right (lengthening).
        delta: f64,
    },
    /// Cuts each named clip in two at one playhead time. The head keeps the
    /// id and the transition; the tail is minted fresh and stays
    /// source-continuous. A clip the time misses (or grazes within the
    /// minimum duration) is skipped.
    SplitClips {
        /// The clips under the playhead - normally the selection.
        clip_ids: Vec<String>,
        /// The cut point, in timeline seconds.
        time: f64,
    },
    /// Rejoins split pieces into the earliest piece, which keeps its id.
    /// Errs with a user-facing sentence ([`why_not_merge`]) unless the
    /// pieces sit on one track, come from one file at one speed, touch
    /// within a microsecond, and are still in source order.
    MergeClips {
        /// The pieces to rejoin, any order.
        clip_ids: Vec<String>,
    },
    /// Deletes clips from the active timeline. Unknown ids are ignored.
    RemoveClips {
        /// The clips to delete.
        clip_ids: Vec<String>,
    },
    /// Applies a [`ClipPatch`]: only the fields present change, with the
    /// clamps documented on the patch. An unknown clip is a no-op.
    UpdateClip {
        /// The clip to patch.
        clip_id: String,
        /// Which properties change, and to what.
        patch: ClipPatch,
    },
    /// Changes playback rate while holding the amount of source covered
    /// constant - the clip's timeline duration is what stretches, which is
    /// what makes this a speed change rather than a trim.
    SetClipSpeed {
        /// The clip to retime. An unknown id is a no-op.
        clip_id: String,
        /// The new rate, clamped into 0.0625..=16 (the engine's range).
        speed: f64,
    },
    /// Adjusts the picture's placement. Each field is optional so a drag
    /// can send just the axis it moved; absent fields stay put.
    SetClipTransform {
        /// The clip to place. An unknown id is a no-op.
        clip_id: String,
        /// New scale, clamped into 0.05..=8.
        scale: Option<f64>,
        /// New horizontal offset as a frame-width fraction, clamped to ±3.
        offset_x: Option<f64>,
        /// New vertical offset as a frame-height fraction, clamped to ±3.
        offset_y: Option<f64>,
        /// New rotation in degrees, wrapped into (-180, 180] so a full drag
        /// never accumulates turns.
        rotation: Option<f64>,
    },
    /// Pulls a video clip's sound out into its own audio clip on a free
    /// lane (minting one if none is free), muting the video and moving its
    /// audio filters to the sound. A no-op unless the clip is an unmuted
    /// video whose media has audio and is not already detached.
    DetachAudio {
        /// The video clip to detach from.
        clip_id: String,
    },
    /// Undoes a detach: deletes the detached sound clip(s), unmutes the
    /// video and hands the sound's filters back. Accepts either the video's
    /// id or the sound's; a no-op when either side is gone.
    ReattachAudio {
        /// The video clip - or its detached sound.
        clip_id: String,
    },
    /// Appends a lane named after the highest "Track N" in use, minting a
    /// "t" id.
    AddTrack,
    /// Deletes a lane and every clip on it. Errs at the floor of one track.
    RemoveTrack {
        /// The lane to delete.
        track_id: String,
    },
    /// Renames a lane. Whitespace-only names are ignored so a track can
    /// never end up blank; unknown ids are tolerated.
    RenameTrack {
        /// The lane to rename.
        track_id: String,
        /// The new label; trimmed before it lands.
        name: String,
    },
    /// Flips one of a track's two toggles. An unknown id is a no-op.
    SetTrackFlag {
        /// The lane to change.
        track_id: String,
        /// Which toggle: visibility or mute.
        flag: TrackFlag,
        /// The new setting.
        value: bool,
    },
    /// Drops a named pin on the active timeline, minting an "mk" id - the
    /// address an agent gives a moment instead of its timestamp.
    AddMarker {
        /// Seconds from the start of the timeline, floored at 0.
        time: f64,
        /// Label; "Marker N" after the highest already in use when absent
        /// or blank.
        name: Option<String>,
        /// Attaches the new marker to this clip, so it moves with it. An
        /// unknown id leaves the marker free-floating.
        #[serde(default)]
        clip_id: Option<String>,
    },
    /// Deletes clips and closes the gap they leave, under explicit policies
    /// for which lanes close up, what happens to linked audio, and how
    /// markers move. All of it is one edit.
    RippleDelete {
        /// The clips to delete. Unknown ids are ignored.
        clip_ids: Vec<String>,
        /// Which lanes close the gap.
        #[serde(default)]
        ripple_policy: RipplePolicy,
        /// What becomes of linked audio.
        #[serde(default)]
        link_policy: LinkPolicy,
        /// What markers do.
        #[serde(default)]
        marker_policy: MarkerPolicy,
    },
    /// Cuts a span of time out of the timeline and closes it up: every clip
    /// is split at both edges, what falls inside is deleted, and what comes
    /// after moves earlier by the span. The one primitive behind removing
    /// dead air, filler words and unwanted takes - a ripple delete of a
    /// *time range* rather than of named clips. One edit.
    RippleDeleteRange {
        /// Where the cut begins, in timeline seconds.
        start: f64,
        /// Where it ends.
        end: f64,
        /// The lanes to cut; empty cuts every lane and closes up all of
        /// them, so sound and picture stay in step.
        #[serde(default)]
        track_ids: Vec<String>,
        /// What free markers do; attached ones ride with their clips.
        #[serde(default)]
        marker_policy: MarkerPolicy,
    },
    /// Trims a clip and moves everything after it by the change in length,
    /// so no gap opens and no overlap forms. Trimming the head keeps the
    /// clip where it started rather than sliding it forward. Policies are
    /// those of [`Command::RippleDelete`]; under `preserve`, linked audio is
    /// trimmed the same way.
    RippleTrim {
        /// The clip to trim.
        clip_id: String,
        /// Which edge moves.
        edge: TrimEdge,
        /// Seconds the edge moves; positive is rightwards.
        delta: f64,
        /// Which lanes follow.
        #[serde(default)]
        ripple_policy: RipplePolicy,
        /// What becomes of linked audio.
        #[serde(default)]
        link_policy: LinkPolicy,
        /// What markers do.
        #[serde(default)]
        marker_policy: MarkerPolicy,
    },
    /// Places a clip and pushes everything at or after its start later by
    /// its length. Clips that straddle the insertion point are left where
    /// they are. Mints a "c" id.
    RippleInsert {
        /// The bin item to cut from.
        media_id: String,
        /// The lane to place it on.
        track_id: String,
        /// Where it goes, in seconds.
        start: f64,
        /// Which lanes make room.
        #[serde(default)]
        ripple_policy: RipplePolicy,
        /// What becomes of linked audio.
        #[serde(default)]
        link_policy: LinkPolicy,
        /// What markers do.
        #[serde(default)]
        marker_policy: MarkerPolicy,
    },
    /// Opens empty time: everything at or after `at` moves later by
    /// `duration`. With `selectedTracks`, `track_ids` names the lanes.
    InsertGap {
        /// Where the gap opens, in seconds.
        at: f64,
        /// Its length in seconds; zero or less is a no-op.
        duration: f64,
        /// The lanes that make room under `selectedTracks`.
        #[serde(default)]
        track_ids: Vec<String>,
        /// Which lanes make room.
        #[serde(default)]
        ripple_policy: RipplePolicy,
        /// What becomes of linked audio.
        #[serde(default)]
        link_policy: LinkPolicy,
        /// What markers do.
        #[serde(default)]
        marker_policy: MarkerPolicy,
    },
    /// Saves a layout under its name, replacing one of the same name. A blank
    /// name is a no-op.
    SaveLayoutTemplate {
        /// The layout to keep.
        layout: crate::model::LayoutTemplate,
    },
    /// Deletes a layout by name. An unknown name is a no-op.
    RemoveLayoutTemplate {
        /// The layout to drop.
        name: String,
    },
    /// Stamps a saved layout onto clips: scale, offset, rotation, opacity,
    /// flips, blend and crop. Errs if the layout does not exist; unknown or
    /// non-visual clip ids are skipped.
    ApplyLayoutTemplate {
        /// The layout to apply.
        name: String,
        /// The clips to restyle.
        clip_ids: Vec<String>,
    },
    /// Points a bin item at a different file - the relink after media was
    /// moved or replaced. Only the path changes: the id, name and every clip
    /// cut from it stay, so the caller is responsible for checking the new
    /// file is a compatible stand-in. An unknown id or an unchanged path is
    /// a no-op.
    SetMediaPath {
        /// The bin item to repoint.
        media_id: String,
        /// The file that now backs it.
        path: String,
    },
    /// Attaches a marker to a clip (it keeps its current time, so it will
    /// follow the clip from here on), or detaches it with `None`.
    AttachMarker {
        /// The marker to attach or detach.
        marker_id: String,
        /// The clip to ride on; None to free the marker.
        clip_id: Option<String>,
    },
    /// Deletes a marker from the active timeline. An unknown id is a no-op.
    RemoveMarker {
        /// The marker to delete.
        marker_id: String,
    },
    /// Renames a marker, with the same trim-and-ignore-blank rule as
    /// [`Command::RenameTrack`].
    RenameMarker {
        /// The marker to rename.
        marker_id: String,
        /// The new label; trimmed before it lands.
        name: String,
    },
    /// Repositions a marker on the active timeline. An unknown id is a
    /// no-op.
    MoveMarker {
        /// The marker to move.
        marker_id: String,
        /// New time in seconds, floored at 0.
        time: f64,
    },
    /// Adds a fresh timeline - four new lanes, "Timeline N" after the
    /// highest in use - and makes it active. Mints a "tl" id.
    AddTimeline,
    /// Deletes a timeline, moving the active tab to a neighbour if it was
    /// this one. Errs at the floor of one timeline.
    RemoveTimeline {
        /// The timeline to delete. An unknown id is a no-op.
        timeline_id: String,
    },
    /// Renames a timeline tab, with the same trim-and-ignore-blank rule as
    /// [`Command::RenameTrack`].
    RenameTimeline {
        /// The timeline to rename.
        timeline_id: String,
        /// The new tab label; trimmed before it lands.
        name: String,
    },
    /// Switches which timeline subsequent commands act on. An unknown id
    /// leaves the selection where it was.
    SelectTimeline {
        /// The timeline to switch to.
        timeline_id: String,
    },
    /// Moves a timeline tab to a new position in the strip. Which timeline
    /// is active does not change - only the order of the tabs.
    MoveTimeline {
        /// The timeline to move. An unknown id is a no-op.
        timeline_id: String,
        /// Where it lands among its siblings, 0-based, counted with the
        /// timeline already removed from its old slot. Clamped to the end.
        index: usize,
    },
    /// Registers a font file for titles. A path already registered is a
    /// no-op, so re-adding cannot duplicate.
    AddFont {
        /// The family name titles will refer to.
        family: String,
        /// Where the font file lives on disk.
        path: String,
    },
    /// Unregisters a font family. Clips keep the family name: the face may
    /// come back when the file does.
    RemoveFont {
        /// The family to unregister.
        family: String,
    },
}

/// What a command produced, beyond the new state: the ids it minted, so the
/// UI can select what it just created, and whether anything changed at all.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// The id of what the command created - a clip, track, timeline, or
    /// media item - or, for a batch, the last id minted inside it. Absent
    /// when nothing was created, including tolerated no-ops.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_id: Option<String>,
    /// Whether the command actually changed the project. A tolerated no-op -
    /// a missing id, a field set to the value it already had - succeeds but
    /// reports false, which is how the editor knows not to record an undo
    /// snapshot for it. For a batch: whether any member changed anything.
    #[serde(default)]
    pub applied: bool,
}

/// Why a command was refused. Every variant renders through `Display` as the
/// exact user-facing sentence the UI shows - byte for byte the strings
/// the window treats as the contract; the enum only gives
/// those sentences names a host can match on.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum CommandError {
    /// [`Command::FillSlot`] named a media id no longer in the bin.
    #[error("That template slot no longer exists.")]
    SlotGone,
    /// [`Command::FillSlot`] targeted ordinary media rather than a slot.
    #[error("That media is not a template slot.")]
    NotASlot,
    /// A clip-placing command named media no longer in the bin.
    #[error("That media is no longer in the bin.")]
    MediaGone,
    /// A clip-placing command named a track no longer on the timeline.
    #[error("That track no longer exists.")]
    TrackGone,
    /// A first-free-track placement found a timeline with no tracks at all.
    #[error("There are no tracks.")]
    NoTracks,
    /// [`Command::AddFadeToBlack`] found no content to bookend.
    #[error("That track has no clips to fade.")]
    FadeTrackEmpty,
    /// A fade workflow cannot safely guess what a leading timeline gap means.
    #[error("The first clip on that track must start at the beginning of the timeline.")]
    FadeTrackDoesNotStartAtZero,
    /// [`Command::MergeClips`] was refused, for whichever [`why_not_merge`]
    /// reason applied.
    #[error("{reason}")]
    CannotMerge {
        /// The [`why_not_merge`] sentence, verbatim.
        reason: String,
    },
    /// [`Command::RemoveTrack`] would have deleted the last track.
    #[error("A timeline needs at least one track.")]
    LastTrack,
    /// [`Command::RemoveTimeline`] would have deleted the last timeline.
    #[error("A project needs at least one timeline.")]
    LastTimeline,
    /// A command was refused for a reason specific to its arguments.
    #[error("{0}")]
    Rejected(String),
}

/// Mints ids. Owned by the editor so restored projects advance it past every
/// id a file already uses - the collision class `adoptProject` fixed in the
/// UI is prevented here instead.
#[derive(Clone, Default, Debug)]
pub struct IdMint {
    counter: u64,
}

impl IdMint {
    /// The next fresh id: the prefix ("c", "t", "tl", "m") plus a counter
    /// shared across all prefixes, so no two ids ever share a number.
    pub fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    /// Advances the counter past `id`'s numeric suffix, if it has one.
    pub fn adopt(&mut self, id: &str) {
        let digits: String = id
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if let Ok(value) = digits.parse::<u64>() {
            self.counter = self.counter.max(value);
        }
    }

    /// Adopts every id a restored project uses - media, timelines, tracks,
    /// clips - so nothing minted afterwards can collide with the file.
    pub fn adopt_project(&mut self, project: &Project) {
        for item in &project.media {
            self.adopt(&item.id);
        }
        for timeline in &project.timelines {
            self.adopt(&timeline.id);
            for track in &timeline.tracks {
                self.adopt(&track.id);
            }
            for clip in &timeline.clips {
                self.adopt(&clip.id);
            }
            for marker in &timeline.markers {
                self.adopt(&marker.id);
            }
        }
    }
}

fn default_clip(id: String, track_id: String, media: &MediaItem, start: f64) -> Clip {
    Clip {
        id,
        track_id,
        media_id: media.id.clone(),
        name: media.name.clone(),
        kind: match media.kind {
            MediaKind::Video => ClipKind::Video,
            MediaKind::Audio => ClipKind::Audio,
            MediaKind::Image => ClipKind::Image,
        },
        start: start.max(0.0),
        duration: match media.kind {
            MediaKind::Image => DEFAULT_IMAGE_DURATION,
            _ => media.duration.unwrap_or(UNKNOWN_DURATION),
        },
        source_start: 0.0,
        volume: 1.0,
        fade_in: 0.0,
        fade_out: 0.0,
        scale: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        speed: 1.0,
        preserve_pitch: true,
        speed_curve: None,
        reverse: false,
        animation_in: None,
        animation_out: None,
        animation_combo: None,
        flip_h: false,
        flip_v: false,
        blend: String::new(),
        crop: None,
        filters: Vec::new(),
        video_effects: Vec::new(),
        muted: None,
        detached_from: None,
        transition_in: None,
        text: None,
    }
}

/// The lowest track with nothing occupying `[start, start + duration)`,
/// falling back to the bottom track.
fn first_free_track(timeline: &Timeline, start: f64, duration: f64) -> Option<String> {
    let end = start + duration;
    timeline
        .tracks
        .iter()
        .find(|track| {
            !timeline.clips.iter().any(|clip| {
                clip.track_id == track.id && clip.start < end && start < clip.start + clip.duration
            })
        })
        .or(timeline.tracks.first())
        .map(|track| track.id.clone())
}

/// A one-line label for a block of text.
fn first_line(content: &str) -> String {
    let line = content
        .lines()
        .find(|candidate| !candidate.trim().is_empty())
        .unwrap_or("Text")
        .trim();
    let label: String = line.chars().take(40).collect();
    if label.is_empty() {
        "Text".to_owned()
    } else {
        label
    }
}

/// "Track 5" from the highest number already in use, not from the count.
fn next_numbered(name: &str, existing: impl Iterator<Item = String>) -> String {
    let highest = existing
        .filter_map(|candidate| {
            let digits: String = candidate.chars().filter(char::is_ascii_digit).collect();
            digits.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0);
    format!("{name} {}", highest + 1)
}

/// Why these clips cannot be merged, or None if they can. A sentence, because
/// a disabled button that will not say why is worse than no button.
pub fn why_not_merge(timeline: &Timeline, clip_ids: &[String]) -> Option<String> {
    if clip_ids.len() < 2 {
        return Some("Select two or more clips to merge.".to_owned());
    }
    let clips: Vec<&Clip> = clip_ids.iter().filter_map(|id| timeline.clip(id)).collect();
    if clips.len() < 2 {
        return Some("Select two or more clips to merge.".to_owned());
    }
    if clips.iter().any(|clip| clip.track_id != clips[0].track_id) {
        return Some("Merged clips must be on the same track.".to_owned());
    }
    if clips.iter().any(|clip| clip.media_id != clips[0].media_id) {
        return Some("Merged clips must come from the same file.".to_owned());
    }
    if clips.iter().any(|clip| clip.speed != clips[0].speed) {
        return Some("Merged clips must play at the same speed.".to_owned());
    }

    let mut ordered = clips.clone();
    ordered.sort_by(|left, right| left.start.total_cmp(&right.start));
    for pair in ordered.windows(2) {
        let (previous, current) = (pair[0], pair[1]);
        if (current.start - (previous.start + previous.duration)).abs() > JOIN_EPSILON {
            return Some("Merged clips must touch, with no gap or overlap.".to_owned());
        }
        if (current.source_start - (previous.source_start + previous.duration * previous.speed))
            .abs()
            > JOIN_EPSILON
        {
            return Some("These pieces are no longer in their original order.".to_owned());
        }
    }
    None
}

/// The lanes a ripple touches: `None` is every lane.
fn lanes_for(
    policy: RipplePolicy,
    selected: impl Iterator<Item = String>,
) -> Option<HashSet<String>> {
    match policy {
        RipplePolicy::AllTracks => None,
        _ => Some(selected.collect()),
    }
}

/// Clips linked to any of `ids`, in either direction, excluding `ids`.
fn linked_partners(timeline: &Timeline, ids: &[String]) -> Vec<String> {
    timeline
        .clips
        .iter()
        .filter(|clip| !ids.contains(&clip.id))
        .filter(|clip| {
            clip.detached_from
                .as_ref()
                .is_some_and(|parent| ids.contains(parent))
                || ids.iter().any(|id| {
                    timeline
                        .clip(id)
                        .and_then(|c| c.detached_from.as_ref())
                        .is_some_and(|parent| *parent == clip.id)
                })
        })
        .map(|clip| clip.id.clone())
        .collect()
}

/// Moves everything starting at or after `from` by `amount` seconds on the
/// given lanes (all when `None`), skipping `exclude`, under the link and
/// marker policies.
#[allow(clippy::too_many_arguments)]
fn shift_after(
    timeline: &mut Timeline,
    from: f64,
    amount: f64,
    lanes: Option<&HashSet<String>>,
    exclude: &HashSet<String>,
    ripple: RipplePolicy,
    link: LinkPolicy,
    marker: MarkerPolicy,
) {
    if ripple == RipplePolicy::None || amount == 0.0 {
        return;
    }
    const EPSILON: f64 = 1e-9;
    let mut moving: HashSet<String> = timeline
        .clips
        .iter()
        .filter(|clip| !exclude.contains(&clip.id))
        .filter(|clip| lanes.is_none_or(|lanes| lanes.contains(&clip.track_id)))
        .filter(|clip| clip.start >= from - EPSILON)
        .map(|clip| clip.id.clone())
        .collect();
    if link == LinkPolicy::Preserve {
        let ids: Vec<String> = moving.iter().cloned().collect();
        moving.extend(
            linked_partners(timeline, &ids)
                .into_iter()
                .filter(|id| !exclude.contains(id)),
        );
    }
    if marker == MarkerPolicy::Fixed {
        for m in &mut timeline.markers {
            if m.clip_id.as_ref().is_some_and(|id| moving.contains(id)) {
                m.clip_id = None;
                m.offset = 0.0;
            }
        }
    }
    for clip in &mut timeline.clips {
        if moving.contains(&clip.id) {
            clip.start = (clip.start + amount).max(0.0);
        }
    }
    if marker == MarkerPolicy::ShiftFree {
        for m in timeline.markers.iter_mut().filter(|m| m.clip_id.is_none()) {
            if m.time >= from - EPSILON {
                m.time = (m.time + amount).max(0.0);
            }
        }
    }
}

/// Assigns `value` into `slot`, reporting whether that changed anything.
/// This is how command arms notice a field being set to the value it already
/// holds - which must count as "nothing happened" ([`Outcome::applied`]
/// false), or the undo history would record phantom edits.
fn assign<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

/// Applies one command. Errors are [`CommandError`]s, each rendering as a
/// user-meaningful sentence; a command that legitimately does nothing (a
/// no-op rename, an out-of-range split) returns Ok with no created id and
/// [`Outcome::applied`] false.
pub fn apply(
    project: &mut Project,
    mint: &mut IdMint,
    command: Command,
) -> Result<Outcome, CommandError> {
    let outcome = apply_command(project, mint, command)?;
    for timeline in &mut project.timelines {
        sync_markers(timeline);
    }
    Ok(outcome)
}

/// Keeps attached markers on their clips: the time is the clip's start plus
/// the offset, so any command that moved or resized a clip carries its
/// markers without knowing they exist. A marker whose clip is gone is let
/// go where it last stood.
fn sync_markers(timeline: &mut Timeline) {
    let clips: std::collections::HashMap<&str, (f64, f64)> = timeline
        .clips
        .iter()
        .map(|clip| (clip.id.as_str(), (clip.start, clip.duration)))
        .collect();
    for marker in &mut timeline.markers {
        let Some(id) = marker.clip_id.as_deref() else {
            continue;
        };
        match clips.get(id) {
            Some(&(start, duration)) => {
                marker.offset = marker.offset.clamp(0.0, duration);
                marker.time = start + marker.offset;
            }
            None => {
                marker.clip_id = None;
                marker.offset = 0.0;
            }
        }
    }
}

fn apply_command(
    project: &mut Project,
    mint: &mut IdMint,
    command: Command,
) -> Result<Outcome, CommandError> {
    match command {
        Command::AddMedia { item } => {
            if project
                .media
                .iter()
                .any(|existing| existing.path == item.path)
            {
                return Ok(Outcome::default());
            }
            let id = mint.next("m");
            project.media.push(MediaItem {
                id: id.clone(),
                path: item.path,
                name: item.name,
                duration: item.duration,
                kind: item.kind,
                width: item.width,
                height: item.height,
                frame_rate: item.frame_rate,
                frame_rate_fraction: item.frame_rate_fraction,
                video_codec: item.video_codec,
                audio_codec: item.audio_codec,
                has_audio: item.has_audio,
                placeholder: false,
            });
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::SetMediaPlaceholder {
            media_id,
            placeholder,
        } => {
            let applied = project
                .media
                .iter_mut()
                .find(|item| item.id == media_id)
                .is_some_and(|item| assign(&mut item.placeholder, placeholder));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::FillSlot { media_id, item } => {
            let media = project
                .media
                .iter_mut()
                .find(|existing| existing.id == media_id)
                .ok_or(CommandError::SlotGone)?;
            if !media.placeholder {
                return Err(CommandError::NotASlot);
            }

            // The slot keeps its id, so every clip that references it keeps
            // working; only the identity behind the id changes.
            media.path = item.path;
            media.name = item.name.clone();
            media.duration = item.duration;
            media.kind = item.kind;
            media.width = item.width;
            media.height = item.height;
            media.frame_rate = item.frame_rate;
            media.frame_rate_fraction = item.frame_rate_fraction;
            media.video_codec = item.video_codec;
            media.audio_codec = item.audio_codec;
            media.has_audio = item.has_audio;
            media.placeholder = false;
            let kind = match item.kind {
                MediaKind::Video => ClipKind::Video,
                MediaKind::Audio => ClipKind::Audio,
                MediaKind::Image => ClipKind::Image,
            };

            // Slot timing is the template's: start, duration and speed stay
            // put, which is what keeps cuts on the beat. The in-point resets
            // because it referred to the old footage; a clip shorter than its
            // slot freeze-frames on its last frame downstream, which is the
            // renderer's existing behaviour for a trim past the media's end.
            // All timelines, like RemoveMedia: slots are not per-timeline.
            for timeline in &mut project.timelines {
                for clip in &mut timeline.clips {
                    if clip.media_id == media_id {
                        clip.source_start = 0.0;
                        clip.kind = kind;
                        clip.name = item.name.clone();
                    }
                }
            }
            // Filling always changes the project: the target was a
            // placeholder and is one no longer.
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::Batch { commands } => {
            // All or nothing: apply to a staged copy and commit only a fully
            // successful run, so one bad command cannot leave a half-applied
            // batch behind (and the editor records it as one undo step).
            let mut staged = project.clone();
            let mut created = None;
            let mut applied = false;
            for command in commands {
                let outcome = apply(&mut staged, mint, command)?;
                if outcome.created_id.is_some() {
                    created = outcome.created_id;
                }
                applied |= outcome.applied;
            }
            *project = staged;
            Ok(Outcome {
                created_id: created,
                applied,
            })
        }

        Command::RemoveMedia { media_id } => {
            // All timelines, not just the active one: a shelved clip whose
            // media is gone would linger as a dead reference.
            let media_count = project.media.len();
            project.media.retain(|item| item.id != media_id);
            let mut applied = project.media.len() != media_count;
            for timeline in &mut project.timelines {
                let clip_count = timeline.clips.len();
                timeline.clips.retain(|clip| clip.media_id != media_id);
                applied |= timeline.clips.len() != clip_count;
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::AddClip {
            media_id,
            track_id,
            start,
        } => {
            let media = project
                .media_by_id(&media_id)
                .ok_or(CommandError::MediaGone)?
                .clone();
            let timeline = project.active_mut();
            if timeline.track(&track_id).is_none() {
                return Err(CommandError::TrackGone);
            }
            let id = mint.next("c");
            timeline
                .clips
                .push(default_clip(id.clone(), track_id, &media, start));
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::AddClipAtFirstFree { media_id, start } => {
            let media = project
                .media_by_id(&media_id)
                .ok_or(CommandError::MediaGone)?
                .clone();
            let duration = match media.kind {
                MediaKind::Image => DEFAULT_IMAGE_DURATION,
                _ => media.duration.unwrap_or(UNKNOWN_DURATION),
            };
            let timeline = project.active_mut();
            let track_id =
                first_free_track(timeline, start, duration).ok_or(CommandError::NoTracks)?;
            let id = mint.next("c");
            timeline
                .clips
                .push(default_clip(id.clone(), track_id, &media, start));
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::AddFadeToBlack {
            media_id,
            track_id,
            duration,
            transition_duration,
            shift_all_tracks,
            shift_free_markers,
        } => {
            let media = project
                .media_by_id(&media_id)
                .ok_or(CommandError::MediaGone)?
                .clone();
            if media.kind == MediaKind::Audio {
                return Err(CommandError::MediaGone);
            }
            let timeline = project.active_mut();
            if timeline.track(&track_id).is_none() {
                return Err(CommandError::TrackGone);
            }
            if timeline.clips.iter().any(|clip| {
                clip.track_id == track_id
                    && (clip.name == "Fade from Black" || clip.name == "Fade to Black")
            }) {
                return Ok(Outcome::default());
            }

            let mut content: Vec<(String, f64, f64)> = timeline
                .clips
                .iter()
                .filter(|clip| clip.track_id == track_id)
                .map(|clip| (clip.id.clone(), clip.start, clip.duration))
                .collect();
            if content.is_empty() {
                return Err(CommandError::FadeTrackEmpty);
            }
            content.sort_by(|left, right| left.1.total_cmp(&right.1));
            if content[0].1.abs() > JOIN_EPSILON {
                return Err(CommandError::FadeTrackDoesNotStartAtZero);
            }

            let duration = duration.max(MIN_CLIP_DURATION);
            let transition_duration = transition_duration.max(0.0).min(duration).min(content[0].2);
            let first_id = content[0].0.clone();
            let content_end = content
                .iter()
                .map(|(_, start, length)| start + length)
                .fold(0.0, f64::max);

            for clip in &mut timeline.clips {
                if shift_all_tracks || clip.track_id == track_id {
                    clip.start += duration;
                }
            }
            if shift_free_markers {
                for marker in &mut timeline.markers {
                    if marker.clip_id.is_none() {
                        marker.time += duration;
                    }
                }
            }

            let mut intro = default_clip(mint.next("c"), track_id.clone(), &media, 0.0);
            intro.name = "Fade from Black".to_owned();
            intro.duration = duration;
            intro.volume = 0.0;
            let mut outro = default_clip(mint.next("c"), track_id, &media, content_end + duration);
            outro.name = "Fade to Black".to_owned();
            outro.duration = duration;
            outro.volume = 0.0;
            outro.transition_in = Some(Transition {
                id: "cross-fade".to_owned(),
                duration: transition_duration,
            });
            let outro_id = outro.id.clone();
            timeline.clips.push(intro);
            timeline.clips.push(outro);
            if let Some(first) = timeline.clip_mut(&first_id) {
                first.transition_in = Some(Transition {
                    id: "cross-fade".to_owned(),
                    duration: transition_duration,
                });
            }
            Ok(Outcome {
                created_id: Some(outro_id),
                applied: true,
            })
        }

        Command::AddTextClip {
            track_id,
            start,
            style,
            duration,
            offset_y,
        } => {
            let style = style.unwrap_or_default();
            let duration = duration
                .unwrap_or(DEFAULT_TEXT_DURATION)
                .max(MIN_CLIP_DURATION);
            let timeline = project.active_mut();
            let track_id = match track_id {
                Some(id) if timeline.track(&id).is_some() => id,
                Some(_) => return Err(CommandError::TrackGone),
                None => {
                    first_free_track(timeline, start, duration).ok_or(CommandError::NoTracks)?
                }
            };
            let id = mint.next("c");
            timeline.clips.push(Clip {
                id: id.clone(),
                track_id,
                media_id: String::new(),
                name: first_line(&style.content),
                kind: ClipKind::Text,
                start: start.max(0.0),
                duration,
                source_start: 0.0,
                volume: 1.0,
                fade_in: 0.0,
                fade_out: 0.0,
                scale: 1.0,
                offset_x: 0.0,
                offset_y: offset_y.unwrap_or(0.0).clamp(-MAX_OFFSET, MAX_OFFSET),
                rotation: 0.0,
                opacity: 1.0,
                speed: 1.0,
                preserve_pitch: true,
                speed_curve: None,
                reverse: false,
                animation_in: None,
                animation_out: None,
                animation_combo: None,
                flip_h: false,
                flip_v: false,
                blend: String::new(),
                crop: None,
                filters: Vec::new(),
                video_effects: Vec::new(),
                muted: None,
                detached_from: None,
                transition_in: None,
                text: Some(style),
            });
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::AddLayerClip {
            track_id,
            start,
            duration,
            effect_id,
            name,
        } => {
            let duration = duration
                .unwrap_or(DEFAULT_LAYER_DURATION)
                .max(MIN_CLIP_DURATION);
            let timeline = project.active_mut();
            let track_id = match track_id {
                Some(id) if timeline.track(&id).is_some() => id,
                Some(_) => return Err(CommandError::TrackGone),
                None => {
                    first_free_track(timeline, start, duration).ok_or(CommandError::NoTracks)?
                }
            };
            let id = mint.next("c");
            timeline.clips.push(Clip {
                id: id.clone(),
                track_id,
                media_id: String::new(),
                name: if name.trim().is_empty() {
                    effect_id.clone()
                } else {
                    name
                },
                kind: ClipKind::Layer,
                start: start.max(0.0),
                duration,
                source_start: 0.0,
                volume: 1.0,
                fade_in: 0.0,
                fade_out: 0.0,
                scale: 1.0,
                offset_x: 0.0,
                offset_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                speed: 1.0,
                preserve_pitch: true,
                speed_curve: None,
                reverse: false,
                animation_in: None,
                animation_out: None,
                animation_combo: None,
                flip_h: false,
                flip_v: false,
                blend: String::new(),
                crop: None,
                filters: Vec::new(),
                video_effects: vec![AppliedFilter {
                    id: effect_id,
                    params: Default::default(),
                    enabled: true,
                }],
                muted: None,
                detached_from: None,
                transition_in: None,
                text: None,
            });
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::MoveClips { moves } => {
            let timeline = project.active_mut();
            let track_ids: HashSet<String> = timeline
                .tracks
                .iter()
                .map(|track| track.id.clone())
                .collect();
            let mut applied = false;
            for wanted in moves {
                if let Some(clip) = timeline.clip_mut(&wanted.clip_id) {
                    applied |= assign(&mut clip.start, wanted.start.max(0.0));
                    if track_ids.contains(&wanted.track_id) {
                        applied |= assign(&mut clip.track_id, wanted.track_id);
                    }
                }
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::TrimClip {
            clip_id,
            edge,
            delta,
        } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip_mut(&clip_id) else {
                return Ok(Outcome::default());
            };
            let mut head_shift = 0.0;
            let applied = match edge {
                TrimEdge::End => {
                    let duration = (clip.duration + delta).max(MIN_CLIP_DURATION);
                    assign(&mut clip.duration, duration)
                }
                TrimEdge::Start => {
                    // Dragging the head moves the in-point too, so the pixels
                    // under the remaining part of the clip do not slide.
                    let shift = delta.min(clip.duration - MIN_CLIP_DURATION);
                    let start = (clip.start + shift).max(0.0);
                    let moved = start - clip.start;
                    let duration = clip.duration - moved;
                    let source_start = (clip.source_start + moved * clip.speed).max(0.0);
                    head_shift = moved;
                    // Bitwise so no assignment is short-circuited away.
                    assign(&mut clip.start, start)
                        | assign(&mut clip.duration, duration)
                        | assign(&mut clip.source_start, source_start)
                }
            };
            // Markers keep their place on the footage when the head moves;
            // one the head passes is let go where it stood.
            if head_shift != 0.0 {
                for marker in &mut timeline.markers {
                    if marker.clip_id.as_deref() == Some(clip_id.as_str()) {
                        marker.offset -= head_shift;
                        if marker.offset < 0.0 {
                            marker.clip_id = None;
                            marker.offset = 0.0;
                        }
                    }
                }
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SplitClips { clip_ids, time } => {
            let timeline = project.active_mut();
            let mut created = None;
            for clip_id in clip_ids {
                let Some(index) = timeline.clips.iter().position(|clip| clip.id == clip_id) else {
                    continue;
                };
                {
                    // A curve or a reverse does not survive a cut in halves:
                    // the map from here to the source is not affine, so both
                    // halves go to the constant mean, which is what they
                    // averaged.
                    let clip = &mut timeline.clips[index];
                    let offset = time - clip.start;
                    if offset > MIN_CLIP_DURATION
                        && offset < clip.duration - MIN_CLIP_DURATION
                        && (clip.speed_curve.is_some() || clip.reverse)
                    {
                        clip.speed_curve = None;
                        clip.reverse = false;
                    }
                }
                let clip = &timeline.clips[index];
                let offset = time - clip.start;
                if offset <= MIN_CLIP_DURATION || offset >= clip.duration - MIN_CLIP_DURATION {
                    continue;
                }
                let mut tail = clip.clone();
                tail.id = mint.next("c");
                tail.start = clip.start + offset;
                tail.duration = clip.duration - offset;
                tail.source_start = clip.source_start + offset * clip.speed;
                // The transition belongs to the cut at the original clip's
                // start, which the head keeps.
                tail.transition_in = None;
                created = Some(tail.id.clone());
                timeline.clips[index].duration = offset;
                // Markers past the cut ride the tail from here on.
                let head_id = timeline.clips[index].id.clone();
                for marker in &mut timeline.markers {
                    if marker.clip_id.as_deref() == Some(head_id.as_str())
                        && marker.offset >= offset
                    {
                        marker.clip_id = Some(tail.id.clone());
                        marker.offset -= offset;
                    }
                }
                timeline.clips.insert(index + 1, tail);
            }
            // A split always mints the tail, so "minted anything" and
            // "changed anything" are the same fact here.
            let applied = created.is_some();
            Ok(Outcome {
                created_id: created,
                applied,
            })
        }

        Command::MergeClips { clip_ids } => {
            let timeline = project.active_mut();
            if let Some(reason) = why_not_merge(timeline, &clip_ids) {
                return Err(CommandError::CannotMerge { reason });
            }
            let mut ordered: Vec<Clip> = clip_ids
                .iter()
                .filter_map(|id| timeline.clip(id).cloned())
                .collect();
            ordered.sort_by(|left, right| left.start.total_cmp(&right.start));
            let first = ordered.first().expect("validated above").clone();
            let last = ordered.last().expect("validated above");
            let merged_duration = last.start + last.duration - first.start;

            // A set, not a Vec: the retain below tests every clip on the
            // timeline against it.
            let doomed: HashSet<String> =
                ordered.iter().skip(1).map(|clip| clip.id.clone()).collect();
            // Markers on an absorbed piece move to the survivor, keeping
            // their place on the timeline.
            for piece in ordered.iter().skip(1) {
                for marker in &mut timeline.markers {
                    if marker.clip_id.as_deref() == Some(piece.id.as_str()) {
                        marker.clip_id = Some(first.id.clone());
                        marker.offset += piece.start - first.start;
                    }
                }
            }
            timeline.clips.retain(|clip| !doomed.contains(&clip.id));
            let survivor = timeline
                .clip_mut(&first.id)
                .expect("the first piece survives the retain");
            survivor.duration = merged_duration;
            // A validated merge always absorbs at least one piece.
            Ok(Outcome {
                created_id: Some(first.id),
                applied: true,
            })
        }

        Command::RemoveClips { clip_ids } => {
            let timeline = project.active_mut();
            let doomed: HashSet<&str> = clip_ids.iter().map(String::as_str).collect();
            let clip_count = timeline.clips.len();
            timeline
                .clips
                .retain(|clip| !doomed.contains(clip.id.as_str()));
            let applied = timeline.clips.len() != clip_count;
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::UpdateClip { clip_id, patch } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip_mut(&clip_id) else {
                return Ok(Outcome::default());
            };
            let mut applied = false;
            if let Some(name) = patch.name {
                applied |= assign(&mut clip.name, name);
            }
            if let Some(volume) = patch.volume {
                applied |= assign(&mut clip.volume, volume.max(0.0));
            }
            if let Some(fade_in) = patch.fade_in {
                applied |= assign(&mut clip.fade_in, fade_in.max(0.0));
            }
            if let Some(fade_out) = patch.fade_out {
                applied |= assign(&mut clip.fade_out, fade_out.max(0.0));
            }
            if let Some(opacity) = patch.opacity {
                applied |= assign(&mut clip.opacity, opacity.clamp(0.0, 1.0));
            }
            if let Some(preserve) = patch.preserve_pitch {
                applied |= assign(&mut clip.preserve_pitch, preserve);
            }
            if let Some(reverse) = patch.reverse {
                applied |= assign(&mut clip.reverse, reverse);
            }
            if let Some(flip) = patch.flip_h {
                applied |= assign(&mut clip.flip_h, flip);
            }
            if let Some(flip) = patch.flip_v {
                applied |= assign(&mut clip.flip_v, flip);
            }
            if let Some(blend) = patch.blend {
                let blend = if blend == "normal" {
                    String::new()
                } else {
                    blend
                };
                applied |= assign(&mut clip.blend, blend);
            }
            if let Some(crop) = patch.crop {
                let crop = crop.map(Crop::tidy).filter(|crop| !crop.is_none());
                applied |= assign(&mut clip.crop, crop);
            }
            if let Some(filters) = patch.filters {
                applied |= assign(&mut clip.filters, filters);
            }
            if let Some(effects) = patch.video_effects {
                applied |= assign(&mut clip.video_effects, effects);
            }
            if let Some(transition) = patch.transition_in {
                applied |= assign(&mut clip.transition_in, transition);
            }
            if let Some(text) = patch.text {
                // The name follows the words, like addTextClip snapshots it.
                if let Some(style) = &text {
                    applied |= assign(&mut clip.name, first_line(&style.content));
                }
                applied |= assign(&mut clip.text, text);
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetClipSpeed { clip_id, speed } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip_mut(&clip_id) else {
                return Ok(Outcome::default());
            };
            // The amount of source covered is held constant - that is what
            // makes this a speed change rather than a trim.
            let next = speed.clamp(MIN_SPEED, MAX_SPEED);
            let source_covered = clip.duration * clip.speed;
            // Bitwise so no assignment is short-circuited away. A rate set
            // by hand is a constant rate: the curve goes.
            let applied = assign(&mut clip.speed, next)
                | assign(
                    &mut clip.duration,
                    (source_covered / next).max(MIN_CLIP_DURATION),
                )
                | assign(&mut clip.speed_curve, None);
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetClipAnimation {
            clip_id,
            slot,
            animation,
        } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip_mut(&clip_id) else {
                return Ok(Outcome::default());
            };
            let animation = animation
                .filter(|set| crate::animation::index_of(slot, &set.preset).is_some())
                .map(|set| ClipAnimation {
                    preset: set.preset,
                    duration: set.duration.clamp(0.05, 60.0),
                });
            let field = match slot {
                AnimationSlot::In => &mut clip.animation_in,
                AnimationSlot::Out => &mut clip.animation_out,
                AnimationSlot::Combo => &mut clip.animation_combo,
            };
            let applied = assign(field, animation);
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetClipSpeedCurve { clip_id, curve } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip_mut(&clip_id) else {
                return Ok(Outcome::default());
            };
            let curve = curve.filter(|points| crate::speed::curve_of(points).is_some());
            let source_covered = clip.duration * clip.speed;
            let mean = curve
                .as_ref()
                .map(|points| crate::speed::mean_of(points))
                .unwrap_or(clip.speed)
                .clamp(MIN_SPEED, MAX_SPEED);
            let applied = assign(&mut clip.speed_curve, curve)
                | assign(&mut clip.speed, mean)
                | assign(
                    &mut clip.duration,
                    (source_covered / mean).max(MIN_CLIP_DURATION),
                );
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetClipTransform {
            clip_id,
            scale,
            offset_x,
            offset_y,
            rotation,
        } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip_mut(&clip_id) else {
                return Ok(Outcome::default());
            };
            let mut applied = false;
            if let Some(scale) = scale {
                applied |= assign(&mut clip.scale, scale.clamp(MIN_SCALE, MAX_SCALE));
            }
            if let Some(offset) = offset_x {
                applied |= assign(&mut clip.offset_x, offset.clamp(-MAX_OFFSET, MAX_OFFSET));
            }
            if let Some(offset) = offset_y {
                applied |= assign(&mut clip.offset_y, offset.clamp(-MAX_OFFSET, MAX_OFFSET));
            }
            if let Some(rotation) = rotation {
                // Kept in (-180, 180] so a full drag never accumulates turns.
                let wrapped = ((rotation % 360.0) + 540.0) % 360.0 - 180.0;
                let next = if wrapped == -180.0 { 180.0 } else { wrapped };
                applied |= assign(&mut clip.rotation, next);
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::DetachAudio { clip_id } => {
            let has_audio = {
                let timeline = project.active();
                let Some(clip) = timeline.clip(&clip_id) else {
                    return Ok(Outcome::default());
                };
                clip.kind == ClipKind::Video
                    && clip.muted != Some(true)
                    && project
                        .media_by_id(&clip.media_id)
                        .is_some_and(|media| media.has_audio)
                    && !timeline
                        .clips
                        .iter()
                        .any(|other| other.detached_from.as_deref() == Some(clip_id.as_str()))
            };
            if !has_audio {
                return Ok(Outcome::default());
            }

            let timeline = project.active_mut();
            let clip = timeline.clip(&clip_id).expect("checked above").clone();
            // A lane free for the whole span, or a fresh one.
            let track_id = {
                let end = clip.start + clip.duration;
                let free = timeline.tracks.iter().find(|track| {
                    !timeline.clips.iter().any(|other| {
                        other.track_id == track.id
                            && other.start < end
                            && clip.start < other.start + other.duration
                    })
                });
                match free {
                    Some(track) => track.id.clone(),
                    None => {
                        let id = mint.next("t");
                        let name = next_numbered(
                            "Track",
                            timeline.tracks.iter().map(|track| track.name.clone()),
                        );
                        timeline.tracks.push(Track {
                            id: id.clone(),
                            name,
                            visible: true,
                            muted: false,
                        });
                        id
                    }
                }
            };

            let mut sound = clip.clone();
            sound.id = mint.next("c");
            sound.track_id = track_id;
            sound.kind = ClipKind::Audio;
            sound.video_effects = Vec::new();
            sound.transition_in = None;
            sound.detached_from = Some(clip_id.clone());
            sound.muted = None;
            let sound_id = sound.id.clone();
            timeline.clips.push(sound);

            let video = timeline.clip_mut(&clip_id).expect("still present");
            video.muted = Some(true);
            video.filters = Vec::new();
            Ok(Outcome {
                created_id: Some(sound_id),
                applied: true,
            })
        }

        Command::ReattachAudio { clip_id } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip(&clip_id) else {
                return Ok(Outcome::default());
            };
            let video_id = match (&clip.kind, &clip.detached_from) {
                (ClipKind::Audio, Some(source)) => source.clone(),
                _ => clip.id.clone(),
            };
            if timeline.clip(&video_id).is_none() {
                return Ok(Outcome::default());
            }
            let sounds: Vec<Clip> = timeline
                .clips
                .iter()
                .filter(|other| other.detached_from.as_deref() == Some(video_id.as_str()))
                .cloned()
                .collect();
            if sounds.is_empty() {
                return Ok(Outcome::default());
            }
            // A set, not a Vec: the retain below tests every clip on the
            // timeline against it.
            let doomed: HashSet<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
            timeline.clips.retain(|other| !doomed.contains(&other.id));
            let video = timeline.clip_mut(&video_id).expect("checked above");
            video.muted = None;
            video.filters = sounds[0].filters.clone();
            // Reaching here means at least one sound clip was deleted.
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::AddTrack => {
            let timeline = project.active_mut();
            let id = mint.next("t");
            let name = next_numbered(
                "Track",
                timeline.tracks.iter().map(|track| track.name.clone()),
            );
            timeline.tracks.push(Track {
                id: id.clone(),
                name,
                visible: true,
                muted: false,
            });
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::RemoveTrack { track_id } => {
            let timeline = project.active_mut();
            if timeline.tracks.len() <= 1 {
                return Err(CommandError::LastTrack);
            }
            let track_count = timeline.tracks.len();
            timeline.tracks.retain(|track| track.id != track_id);
            timeline.clips.retain(|clip| clip.track_id != track_id);
            // Clips only ever sit on existing tracks, so an unknown id - the
            // tolerated no-op - removes neither.
            let applied = timeline.tracks.len() != track_count;
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::RenameTrack { track_id, name } => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Ok(Outcome::default());
            }
            let timeline = project.active_mut();
            let applied = timeline
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .is_some_and(|track| assign(&mut track.name, trimmed.to_owned()));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetTrackFlag {
            track_id,
            flag,
            value,
        } => {
            let timeline = project.active_mut();
            let applied = timeline
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .is_some_and(|track| match flag {
                    TrackFlag::Visible => assign(&mut track.visible, value),
                    TrackFlag::Muted => assign(&mut track.muted, value),
                });
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::AddMarker {
            time,
            name,
            clip_id,
        } => {
            let timeline = project.active_mut();
            let id = mint.next("mk");
            let name = name
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    next_numbered(
                        "Marker",
                        timeline.markers.iter().map(|marker| marker.name.clone()),
                    )
                });
            let time = time.max(0.0);
            let (clip_id, offset) = match clip_id.and_then(|id| timeline.clip(&id)) {
                Some(clip) => (
                    Some(clip.id.clone()),
                    (time - clip.start).clamp(0.0, clip.duration),
                ),
                None => (None, 0.0),
            };
            timeline.markers.push(Marker {
                id: id.clone(),
                name,
                time,
                clip_id,
                offset,
            });
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::AttachMarker { marker_id, clip_id } => {
            let timeline = project.active_mut();
            let target = clip_id
                .and_then(|id| timeline.clip(&id))
                .map(|clip| (clip.id.clone(), clip.start, clip.duration));
            let Some(marker) = timeline.marker_mut(&marker_id) else {
                return Ok(Outcome::default());
            };
            let (clip_id, offset) = match target {
                Some((id, start, duration)) => {
                    (Some(id), (marker.time - start).clamp(0.0, duration))
                }
                None => (None, 0.0),
            };
            // Bitwise so neither assignment is short-circuited away.
            let applied = assign(&mut marker.clip_id, clip_id) | assign(&mut marker.offset, offset);
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::RippleDelete {
            clip_ids,
            ripple_policy,
            link_policy,
            marker_policy,
        } => {
            let timeline = project.active_mut();
            let mut doomed: HashSet<String> = clip_ids
                .into_iter()
                .filter(|id| timeline.clip(id).is_some())
                .collect();
            if doomed.is_empty() {
                return Ok(Outcome::default());
            }
            let children: Vec<String> = timeline
                .clips
                .iter()
                .filter(|clip| {
                    clip.detached_from
                        .as_ref()
                        .is_some_and(|parent| doomed.contains(parent))
                })
                .map(|clip| clip.id.clone())
                .collect();
            match link_policy {
                LinkPolicy::Preserve => doomed.extend(children),
                LinkPolicy::Detach => {
                    for clip in &mut timeline.clips {
                        if children.contains(&clip.id) && !doomed.contains(&clip.id) {
                            clip.detached_from = None;
                        }
                    }
                }
                LinkPolicy::Explicit => {}
            }

            // The deleted spans, merged, and the lanes that close up.
            let mut spans: Vec<(f64, f64)> = timeline
                .clips
                .iter()
                .filter(|clip| doomed.contains(&clip.id))
                .map(|clip| (clip.start, clip.start + clip.duration))
                .collect();
            let lanes: HashSet<String> = timeline
                .clips
                .iter()
                .filter(|clip| doomed.contains(&clip.id))
                .map(|clip| clip.track_id.clone())
                .collect();
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f64, f64)> = Vec::new();
            for span in spans {
                match merged.last_mut() {
                    Some(last) if span.0 <= last.1 => last.1 = last.1.max(span.1),
                    _ => merged.push(span),
                }
            }
            let covered = |time: f64| -> f64 {
                merged
                    .iter()
                    .map(|(start, end)| (time.min(*end) - start).max(0.0))
                    .sum()
            };

            timeline.clips.retain(|clip| !doomed.contains(&clip.id));
            if ripple_policy != RipplePolicy::None {
                let on_lane = |clip: &Clip| match ripple_policy {
                    RipplePolicy::AllTracks => true,
                    _ => lanes.contains(&clip.track_id),
                };
                let mut shifts: HashMap<String, f64> = timeline
                    .clips
                    .iter()
                    .filter(|clip| on_lane(clip))
                    .map(|clip| (clip.id.clone(), covered(clip.start)))
                    .collect();
                if link_policy == LinkPolicy::Preserve {
                    let partners: Vec<(String, f64)> = timeline
                        .clips
                        .iter()
                        .filter_map(|clip| {
                            let parent = clip.detached_from.as_ref()?;
                            if shifts.contains_key(&clip.id) {
                                return None;
                            }
                            shifts.get(parent).map(|shift| (clip.id.clone(), *shift))
                        })
                        .chain(timeline.clips.iter().filter_map(|clip| {
                            let child = timeline.clips.iter().find(|other| {
                                other.detached_from.as_deref() == Some(clip.id.as_str())
                            })?;
                            if shifts.contains_key(&child.id) {
                                return None;
                            }
                            shifts.get(&clip.id).map(|shift| (child.id.clone(), *shift))
                        }))
                        .collect();
                    shifts.extend(partners);
                }
                if marker_policy == MarkerPolicy::Fixed {
                    for marker in &mut timeline.markers {
                        if marker
                            .clip_id
                            .as_ref()
                            .is_some_and(|id| shifts.get(id).is_some_and(|s| *s > 0.0))
                        {
                            marker.clip_id = None;
                            marker.offset = 0.0;
                        }
                    }
                }
                for clip in &mut timeline.clips {
                    if let Some(shift) = shifts.get(&clip.id) {
                        clip.start = (clip.start - shift).max(0.0);
                    }
                }
                if marker_policy == MarkerPolicy::ShiftFree {
                    for marker in timeline.markers.iter_mut().filter(|m| m.clip_id.is_none()) {
                        marker.time = (marker.time - covered(marker.time)).max(0.0);
                    }
                }
            }
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::RippleDeleteRange {
            start,
            end,
            track_ids,
            marker_policy,
        } => {
            let start = start.max(0.0);
            let length = end - start;
            if !length.is_finite() || length <= MIN_CLIP_DURATION {
                return Ok(Outcome::default());
            }
            let targeted =
                |track_id: &str| track_ids.is_empty() || track_ids.iter().any(|id| id == track_id);

            // Split at the far edge first, so the id that stays behind keeps
            // the head and the one minted is what comes after the cut.
            let mut after: HashMap<String, String> = HashMap::new();
            let mut applied = false;
            for time in [end, start] {
                let straddling: Vec<String> = project
                    .active()
                    .clips
                    .iter()
                    .filter(|clip| {
                        targeted(&clip.track_id)
                            && clip.start < time - MIN_CLIP_DURATION
                            && clip.start + clip.duration > time + MIN_CLIP_DURATION
                    })
                    .map(|clip| clip.id.clone())
                    .collect();
                for id in straddling {
                    let outcome = apply_command(
                        project,
                        mint,
                        Command::SplitClips {
                            clip_ids: vec![id.clone()],
                            time,
                        },
                    )?;
                    applied |= outcome.applied;
                    if let (true, Some(tail)) = (time == end, outcome.created_id) {
                        after.insert(id, tail);
                    }
                }
            }
            // A split clones its piece, so the audio that follows a video
            // clip across the cut would still name the video *before* it as
            // its parent. Point each such piece at its own picture.
            let pieces: HashSet<String> = after.values().cloned().collect();
            for clip in &mut project.active_mut().clips {
                if !pieces.contains(&clip.id) {
                    continue;
                }
                if let Some(tail) = clip
                    .detached_from
                    .as_ref()
                    .and_then(|parent| after.get(parent))
                {
                    clip.detached_from = Some(tail.clone());
                }
            }

            let doomed: Vec<String> = project
                .active()
                .clips
                .iter()
                .filter(|clip| {
                    targeted(&clip.track_id)
                        && clip.start >= start - MIN_CLIP_DURATION
                        && clip.start + clip.duration <= end + MIN_CLIP_DURATION
                })
                .map(|clip| clip.id.clone())
                .collect();
            if !doomed.is_empty() {
                applied |= apply_command(
                    project,
                    mint,
                    Command::RippleDelete {
                        clip_ids: doomed,
                        ripple_policy: RipplePolicy::None,
                        link_policy: LinkPolicy::Explicit,
                        marker_policy: MarkerPolicy::AttachedOnly,
                    },
                )?
                .applied;
            }

            // Close the span by its full length, not by what the deleted
            // clips happened to cover: a stretch with nothing on any lane
            // is still part of the cut.
            let timeline = project.active_mut();
            for clip in &mut timeline.clips {
                if targeted(&clip.track_id) && clip.start >= end - MIN_CLIP_DURATION {
                    clip.start = (clip.start - length).max(0.0);
                    applied = true;
                }
            }
            if marker_policy == MarkerPolicy::ShiftFree {
                for marker in timeline.markers.iter_mut().filter(|m| m.clip_id.is_none()) {
                    if marker.time >= end {
                        marker.time -= length;
                    } else if marker.time > start {
                        marker.time = start;
                    }
                }
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::RippleTrim {
            clip_id,
            edge,
            delta,
            ripple_policy,
            link_policy,
            marker_policy,
        } => {
            let timeline = project.active().clone();
            let Some(clip) = timeline.clip(&clip_id) else {
                return Ok(Outcome::default());
            };
            let lane = clip.track_id.clone();
            let old_end = clip.start + clip.duration;
            let mut trimmed = vec![clip_id.clone()];
            if link_policy == LinkPolicy::Preserve {
                trimmed.extend(linked_partners(&timeline, std::slice::from_ref(&clip_id)));
            }
            let mut change = 0.0;
            let mut applied = false;
            for id in &trimmed {
                let Some(before) = project.active().clip(id) else {
                    continue;
                };
                let (start, duration) = (before.start, before.duration);
                applied |= apply_command(
                    project,
                    mint,
                    Command::TrimClip {
                        clip_id: id.clone(),
                        edge,
                        delta,
                    },
                )?
                .applied;
                let after = project.active_mut().clip_mut(id).expect("still there");
                if *id == clip_id {
                    change = after.duration - duration;
                }
                if edge == TrimEdge::Start {
                    after.start = start;
                }
            }
            if applied && change != 0.0 {
                let exclude: HashSet<String> = trimmed.into_iter().collect();
                let lanes = lanes_for(ripple_policy, std::iter::once(lane));
                shift_after(
                    project.active_mut(),
                    old_end,
                    change,
                    lanes.as_ref(),
                    &exclude,
                    ripple_policy,
                    link_policy,
                    marker_policy,
                );
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::RippleInsert {
            media_id,
            track_id,
            start,
            ripple_policy,
            link_policy,
            marker_policy,
        } => {
            let outcome = apply_command(
                project,
                mint,
                Command::AddClip {
                    media_id,
                    track_id: track_id.clone(),
                    start,
                },
            )?;
            let id = outcome.created_id.clone().expect("AddClip mints an id");
            let timeline = project.active_mut();
            let (at, length) = {
                let clip = timeline.clip(&id).expect("just added");
                (clip.start, clip.duration)
            };
            let lanes = lanes_for(ripple_policy, std::iter::once(track_id));
            shift_after(
                timeline,
                at,
                length,
                lanes.as_ref(),
                &HashSet::from([id]),
                ripple_policy,
                link_policy,
                marker_policy,
            );
            Ok(outcome)
        }

        Command::InsertGap {
            at,
            duration,
            track_ids,
            ripple_policy,
            link_policy,
            marker_policy,
        } => {
            if duration <= 0.0 || ripple_policy == RipplePolicy::None {
                return Ok(Outcome::default());
            }
            if ripple_policy == RipplePolicy::SelectedTracks && track_ids.is_empty() {
                return Err(CommandError::Rejected(
                    "name trackIds, or use allTracks, to say which lanes make room".to_owned(),
                ));
            }
            let lanes = lanes_for(ripple_policy, track_ids.into_iter());
            let timeline = project.active_mut();
            let before: Vec<f64> = timeline.clips.iter().map(|clip| clip.start).collect();
            let markers_before: Vec<f64> = timeline.markers.iter().map(|m| m.time).collect();
            shift_after(
                timeline,
                at.max(0.0),
                duration,
                lanes.as_ref(),
                &HashSet::new(),
                ripple_policy,
                link_policy,
                marker_policy,
            );
            let applied = timeline
                .clips
                .iter()
                .zip(before)
                .any(|(clip, start)| clip.start != start)
                || timeline
                    .markers
                    .iter()
                    .zip(markers_before)
                    .any(|(marker, time)| marker.time != time);
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SaveLayoutTemplate { mut layout } => {
            layout.name = layout.name.trim().to_owned();
            if layout.name.is_empty() {
                return Ok(Outcome::default());
            }
            layout.opacity = layout.opacity.clamp(0.0, 1.0);
            match project.layouts.iter_mut().find(|l| l.name == layout.name) {
                Some(existing) if *existing == layout => return Ok(Outcome::default()),
                Some(existing) => *existing = layout,
                None => project.layouts.push(layout),
            }
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::RemoveLayoutTemplate { name } => {
            let count = project.layouts.len();
            project.layouts.retain(|layout| layout.name != name);
            Ok(Outcome {
                created_id: None,
                applied: project.layouts.len() != count,
            })
        }

        Command::ApplyLayoutTemplate { name, clip_ids } => {
            let layout = project
                .layouts
                .iter()
                .find(|layout| layout.name == name)
                .cloned()
                .ok_or_else(|| CommandError::Rejected(format!("no layout named \"{name}\"")))?;
            let mut applied = false;
            for clip in project
                .active_mut()
                .clips
                .iter_mut()
                .filter(|clip| clip_ids.contains(&clip.id))
                .filter(|clip| !matches!(clip.kind, ClipKind::Audio))
            {
                let before = (
                    clip.scale,
                    clip.offset_x,
                    clip.offset_y,
                    clip.rotation,
                    clip.opacity,
                    clip.flip_h,
                    clip.flip_v,
                    clip.blend.clone(),
                    clip.crop,
                );
                clip.scale = layout.scale;
                clip.offset_x = layout.offset_x;
                clip.offset_y = layout.offset_y;
                clip.rotation = layout.rotation;
                clip.opacity = layout.opacity;
                clip.flip_h = layout.flip_h;
                clip.flip_v = layout.flip_v;
                clip.blend = layout.blend.clone();
                clip.crop = layout.crop;
                applied |= before
                    != (
                        clip.scale,
                        clip.offset_x,
                        clip.offset_y,
                        clip.rotation,
                        clip.opacity,
                        clip.flip_h,
                        clip.flip_v,
                        clip.blend.clone(),
                        clip.crop,
                    );
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetMediaPath { media_id, path } => {
            let applied = project
                .media
                .iter_mut()
                .find(|item| item.id == media_id)
                .is_some_and(|item| {
                    let changed = item.path != path;
                    item.path = path;
                    changed
                });
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::RemoveMarker { marker_id } => {
            let timeline = project.active_mut();
            let count = timeline.markers.len();
            timeline.markers.retain(|marker| marker.id != marker_id);
            Ok(Outcome {
                created_id: None,
                applied: timeline.markers.len() != count,
            })
        }

        Command::RenameMarker { marker_id, name } => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Ok(Outcome::default());
            }
            let timeline = project.active_mut();
            let applied = timeline
                .markers
                .iter_mut()
                .find(|marker| marker.id == marker_id)
                .is_some_and(|marker| assign(&mut marker.name, trimmed.to_owned()));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::MoveMarker { marker_id, time } => {
            let timeline = project.active_mut();
            let time = time.max(0.0);
            // An attached marker slides along its clip: the new time becomes
            // an offset, clamped to the clip, and the sync pass derives the
            // time from it.
            let anchor = timeline
                .marker(&marker_id)
                .and_then(|marker| marker.clip_id.as_deref())
                .and_then(|id| timeline.clip(id))
                .map(|clip| (clip.start, clip.duration));
            let Some(marker) = timeline.marker_mut(&marker_id) else {
                return Ok(Outcome::default());
            };
            let applied = match anchor {
                Some((start, duration)) => {
                    let offset = (time - start).clamp(0.0, duration);
                    assign(&mut marker.offset, offset) | assign(&mut marker.time, start + offset)
                }
                None => assign(&mut marker.time, time),
            };
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::AddTimeline => {
            let id = mint.next("tl");
            let name = next_numbered(
                "Timeline",
                project
                    .timelines
                    .iter()
                    .map(|timeline| timeline.name.clone()),
            );
            let tracks = (1..=4)
                .map(|number| Track {
                    id: mint.next("t"),
                    name: format!("Track {number}"),
                    visible: true,
                    muted: false,
                })
                .collect();
            project.timelines.push(Timeline {
                id: id.clone(),
                name,
                tracks,
                clips: Vec::new(),
                markers: Vec::new(),
            });
            project.active_timeline_id = id.clone();
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::RemoveTimeline { timeline_id } => {
            if project.timelines.len() <= 1 {
                return Err(CommandError::LastTimeline);
            }
            let Some(index) = project
                .timelines
                .iter()
                .position(|timeline| timeline.id == timeline_id)
            else {
                return Ok(Outcome::default());
            };
            if project.active_timeline_id == timeline_id {
                let neighbour = if index + 1 < project.timelines.len() {
                    index + 1
                } else {
                    index - 1
                };
                project.active_timeline_id = project.timelines[neighbour].id.clone();
            }
            project.timelines.remove(index);
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::RenameTimeline { timeline_id, name } => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Ok(Outcome::default());
            }
            let applied = project
                .timelines
                .iter_mut()
                .find(|timeline| timeline.id == timeline_id)
                .is_some_and(|timeline| assign(&mut timeline.name, trimmed.to_owned()));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SelectTimeline { timeline_id } => {
            // Short-circuiting is right here: an unknown id must not touch
            // the selection at all.
            let applied = project
                .timelines
                .iter()
                .any(|timeline| timeline.id == timeline_id)
                && assign(&mut project.active_timeline_id, timeline_id);
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::MoveTimeline { timeline_id, index } => {
            let Some(from) = project
                .timelines
                .iter()
                .position(|timeline| timeline.id == timeline_id)
            else {
                return Ok(Outcome::default());
            };
            let to = index.min(project.timelines.len() - 1);
            if to == from {
                return Ok(Outcome::default());
            }
            let timeline = project.timelines.remove(from);
            project.timelines.insert(to, timeline);
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::AddFont { family, path } => {
            if project.fonts.iter().any(|font| font.path == path) {
                return Ok(Outcome::default());
            }
            project.fonts.push(CustomFont { family, path });
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::RemoveFont { family } => {
            // Clips keep the family name: the face may come back when the
            // file does.
            let font_count = project.fonts.len();
            project.fonts.retain(|font| font.family != family);
            let applied = project.fonts.len() != font_count;
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }
    }
}
