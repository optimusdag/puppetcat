// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! What transcription and speech mean for the edit.
//!
//! Whisper turns a clip's sound into timed text, and Kokoro turns text into a
//! WAV; neither knows about timelines. This is the half in between - which
//! window of a clip to transcribe, which title clips its words become, where
//! a spoken title's voice lands - and both the window and the agent's tools
//! call it, so a button and a tool make the same edit.

use puppetcat_project::Command;
use puppetcat_project::model::{ClipKind, Project, TextStyle};

use crate::transcribe::{Segment, TranscribeRequest};
use crate::tts::SpeakRequest;

/// The Kokoro speaker used when nobody picks one: `af_heart`.
pub const DEFAULT_VOICE: i32 = 3;

/// What transcribing one clip needs, and what mapping its words back needs.
#[derive(Clone, Debug)]
pub struct CaptionPlan {
    /// What to hand the transcriber.
    pub request: TranscribeRequest,
    /// Where the clip starts on the timeline, seconds.
    pub clip_start: f64,
    /// The clip's playback rate, so source seconds become timeline seconds.
    pub clip_speed: f64,
}

/// The transcription of one clip's stretch of its source.
pub fn caption_plan(
    project: &Project,
    clip_id: &str,
    language: &str,
    model_id: &str,
) -> Result<CaptionPlan, String> {
    let clip = project
        .active()
        .clip(clip_id)
        .ok_or_else(|| format!("clip {clip_id:?} is not on the active timeline"))?;
    let media = project
        .media_by_id(&clip.media_id)
        .ok_or("this clip has no file to transcribe")?;
    if !media.has_audio {
        return Err("this clip has no sound to transcribe".to_owned());
    }
    Ok(CaptionPlan {
        request: TranscribeRequest {
            path: media.path.clone(),
            source_start: clip.source_start,
            window: clip.duration * clip.speed,
            language: language.to_owned(),
            model_id: model_id.to_owned(),
        },
        clip_start: clip.start,
        clip_speed: clip.speed.max(0.0625),
    })
}

/// One title clip per spoken segment, on the timeline where it was said.
pub fn caption_commands(plan: &CaptionPlan, segments: &[Segment]) -> Vec<Command> {
    segments
        .iter()
        .map(|segment| Command::AddTextClip {
            track_id: None,
            start: plan.clip_start + segment.start / plan.clip_speed,
            style: Some(TextStyle {
                content: segment.text.clone(),
                font_family: "Inter".to_owned(),
                font_size: 0.05,
                font_weight: 600.0,
                ..TextStyle::default()
            }),
            duration: Some(((segment.end - segment.start) / plan.clip_speed).max(0.2)),
            offset_y: Some(0.35),
        })
        .collect()
}

/// What speaking some text needs: the request, and where the voice goes.
#[derive(Clone, Debug)]
pub struct SpeakPlan {
    /// What to hand the synthesiser.
    pub request: SpeakRequest,
    /// Where on the timeline the voice starts, seconds.
    pub start: f64,
}

/// Speaking a title clip's words, starting where the title starts.
pub fn speak_title_plan(
    project: &Project,
    clip_id: &str,
    project_dir: &str,
    model_id: &str,
    voice: i32,
    speed: f32,
) -> Result<SpeakPlan, String> {
    let clip = project
        .active()
        .clip(clip_id)
        .ok_or_else(|| format!("clip {clip_id:?} is not on the active timeline"))?;
    if clip.kind != ClipKind::Text {
        return Err("only a title has words to speak".to_owned());
    }
    let text = clip
        .text
        .as_ref()
        .map(|text| text.content.clone())
        .filter(|text| !text.trim().is_empty())
        .ok_or("that title is empty")?;
    Ok(SpeakPlan {
        request: SpeakRequest {
            model_id: model_id.to_owned(),
            voice,
            text,
            speed,
            project: project_dir.to_owned(),
        },
        start: clip.start,
    })
}

/// The command that puts a voice already in the bin onto the first lane with
/// room at `start`. Adding the WAV to the bin comes first (`AddMedia`); it
/// mints the id this wants - or mints none when the bin already held the file,
/// in which case the caller looks the path up.
pub fn voice_clip_command(media_id: String, start: f64) -> Command {
    Command::AddClipAtFirstFree { media_id, start }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(clip_start: f64, clip_speed: f64) -> CaptionPlan {
        CaptionPlan {
            request: TranscribeRequest {
                path: "a.mp4".to_owned(),
                source_start: 0.0,
                window: 10.0,
                language: "auto".to_owned(),
                model_id: "base.en".to_owned(),
            },
            clip_start,
            clip_speed,
        }
    }

    #[test]
    fn captions_land_where_the_words_were_said_on_the_timeline() {
        let segments = [
            Segment {
                start: 0.0,
                end: 4.0,
                text: "hello".to_owned(),
            },
            Segment {
                start: 6.0,
                end: 6.05,
                text: "ok".to_owned(),
            },
        ];
        // A clip at 10 s playing at double speed: source seconds halve.
        let commands = caption_commands(&plan(10.0, 2.0), &segments);
        let Command::AddTextClip {
            start, duration, ..
        } = &commands[0]
        else {
            panic!("a title clip");
        };
        assert_eq!((*start, *duration), (10.0, Some(2.0)));
        let Command::AddTextClip {
            start, duration, ..
        } = &commands[1]
        else {
            panic!("a title clip");
        };
        assert_eq!(*start, 13.0);
        assert_eq!(
            *duration,
            Some(0.2),
            "a flicker is held long enough to read"
        );
    }
}
