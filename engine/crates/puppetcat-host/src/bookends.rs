// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Fade-from-black and fade-to-black bookends as one workflow.
//!
//! The edit itself is one command; what it needs around it is a black still in
//! the bin and a lane to bookend. The still is generated once, owned by the
//! project, and reused, so neither a person nor an agent has to make media to
//! fade a film in and out.

use std::path::Path;

use puppetcat_core::frame::Frame;
use puppetcat_project::Command;
use puppetcat_project::model::ClipKind;

use crate::session::{EditorView, Session};

/// Adds black bookends of `duration` seconds at both ends of the timeline's
/// main lane (or `track_id`), each cross-faded for `transition_duration`.
/// `shift_all_tracks` keeps every lane in sync with the head bookend;
/// `shift_free_markers` moves free markers with it. One undo step for the
/// edit; the black still, if it had to be made, is added first.
pub fn add_fade_to_black(
    session: &mut Session,
    track_id: Option<String>,
    duration: f64,
    transition_duration: f64,
    shift_all_tracks: bool,
    shift_free_markers: bool,
) -> Result<EditorView, String> {
    let generated = Path::new(session.path()).join(".puppetcat-generated");
    std::fs::create_dir_all(&generated)
        .map_err(|error| format!("could not create {}: {error}", generated.display()))?;
    let black = generated.join("solid-000000.png");
    if !black.exists() {
        let frame = Frame::from_rgba(1, 1, vec![0, 0, 0, 255])
            .ok_or_else(|| "could not create the black generator frame".to_owned())?;
        let bytes = puppetcat_media::png(&frame).map_err(|error| error.to_string())?;
        let partial = generated.join(".solid-000000.png.part");
        std::fs::write(&partial, bytes)
            .map_err(|error| format!("could not write {}: {error}", partial.display()))?;
        std::fs::rename(&partial, &black)
            .map_err(|error| format!("could not install {}: {error}", black.display()))?;
    }
    let path = black.to_string_lossy().into_owned();
    if !session.project().media.iter().any(|item| item.path == path) {
        let summary = crate::media::probe(&path)?;
        session
            .apply(Command::AddMedia {
                item: summary.to_new_media(),
            })
            .map_err(|error| error.to_string())?;
    }
    let media_id = session
        .project()
        .media
        .iter()
        .find(|item| item.path == path)
        .map(|item| item.id.clone())
        .ok_or_else(|| "the generated black media did not enter the bin".to_owned())?;
    let track_id = match track_id {
        Some(id) => id,
        None => {
            let timeline = session.project().active();
            timeline
                .clips
                .iter()
                .filter(|clip| {
                    matches!(clip.kind, ClipKind::Video | ClipKind::Image)
                        && timeline
                            .track(&clip.track_id)
                            .is_some_and(|track| track.visible)
                })
                .min_by(|left, right| {
                    left.start
                        .total_cmp(&right.start)
                        .then_with(|| right.duration.total_cmp(&left.duration))
                })
                .map(|clip| clip.track_id.clone())
                .ok_or_else(|| "the active timeline has no visible video clip".to_owned())?
        }
    };
    session
        .apply(Command::AddFadeToBlack {
            media_id,
            track_id,
            duration,
            transition_duration,
            shift_all_tracks,
            shift_free_markers,
        })
        .map_err(|error| error.to_string())?;
    Ok(session.view())
}
