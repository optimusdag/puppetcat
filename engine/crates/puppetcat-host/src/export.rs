// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Rendering the session to a file.
//!
//! The rendering is `puppetcat-export`'s; what is here is the seam the window
//! uses: the session flattens itself, the window adds the destination and
//! the quality, and the one export that can run at a time reports through a
//! callback and stops through a flag.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub use puppetcat_export::{ExportClip, ExportRange, ExportRequest, Reporter, render};

use crate::AppDirs;
use crate::jobs::{Job, SingleFlight};
use crate::session::Session;

/// The user's remembered scratch folder for exports, straight out of the
/// window's own `settings.json` - read here, rather than taken as a
/// `Preferences` value, so the agent (which never builds a `Preferences`)
/// can honour the same machine-wide choice as the window without either
/// depending on the other's crate. A missing file, an unset field, or a
/// blank string are all "no preference", the same as `None`.
pub fn temp_folder_preference(dirs: &AppDirs) -> Option<String> {
    let bytes = std::fs::read(dirs.config.join("settings.json")).ok()?;
    let document: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    document
        .get("tempExportFolder")?
        .as_str()
        .filter(|folder| !folder.is_empty())
        .map(str::to_owned)
}

/// What the window decides about an export: where it goes, how good it is,
/// and any rasterised titles rejoining as image clips.
#[derive(Clone, Debug, Default)]
pub struct ExportSpec {
    /// The file to write.
    pub output: String,
    /// Where to render before moving the finished file to `output`; see
    /// [`puppetcat_export::ExportRequest::scratch_dir`].
    pub scratch_dir: Option<String>,
    /// Constant rate factor; lower is better quality and a bigger file.
    pub crf: u8,
    /// The x264 speed/size preset name, e.g. "medium".
    pub preset: String,
    /// Export only this stretch of the timeline, as `(start, end)` seconds.
    /// `None` is all of it. See [`puppetcat_export::ExportRequest::range`].
    pub range: Option<(f64, f64)>,
}

/// One progress report: which frame of how many, in which stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Frames done.
    pub frame: i64,
    /// Frames in total.
    pub total: i64,
    /// What the exporter is doing: "video", "audio", "mux".
    pub stage: &'static str,
}

/// The session flattened into a request the engine can render.
pub fn request(session: &Session, spec: &ExportSpec, titles: Vec<ExportClip>) -> ExportRequest {
    let settings = session.settings();
    let mut clips = session.flattened_clips();
    clips.extend(titles);
    ExportRequest {
        output: spec.output.clone(),
        scratch_dir: spec.scratch_dir.clone(),
        width: settings.width,
        height: settings.height,
        rate_num: settings.rate_num,
        rate_den: settings.rate_den,
        crf: spec.crf,
        preset: spec.preset.clone(),
        clips,
        range: spec.range.map(|(start, end)| ExportRange { start, end }),
    }
}

/// Renders `request` and returns the path written, reporting through
/// `progress` and stopping at the next frame once `cancel` is set. Blocks for
/// the whole render: run it on its own thread.
pub fn run(
    request: &ExportRequest,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<String, String> {
    let mut report = |frame: i64, total: i64, stage: &'static str| {
        progress(Progress {
            frame,
            total,
            stage,
        });
    };
    render(
        request,
        Reporter {
            progress: &mut report,
            cancel,
        },
    )
}

/// The slot for the one export that can run at a time - enforced, not
/// documented: a second export while one runs is refused instead of racing
/// the first for its temp files and cancel flag.
#[derive(Clone, Default)]
pub struct Exporter {
    slot: Arc<SingleFlight>,
}

impl Exporter {
    /// An idle exporter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claims the slot for a new export, whose cancel flag the render polls.
    pub fn begin(&self) -> Result<Job, String> {
        self.slot.begin("export")
    }

    /// Asks the running export to stop at the next frame. Idle is a no-op.
    pub fn cancel(&self) {
        self.slot.cancel();
    }

    /// Whether an export is running.
    pub fn is_busy(&self) -> bool {
        self.slot.is_busy()
    }
}
