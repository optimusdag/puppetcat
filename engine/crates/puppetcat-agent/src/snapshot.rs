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

#![allow(missing_docs)] // Serialized contact-sheet fields are the public wire schema.

use std::path::Path;

use puppetcat_core::frame::Frame;
use puppetcat_export::PreviewFrameRequest;
use puppetcat_host::{Session, Titles};
use puppetcat_media::ReaderPool;
use serde::Serialize;

/// Most frames one contact sheet may hold.
pub const MAX_CONTACT_SHEET_FRAMES: usize = 64;
/// Longest side of a contact sheet whose cell size was not given.
const MAX_DEFAULT_SHEET_EDGE: u32 = 4096;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactSheetCell {
    pub index: usize,
    pub time: f64,
    pub column: u32,
    pub row: u32,
    /// Mean absolute RGB difference from the preceding requested frame,
    /// normalised to 0..=1. The first cell has no predecessor.
    pub change_from_previous: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ContactSheet {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub columns: u32,
    pub rows: u32,
    pub cells: Vec<ContactSheetCell>,
    pub output_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactSheetMetadata<'a> {
    pub width: u32,
    pub height: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub columns: u32,
    pub rows: u32,
    pub cells: &'a [ContactSheetCell],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<&'a str>,
}

impl ContactSheet {
    pub fn metadata(&self) -> ContactSheetMetadata<'_> {
        ContactSheetMetadata {
            width: self.width,
            height: self.height,
            frame_width: self.frame_width,
            frame_height: self.frame_height,
            columns: self.columns,
            rows: self.rows,
            cells: &self.cells,
            output_path: self.output_path.as_deref(),
        }
    }
}

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
    let frame = render_frame(session, titles, pool, time, width, height)?;
    puppetcat_media::png(&frame).map_err(|error| error.to_string())
}

fn render_frame(
    session: &Session,
    titles: &Titles,
    pool: &ReaderPool,
    time: f64,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<Frame, String> {
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
    Frame::from_rgba(width, height, rgba)
        .ok_or_else(|| "the rendered frame had the wrong number of bytes".to_owned())
}

#[allow(clippy::too_many_arguments)]
pub fn render_contact_sheet(
    session: &Session,
    titles: &Titles,
    pool: &ReaderPool,
    times: &[f64],
    width: Option<u32>,
    height: Option<u32>,
    columns: Option<u32>,
    output: Option<&Path>,
    frames_output_dir: Option<&Path>,
) -> Result<ContactSheet, String> {
    if times.is_empty() {
        return Err("times must contain at least one timeline instant".to_owned());
    }
    if times.len() > MAX_CONTACT_SHEET_FRAMES {
        return Err(format!(
            "a contact sheet is limited to {MAX_CONTACT_SHEET_FRAMES} frames"
        ));
    }
    if times.iter().any(|time| !time.is_finite() || *time < 0.0) {
        return Err("every time must be a finite non-negative number".to_owned());
    }

    let settings = session.settings();
    let columns = columns
        .unwrap_or_else(|| (times.len() as f64).sqrt().ceil() as u32)
        .clamp(1, times.len() as u32);
    let rows = (times.len() as u32).div_ceil(columns);
    // Unsized cells default to the timeline size, shrunk so the whole sheet
    // stays within MAX_DEFAULT_SHEET_EDGE; a full-size grid of 24 frames is
    // ~200 MB of pixels before PNG and base64 encoding.
    let fit = (f64::from(MAX_DEFAULT_SHEET_EDGE) / f64::from(settings.width * columns))
        .min(f64::from(MAX_DEFAULT_SHEET_EDGE) / f64::from(settings.height * rows))
        .min(1.0);
    let scaled = |size: u32| ((f64::from(size) * fit).round() as u32).max(1);
    let (default_width, default_height) = match (width, height) {
        (None, None) => (scaled(settings.width), scaled(settings.height)),
        _ => (settings.width, settings.height),
    };
    let frame_width = width.unwrap_or(default_width).max(1);
    let frame_height = height.unwrap_or(default_height).max(1);
    let sheet_width = frame_width
        .checked_mul(columns)
        .ok_or_else(|| "contact sheet width is too large".to_owned())?;
    let sheet_height = frame_height
        .checked_mul(rows)
        .ok_or_else(|| "contact sheet height is too large".to_owned())?;
    let mut sheet = Frame::black(sheet_width, sheet_height);
    let mut cells = Vec::with_capacity(times.len());
    let mut previous: Option<Frame> = None;

    if let Some(directory) = frames_output_dir {
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    }

    for (index, time) in times.iter().copied().enumerate() {
        let frame = render_frame(
            session,
            titles,
            pool,
            time,
            Some(frame_width),
            Some(frame_height),
        )?;
        let change_from_previous = previous.as_ref().map(|prior| visual_change(prior, &frame));
        let output_path = if let Some(directory) = frames_output_dir {
            let path = directory.join(format!(
                "frame-{index:03}-t{:010}.png",
                (time * 1000.0).round() as u64
            ));
            write_atomic(
                &path,
                &puppetcat_media::png(&frame).map_err(|error| error.to_string())?,
            )?;
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        };
        let column = index as u32 % columns;
        let row = index as u32 / columns;
        sheet.blit(&frame, column * frame_width, row * frame_height);
        cells.push(ContactSheetCell {
            index,
            time,
            column,
            row,
            change_from_previous,
            output_path,
        });
        previous = Some(frame);
    }

    let png = puppetcat_media::png(&sheet).map_err(|error| error.to_string())?;
    let output_path = if let Some(path) = output {
        write_atomic(path, &png)?;
        Some(path.to_string_lossy().into_owned())
    } else {
        None
    };
    Ok(ContactSheet {
        png,
        width: sheet_width,
        height: sheet_height,
        frame_width,
        frame_height,
        columns,
        rows,
        cells,
        output_path,
    })
}

fn visual_change(left: &Frame, right: &Frame) -> f64 {
    let total: u64 = left
        .pixels()
        .chunks_exact(4)
        .zip(right.pixels().chunks_exact(4))
        .map(|(a, b)| {
            u64::from(a[0].abs_diff(b[0]))
                + u64::from(a[1].abs_diff(b[1]))
                + u64::from(a[2].abs_diff(b[2]))
        })
        .sum();
    total as f64 / (left.width() as f64 * left.height() as f64 * 3.0 * 255.0)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{} is not a file path", path.display()))?;
    let partial = path.with_file_name(format!(".{}.part", file_name.to_string_lossy()));
    std::fs::write(&partial, bytes)
        .map_err(|error| format!("could not write {}: {error}", partial.display()))?;
    std::fs::rename(&partial, path)
        .map_err(|error| format!("could not install {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_change_is_normalised() {
        let black = Frame::black(2, 1);
        let mut white = Frame::black(2, 1);
        white.fill([255, 255, 255, 255]);
        assert_eq!(visual_change(&black, &black), 0.0);
        assert_eq!(visual_change(&black, &white), 1.0);
    }
}
