// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Transcription and voice: the Settings pages' model list and downloads, the
//! window's Auto captions on a clip, and its Generate voice on a title - as
//! tools. The edits come from `puppetcat_speech::edits`, the same functions the
//! window calls.

use puppetcat_project::Command;
use puppetcat_speech::edits;
use puppetcat_speech::transcribe::Segment;
use serde::Serialize;

use super::{AgentEngine, no_project};

/// What `caption_clip` did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionResult {
    /// The model that listened.
    pub model_id: String,
    /// What was said, in seconds from the start of the clip's source window.
    pub segments: Vec<Segment>,
    /// Title clips added to the timeline.
    pub captions_added: usize,
}

/// What `generate_voice` did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceResult {
    /// The voice model that spoke.
    pub model_id: String,
    /// The WAV, inside the project folder.
    pub path: String,
    /// Seconds of speech.
    pub duration: f64,
    /// The bin item the WAV became.
    pub media_id: Option<String>,
    /// Where on the timeline it starts.
    pub start: f64,
}

/// What `generate_voice` is asked to say.
#[derive(Debug, Default)]
pub struct VoiceRequest {
    /// A title clip whose words are spoken, starting where it starts.
    pub clip_id: Option<String>,
    /// Or words given outright; `start` says where they go.
    pub text: Option<String>,
    /// Timeline seconds where the voice starts, for `text`.
    pub start: Option<f64>,
    /// Kokoro speaker id; defaults to `af_heart`.
    pub voice: Option<i32>,
    /// Speaking rate, 1.0 natural. Defaults to 1.
    pub speed: Option<f32>,
    /// A voice model id; defaults to the first one downloaded.
    pub model_id: Option<String>,
}

impl AgentEngine {
    /// The transcriber and voice models, whether each is downloaded, and the
    /// speakers on offer.
    pub fn speech_status(&self) -> Result<serde_json::Value, String> {
        let transcriber = puppetcat_speech::Transcriber::status(&self.dirs)?;
        let voices = puppetcat_speech::Speech::status(&self.dirs)?;
        Ok(serde_json::json!({
            "transcriber": transcriber,
            "voice": voices,
        }))
    }

    /// Downloads one model - `kind` is `transcriber` or `voice` - and returns
    /// when it is on disk. Large: a transcriber is tens to hundreds of
    /// megabytes and a voice bundle more.
    pub fn download_speech_model(&self, kind: &str, id: &str) -> Result<(), String> {
        match kind {
            "transcriber" => self.transcriber.download_model(&self.dirs, id, |_| {}),
            "voice" => self.speech.download_model(&self.dirs, id, |_| {}),
            other => Err(format!("unknown kind {other:?}; use transcriber or voice")),
        }
    }

    fn installed_transcriber(&self, wanted: Option<String>) -> Result<String, String> {
        let status = puppetcat_speech::Transcriber::status(&self.dirs)?;
        match wanted {
            Some(id) => Ok(id),
            None => status
                .models
                .iter()
                .find(|model| model.downloaded)
                .map(|model| model.id.clone())
                .ok_or_else(|| {
                    "no transcriber model is downloaded: call download_speech_model first \
                     (see speech_status for the list)"
                        .to_owned()
                }),
        }
    }

    fn installed_voice(&self, wanted: Option<String>) -> Result<String, String> {
        let status = puppetcat_speech::Speech::status(&self.dirs)?;
        match wanted {
            Some(id) => Ok(id),
            None => status
                .models
                .iter()
                .find(|model| model.downloaded)
                .map(|model| model.id.clone())
                .ok_or_else(|| {
                    "no voice model is downloaded: call download_speech_model first \
                     (see speech_status for the list)"
                        .to_owned()
                }),
        }
    }

    /// Transcribes one clip's sound and adds the words to the timeline as
    /// title clips, one per segment, as one edit. Blocks while whisper runs.
    pub fn caption_clip(
        &self,
        clip_id: &str,
        language: Option<String>,
        model_id: Option<String>,
    ) -> Result<CaptionResult, String> {
        let model_id = self.installed_transcriber(model_id)?;
        let plan = {
            let guard = self.session_lock();
            let session = guard.as_ref().ok_or_else(no_project)?;
            edits::caption_plan(
                session.project(),
                clip_id,
                language.as_deref().unwrap_or("auto"),
                &model_id,
            )?
        };
        let segments = self
            .transcriber
            .transcribe(&self.dirs, &plan.request, |_| {})?;
        let commands = edits::caption_commands(&plan, &segments);
        let captions_added = commands.len();
        if captions_added > 0 {
            self.apply(Command::Batch { commands })?;
        }
        Ok(CaptionResult {
            model_id,
            segments,
            captions_added,
        })
    }

    /// Speaks a title's words, or some given text, and puts the voice on the
    /// timeline. Blocks while the voice is synthesised.
    pub fn generate_voice(&self, request: VoiceRequest) -> Result<VoiceResult, String> {
        let model_id = self.installed_voice(request.model_id.clone())?;
        let voice = request.voice.unwrap_or(edits::DEFAULT_VOICE);
        let speed = request.speed.unwrap_or(1.0);
        let plan = {
            let guard = self.session_lock();
            let session = guard.as_ref().ok_or_else(no_project)?;
            match (&request.clip_id, &request.text) {
                (Some(_), Some(_)) => return Err("give a clipId or a text, not both".to_owned()),
                (Some(id), None) => edits::speak_title_plan(
                    session.project(),
                    id,
                    session.path(),
                    &model_id,
                    voice,
                    speed,
                )?,
                (None, Some(text)) => edits::SpeakPlan {
                    request: puppetcat_speech::tts::SpeakRequest {
                        model_id: model_id.clone(),
                        voice,
                        text: text.clone(),
                        speed,
                        project: session.path().to_owned(),
                    },
                    start: request.start.unwrap_or(0.0).max(0.0),
                },
                (None, None) => return Err("give a clipId or a text to speak".to_owned()),
            }
        };
        let spoken = self.speech.speak(&self.dirs, &plan.request, |_| {})?;
        let summary = puppetcat_host::media::probe(&spoken.path)?;
        let created = self
            .apply(Command::AddMedia {
                item: summary.to_new_media(),
            })?
            .created_id;
        let media_id = created.or_else(|| {
            let guard = self.session_lock();
            guard.as_ref().and_then(|session| {
                session
                    .project()
                    .media
                    .iter()
                    .find(|item| item.path == summary.path)
                    .map(|item| item.id.clone())
            })
        });
        if let Some(id) = &media_id {
            self.apply(edits::voice_clip_command(id.clone(), plan.start))?;
        }
        Ok(VoiceResult {
            model_id,
            path: spoken.path,
            duration: spoken.duration,
            media_id,
            start: plan.start,
        })
    }
}
