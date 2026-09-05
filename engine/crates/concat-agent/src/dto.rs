// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Tool-facing mirrors of `concat_project`'s command payloads.
//!
//! `concat_project`'s types derive `Serialize`/`Deserialize` but not
//! `schemars::JsonSchema`, and should not start: it is the deepest, most
//! shared crate in the workspace, and a leaf concern like agent tooling has
//! no business pushing a dependency up into it. `concat_host` already solved
//! this the same way for its own callers - `ProjectInfo`, `EditorView`,
//! `ExportSpec` are hand-mirrored rather than reused engine-internal types.
//! Every struct here does the same: it exists only to have a schema, and
//! converts into the real command/model type at the call boundary.

use std::collections::BTreeMap;

use concat_project::commands::{self as cmd, Command};
use concat_project::model;
use rmcp::schemars;
use serde::Deserialize;

/// Which edge [`crate::mcp::tools`]'s `trim_clip` drags. Mirrors
/// [`cmd::TrimEdge`].
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TrimEdgeArg {
    /// The head; the in-point moves with the edge.
    Start,
    /// The tail; only the length changes.
    End,
}

impl From<TrimEdgeArg> for cmd::TrimEdge {
    fn from(value: TrimEdgeArg) -> Self {
        match value {
            TrimEdgeArg::Start => cmd::TrimEdge::Start,
            TrimEdgeArg::End => cmd::TrimEdge::End,
        }
    }
}

/// Which per-track toggle `set_track_flag` flips. Mirrors [`cmd::TrackFlag`].
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TrackFlagArg {
    /// Whether the track's video reaches the composite.
    Visible,
    /// Whether the track's audio is silenced.
    Muted,
}

impl From<TrackFlagArg> for cmd::TrackFlag {
    fn from(value: TrackFlagArg) -> Self {
        match value {
            TrackFlagArg::Visible => cmd::TrackFlag::Visible,
            TrackFlagArg::Muted => cmd::TrackFlag::Muted,
        }
    }
}

/// One clip's destination in `move_clips`. Mirrors [`cmd::ClipMove`].
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ClipMoveArg {
    /// The clip to move. An unknown id is skipped, not an error.
    pub clip_id: String,
    /// New timeline position in seconds, floored at 0.
    pub start: f64,
    /// Destination track. An unknown id moves the clip in time but leaves it
    /// on its current track.
    pub track_id: String,
}

impl From<ClipMoveArg> for cmd::ClipMove {
    fn from(value: ClipMoveArg) -> Self {
        cmd::ClipMove {
            clip_id: value.clip_id,
            start: value.start,
            track_id: value.track_id,
        }
    }
}

/// A crop: fractions of the source taken off each edge. Mirrors
/// [`model::Crop`].
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
pub struct CropArg {
    /// Off the left, `0..1`.
    #[serde(default)]
    pub left: f64,
    /// Off the top.
    #[serde(default)]
    pub top: f64,
    /// Off the right.
    #[serde(default)]
    pub right: f64,
    /// Off the bottom.
    #[serde(default)]
    pub bottom: f64,
}

impl From<CropArg> for model::Crop {
    fn from(value: CropArg) -> Self {
        model::Crop {
            left: value.left,
            top: value.top,
            right: value.right,
            bottom: value.bottom,
        }
    }
}

/// One audio filter or video effect. Mirrors [`model::AppliedFilter`].
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct AppliedFilterArg {
    /// The catalogue entry, e.g. "sepia". An id the catalogue no longer
    /// knows is simply skipped at render time.
    pub id: String,
    /// Knob settings, keyed by parameter name. Missing keys mean the
    /// catalogue's defaults.
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// False bypasses without losing settings.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl From<AppliedFilterArg> for model::AppliedFilter {
    fn from(value: AppliedFilterArg) -> Self {
        model::AppliedFilter {
            id: value.id,
            params: value.params,
            enabled: value.enabled,
        }
    }
}

/// A transition on the cut into a clip. Mirrors [`model::Transition`].
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct TransitionArg {
    /// Which transition from the catalogue, e.g. "cross-fade".
    pub id: String,
    /// Seconds the transition covers.
    pub duration: f64,
}

impl From<TransitionArg> for model::Transition {
    fn from(value: TransitionArg) -> Self {
        model::Transition {
            id: value.id,
            duration: value.duration,
        }
    }
}

/// How a title's lines sit within their block. Mirrors [`model::TextAlign`].
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TextAlignArg {
    /// Lines share a left edge.
    Left,
    /// Lines are centred on each other.
    Center,
    /// Lines share a right edge.
    Right,
}

impl From<TextAlignArg> for model::TextAlign {
    fn from(value: TextAlignArg) -> Self {
        match value {
            TextAlignArg::Left => model::TextAlign::Left,
            TextAlignArg::Center => model::TextAlign::Center,
            TextAlignArg::Right => model::TextAlign::Right,
        }
    }
}

/// A title's styling. Mirrors [`model::TextStyle`]; every field matches
/// [`model::TextStyle::default`] when omitted.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct TextStyleArg {
    /// The words themselves, newlines included.
    #[serde(default = "default_content")]
    pub content: String,
    /// CSS-style family name, e.g. `"Cabinet Grotesk"`.
    #[serde(default = "default_font_family")]
    pub font_family: String,
    /// Cap height as a fraction of frame height.
    #[serde(default = "default_font_size")]
    pub font_size: f64,
    /// CSS-scale weight, 100..=900.
    #[serde(default = "default_font_weight")]
    pub font_weight: f64,
    /// Italic when true.
    #[serde(default)]
    pub italic: bool,
    /// Fill colour as a CSS hex string, e.g. "#ffffff".
    #[serde(default = "default_color")]
    pub color: String,
    /// Line alignment within the block.
    #[serde(default = "default_align")]
    pub align: TextAlignArg,
    /// Opacity of the whole title in 0..=1.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// Outline thickness as a fraction of frame height; zero is no stroke.
    #[serde(default)]
    pub stroke_width: f64,
    /// Outline colour, only visible when `stroke_width` is non-zero.
    #[serde(default = "default_stroke_color")]
    pub stroke_color: String,
    /// A drop shadow for legibility over footage.
    #[serde(default = "yes")]
    pub shadow: bool,
    /// A solid plate behind the text; empty string for none.
    #[serde(default)]
    pub background: String,
    /// Baseline spacing as a multiple of the font size.
    #[serde(default = "default_line_height")]
    pub line_height: f64,
    /// Extra letter spacing, in the same frame-height fractions as
    /// `font_size`. Zero is the font's natural fit.
    #[serde(default)]
    pub tracking: f64,
}

fn default_content() -> String {
    "Your text".to_owned()
}
fn default_font_family() -> String {
    "\"Cabinet Grotesk\"".to_owned()
}
fn default_font_size() -> f64 {
    0.09
}
fn default_font_weight() -> f64 {
    700.0
}
fn default_color() -> String {
    "#ffffff".to_owned()
}
fn default_align() -> TextAlignArg {
    TextAlignArg::Center
}
fn default_opacity() -> f64 {
    1.0
}
fn default_stroke_color() -> String {
    "#000000".to_owned()
}
fn default_line_height() -> f64 {
    1.2
}

impl From<TextStyleArg> for model::TextStyle {
    fn from(value: TextStyleArg) -> Self {
        model::TextStyle {
            content: value.content,
            font_family: value.font_family,
            font_size: value.font_size,
            font_weight: value.font_weight,
            italic: value.italic,
            color: value.color,
            align: value.align.into(),
            opacity: value.opacity,
            stroke_width: value.stroke_width,
            stroke_color: value.stroke_color,
            shadow: value.shadow,
            background: value.background,
            line_height: value.line_height,
            tracking: value.tracking,
        }
    }
}

/// `Option<Option<T>>` over the tool's JSON args, same convention as
/// `concat_project::commands`'s own (private) `double_option`: the key
/// absent leaves the field untouched, present as `null` clears it, present
/// with a value sets it. The generated JSON schema cannot show this
/// three-way distinction (it renders as an ordinary nullable optional
/// field) - each field that uses it says so in its own doc comment.
mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

/// A partial update to one clip, for `update_clip`. Mirrors [`cmd::ClipPatch`].
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ClipPatchArg {
    /// New display name, taken verbatim.
    pub name: Option<String>,
    /// New gain; floored at 0, not capped at 1.
    pub volume: Option<f64>,
    /// New fade-in length in seconds, floored at 0.
    pub fade_in: Option<f64>,
    /// New fade-out length in seconds, floored at 0.
    pub fade_out: Option<f64>,
    /// Clamped into 0..=1.
    pub opacity: Option<f64>,
    /// New pitch-preservation setting for a sped-up or slowed-down clip.
    pub preserve_pitch: Option<bool>,
    /// Play backwards.
    pub reverse: Option<bool>,
    /// Mirror left to right.
    pub flip_h: Option<bool>,
    /// Mirror top to bottom.
    pub flip_v: Option<bool>,
    /// The blend mode's name; empty is normal.
    pub blend: Option<String>,
    /// Omit to leave the crop alone, pass `null` to remove it, pass a value
    /// to set it (three-way wire convention - see the `dto` module doc).
    #[serde(default, deserialize_with = "double_option::deserialize")]
    pub crop: Option<Option<CropArg>>,
    /// Wholesale replacement of the audio filter chain.
    pub filters: Option<Vec<AppliedFilterArg>>,
    /// Wholesale replacement of the video effect chain.
    pub video_effects: Option<Vec<AppliedFilterArg>>,
    /// Omit to leave alone, `null` to clear, a value to replace (three-way
    /// wire convention - see the `dto` module doc).
    #[serde(default, deserialize_with = "double_option::deserialize")]
    pub transition_in: Option<Option<TransitionArg>>,
    /// Omit to leave alone, `null` to clear, a value to set (three-way wire
    /// convention - see the `dto` module doc). Setting a style also renames
    /// the clip after its first line.
    #[serde(default, deserialize_with = "double_option::deserialize")]
    pub text: Option<Option<TextStyleArg>>,
}

impl From<ClipPatchArg> for cmd::ClipPatch {
    fn from(value: ClipPatchArg) -> Self {
        cmd::ClipPatch {
            name: value.name,
            volume: value.volume,
            fade_in: value.fade_in,
            fade_out: value.fade_out,
            opacity: value.opacity,
            preserve_pitch: value.preserve_pitch,
            reverse: value.reverse,
            flip_h: value.flip_h,
            flip_v: value.flip_v,
            blend: value.blend,
            crop: value
                .crop
                .map(|inner| inner.map(model::Crop::from)),
            filters: value
                .filters
                .map(|list| list.into_iter().map(model::AppliedFilter::from).collect()),
            video_effects: value
                .video_effects
                .map(|list| list.into_iter().map(model::AppliedFilter::from).collect()),
            transition_in: value
                .transition_in
                .map(|inner| inner.map(model::Transition::from)),
            text: value.text.map(|inner| inner.map(model::TextStyle::from)),
        }
    }
}

/// Deserializes a raw `Command` from the `apply_raw_command` escape hatch.
/// Kept as a thin named function (rather than exposing `Command` itself as a
/// tool parameter type) since `Command` has no `JsonSchema` impl - the tool
/// accepts a `serde_json::Value` and this converts it, so a bad shape comes
/// back as the same message `serde_json` would give directly.
pub fn parse_raw_command(value: serde_json::Value) -> Result<Command, String> {
    serde_json::from_value(value).map_err(|error| format!("not a valid command: {error}"))
}
