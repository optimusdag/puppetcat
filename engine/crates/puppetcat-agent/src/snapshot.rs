// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The gap-fix: rendering one still frame of the open session to PNG bytes,
//! so a tool call can hand an agent something it can actually look at
//! instead of a description of the timeline.
//!
//! Everything this needs already exists headlessly - `puppetcat_export`'s own
//! paused-monitor path (`preview_frame`) does the identical plan/composite
//! an export would run, synchronously, CPU-only, no window - this just adds
//! the encode-to-PNG step that path stops short of.

use puppetcat_core::frame::Frame;
use puppetcat_export::PreviewFrameRequest;
use puppetcat_host::{Session, Titles};
use puppetcat_media::ReaderPool;

/// Renders the session at `time` and returns PNG bytes. `width`/`height`
/// default to the project's own settings when not given.
pub fn render_still(
    session: &Session,
    titles: &Titles,
    pool: &ReaderPool,
    time: f64,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<Vec<u8>, String> {
    let settings = session.settings();
    let width = width.unwrap_or(settings.width).max(1);
    let height = height.unwrap_or(settings.height).max(1);

    // The same two clip lists the GUI's own preview merges: the timeline's
    // media clips, plus rasterised text clips laid on top - see
    // `Titles::clips`'s own doc for why titles are not part of
    // `flattened_clips` itself.
    let mut clips = session.flattened_clips();
    clips.extend(
        titles
            .clips(session.project(), width, height)
            .into_iter()
            .map(|title| title.clip),
    );

    let request = PreviewFrameRequest {
        time,
        width,
        height,
        rate_num: settings.rate_num,
        rate_den: settings.rate_den,
        clips,
    };
    let rgba = puppetcat_export::preview_frame(pool, &request)?;
    let frame = Frame::from_rgba(width, height, rgba)
        .ok_or_else(|| "the rendered frame had the wrong number of bytes".to_owned())?;
    puppetcat_media::png(&frame).map_err(|error| error.to_string())
}
