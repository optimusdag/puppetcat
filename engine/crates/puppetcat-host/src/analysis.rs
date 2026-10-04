// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Facts about a project's media, and the edits they suggest: loudness,
//! silence/dead-air intervals, a deliberately labelled speech-vs-music
//! heuristic, and shot changes. The window and the agent both call these, so
//! what a button does and what a tool does can never drift apart. Decoding
//! stays in the engine through `puppetcat-media`; no shelling out to ffmpeg
//! or parsing its stderr.

#![allow(missing_docs)] // Serialized analysis fields are the public wire schema.

use std::path::Path;

use puppetcat_core::time::{FrameRate, Rational};
use puppetcat_media::{
    AudioDecoder, AudioOptions, DecodeOptions, Decoder, FrameSource, SampleFormat,
};
use puppetcat_project::model::{ClipKind, MediaKind};
use puppetcat_project::{Command, Project};
use serde::Serialize;

const ANALYSIS_RATE: u32 = 16_000;

/// One time interval in target coordinates.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioInterval {
    pub start: f64,
    pub end: f64,
    pub duration: f64,
}

/// One compact analysis bucket, returned only when explicitly requested.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioBucket {
    pub start: f64,
    pub end: f64,
    pub rms_db: f64,
    pub peak_db: f64,
    pub zero_crossing_rate: f64,
}

/// The stable result shape exposed by the MCP tool.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAnalysis {
    pub target: String,
    pub start: f64,
    pub end: f64,
    pub duration: f64,
    pub bucket_seconds: f64,
    pub silence_threshold_db: f64,
    pub integrated_rms_db: f64,
    pub peak_db: f64,
    pub active_fraction: f64,
    pub speech_likelihood: f64,
    pub music_likelihood: f64,
    pub classification_method: &'static str,
    pub silence: Vec<AudioInterval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buckets: Option<Vec<AudioBucket>>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FeatureBucket {
    pub start: f64,
    pub end: f64,
    pub rms: f64,
    pub peak: f64,
    pub zcr: f64,
}

pub fn analyze_file(
    path: &Path,
    source_start: f64,
    duration: Option<f64>,
    bucket_seconds: f64,
) -> Result<Vec<FeatureBucket>, String> {
    let bucket_seconds = bucket_seconds.clamp(0.02, 2.0);
    let options = AudioOptions {
        start: Some(source_start.max(0.0)),
        duration: duration.map(|value| value.max(0.0)),
        filters: Vec::new(),
        rate: ANALYSIS_RATE,
        channels: 1,
        format: SampleFormat::F32,
    };
    let mut decoder = AudioDecoder::open(path, &options).map_err(|error| error.to_string())?;
    let samples_per_bucket = (f64::from(ANALYSIS_RATE) * bucket_seconds).round() as usize;
    let samples_per_bucket = samples_per_bucket.max(1);
    let mut pending = Vec::with_capacity(samples_per_bucket * 2);
    let mut buckets = Vec::new();
    while let Some(chunk) = decoder.next_f32().map_err(|error| error.to_string())? {
        pending.extend(chunk);
        while pending.len() >= samples_per_bucket {
            let tail = pending.split_off(samples_per_bucket);
            let samples = std::mem::replace(&mut pending, tail);
            push_bucket(&mut buckets, &samples, bucket_seconds);
        }
    }
    if !pending.is_empty() {
        let seconds = pending.len() as f64 / f64::from(ANALYSIS_RATE);
        push_bucket(&mut buckets, &pending, seconds);
    }
    Ok(buckets)
}

fn push_bucket(out: &mut Vec<FeatureBucket>, samples: &[f32], length: f64) {
    if samples.is_empty() {
        return;
    }
    let sum_square: f64 = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    let rms = (sum_square / samples.len() as f64).sqrt();
    let peak = samples
        .iter()
        .map(|sample| f64::from(sample.abs()))
        .fold(0.0, f64::max);
    let crossings = samples
        .windows(2)
        .filter(|pair| (pair[0] < 0.0 && pair[1] >= 0.0) || (pair[0] >= 0.0 && pair[1] < 0.0))
        .count();
    let start = out.last().map_or(0.0, |bucket| bucket.end);
    out.push(FeatureBucket {
        start,
        end: start + length,
        rms,
        peak,
        zcr: crossings as f64 / samples.len().saturating_sub(1).max(1) as f64,
    });
}

fn db(value: f64) -> f64 {
    if value <= 1e-9 {
        -120.0
    } else {
        20.0 * value.log10()
    }
}

pub fn summarize(
    target: String,
    origin: f64,
    features: Vec<FeatureBucket>,
    bucket_seconds: f64,
    silence_threshold_db: f64,
    min_silence: f64,
    include_buckets: bool,
    warnings: Vec<String>,
) -> AudioAnalysis {
    let end_local = features.last().map_or(0.0, |bucket| bucket.end);
    let powers: Vec<f64> = features
        .iter()
        .map(|bucket| bucket.rms * bucket.rms)
        .collect();
    let integrated = if powers.is_empty() {
        0.0
    } else {
        (powers.iter().sum::<f64>() / powers.len() as f64).sqrt()
    };
    let peak = features
        .iter()
        .map(|bucket| bucket.peak)
        .fold(0.0, f64::max);
    let active: Vec<&FeatureBucket> = features
        .iter()
        .filter(|bucket| db(bucket.rms) > silence_threshold_db)
        .collect();
    let active_fraction = if features.is_empty() {
        0.0
    } else {
        active.len() as f64 / features.len() as f64
    };
    let mean_zcr = if active.is_empty() {
        0.0
    } else {
        active.iter().map(|bucket| bucket.zcr).sum::<f64>() / active.len() as f64
    };
    let active_db: Vec<f64> = active.iter().map(|bucket| db(bucket.rms)).collect();
    let mean_db = if active_db.is_empty() {
        silence_threshold_db
    } else {
        active_db.iter().sum::<f64>() / active_db.len() as f64
    };
    let dynamic = if active_db.is_empty() {
        0.0
    } else {
        (active_db
            .iter()
            .map(|value| (value - mean_db).powi(2))
            .sum::<f64>()
            / active_db.len() as f64)
            .sqrt()
    };
    // These are intentionally not called probabilities. They are cheap,
    // local hints from envelope continuity, dynamics and zero crossings;
    // callers needing semantic certainty should transcribe or listen.
    let speech_zcr = (1.0 - (mean_zcr - 0.08).abs() / 0.12).clamp(0.0, 1.0);
    let music_zcr = (1.0 - (mean_zcr - 0.14).abs() / 0.20).clamp(0.0, 1.0);
    let speech_likelihood =
        (0.55 * speech_zcr + 0.45 * (dynamic / 12.0).clamp(0.0, 1.0)) * active_fraction;
    let music_likelihood =
        (0.45 * music_zcr + 0.35 * active_fraction + 0.20 * (1.0 - dynamic / 20.0).clamp(0.0, 1.0))
            * active_fraction;

    let mut silence = Vec::new();
    let mut run: Option<f64> = None;
    for bucket in &features {
        if db(bucket.rms) <= silence_threshold_db {
            run.get_or_insert(bucket.start);
        } else if let Some(start) = run.take() {
            push_silence(
                &mut silence,
                origin + start,
                origin + bucket.start,
                min_silence,
            );
        }
    }
    if let Some(start) = run {
        push_silence(
            &mut silence,
            origin + start,
            origin + end_local,
            min_silence,
        );
    }

    let buckets = include_buckets.then(|| {
        features
            .into_iter()
            .map(|bucket| AudioBucket {
                start: origin + bucket.start,
                end: origin + bucket.end,
                rms_db: db(bucket.rms),
                peak_db: db(bucket.peak),
                zero_crossing_rate: bucket.zcr,
            })
            .collect()
    });
    AudioAnalysis {
        target,
        start: origin,
        end: origin + end_local,
        duration: end_local,
        bucket_seconds,
        silence_threshold_db,
        integrated_rms_db: db(integrated),
        peak_db: db(peak),
        active_fraction,
        speech_likelihood: speech_likelihood.clamp(0.0, 1.0),
        music_likelihood: music_likelihood.clamp(0.0, 1.0),
        classification_method: "envelope-dynamics-zero-crossing heuristic",
        silence,
        buckets,
        warnings,
    }
}

fn push_silence(out: &mut Vec<AudioInterval>, start: f64, end: f64, minimum: f64) {
    if end - start + 1e-9 >= minimum.max(0.0) {
        out.push(AudioInterval {
            start,
            end,
            duration: end - start,
        });
    }
}

/// One detected cut, in the coordinates of whatever was analysed.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SceneCut {
    /// The first instant of the new shot, seconds.
    pub time: f64,
    /// How different it is from the shot before, `0.0..=1.0`: the mean
    /// brightness change per pixel. Hard cuts score high; motion scores low.
    pub score: f64,
}

/// Frames are compared at this size: enough to tell one shot from the next,
/// small enough that comparing them costs nothing next to decoding them.
const SCENE_SIZE: (u32, u32) = (64, 36);
/// The fastest rate frames are compared at; a 60 fps source is paced down.
const MAX_SCENE_RATE: f64 = 30.0;

/// Finds the cuts in a stretch of video by comparing each frame with the one
/// before it. `start` and `end` are source seconds; the cuts come back in the
/// same. A cut closer than `min_scene` seconds to the previous one is
/// ignored, which is what keeps a flash or a burst of motion from reading as
/// a string of cuts.
pub fn detect_scenes(
    path: &Path,
    start: f64,
    end: Option<f64>,
    threshold: f64,
    min_scene: f64,
) -> Result<Vec<SceneCut>, String> {
    let info = puppetcat_media::probe(path).map_err(|error| error.to_string())?;
    let video = info
        .video
        .ok_or_else(|| format!("{} has no video stream", path.display()))?;
    let rate = video
        .frame_rate
        .fps()
        .as_f64()
        .clamp(1.0, MAX_SCENE_RATE)
        .round();
    let mut options = DecodeOptions::default()
        .scaled_to(SCENE_SIZE.0, SCENE_SIZE.1)
        .at_rate(FrameRate::new(Rational::from_int(rate as i64)));
    let start = start.max(0.0);
    if start > 0.0 {
        options = options.starting_at(
            Rational::approximate(start).ok_or_else(|| "start is not a time".to_owned())?,
        );
    }
    if let Some(end) = end {
        options = options.limited_to((((end - start).max(0.0) * rate).ceil() as u64).max(1) + 1);
    }
    let mut decoder = Decoder::open(path, &options).map_err(|error| error.to_string())?;
    let mut previous: Option<Vec<u8>> = None;
    let mut scores = Vec::new();
    let mut index = 0u64;
    while let Some(frame) = decoder.next_frame().map_err(|error| error.to_string())? {
        let luma: Vec<u8> = frame
            .pixels()
            .chunks_exact(4)
            .map(|p| {
                (0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])) as u8
            })
            .collect();
        if let Some(before) = &previous {
            let total: u64 = before
                .iter()
                .zip(&luma)
                .map(|(a, b)| u64::from(a.abs_diff(*b)))
                .sum();
            scores.push((
                start + index as f64 / rate,
                total as f64 / (luma.len().max(1) as f64 * 255.0),
            ));
        }
        previous = Some(luma);
        index += 1;
    }
    Ok(cuts_from_scores(&scores, 1.0 / rate, threshold, min_scene))
}

/// The cuts among `(time, difference from the frame before)` scores: every
/// score at or over `threshold`, thinned so no two are within `min_scene`.
/// `first_gap` is how long the first shot has already run when the scores
/// begin (one frame), so a cut on the second frame is held to the same rule.
fn cuts_from_scores(
    scores: &[(f64, f64)],
    first_gap: f64,
    threshold: f64,
    min_scene: f64,
) -> Vec<SceneCut> {
    let mut cuts: Vec<SceneCut> = Vec::new();
    let mut last = scores.first().map_or(0.0, |(time, _)| time - first_gap);
    for &(time, score) in scores {
        if score >= threshold && time - last + 1e-9 >= min_scene {
            cuts.push(SceneCut { time, score });
            last = time;
        }
    }
    cuts
}

/// Analyses one bin item, one timeline clip, or the active timeline's
/// audible mix. Times are source-local for a media item and timeline
/// times for clip/timeline targets.
#[allow(clippy::too_many_arguments)]
pub fn analyze_audio(
    project: &Project,
    media_id: Option<String>,
    clip_id: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    bucket_seconds: f64,
    silence_threshold_db: f64,
    min_silence: f64,
    include_buckets: bool,
) -> Result<AudioAnalysis, String> {
    if media_id.is_some() && clip_id.is_some() {
        return Err("choose mediaId or clipId, not both".to_owned());
    }

    #[derive(Clone)]
    struct Source {
        path: String,
        source_start: f64,
        source_duration: f64,
        timeline_start: f64,
        speed: f64,
        volume: f64,
        has_filters: bool,
        curved_speed: bool,
    }

    let (target, origin, target_end, sources) = {
        let timeline = project.active();
        if let Some(id) = media_id {
            let media = project
                .media_by_id(&id)
                .ok_or_else(|| format!("media {id:?} is not in the bin"))?;
            if !media.has_audio {
                return Err(format!("media {id:?} has no audio stream"));
            }
            let origin = start.unwrap_or(0.0).max(0.0);
            let target_end = end.or(media.duration).unwrap_or(origin).max(origin);
            (
                format!("media:{id}"),
                origin,
                target_end,
                vec![Source {
                    path: media.path.clone(),
                    source_start: origin,
                    source_duration: target_end - origin,
                    timeline_start: origin,
                    speed: 1.0,
                    volume: 1.0,
                    has_filters: false,
                    curved_speed: false,
                }],
            )
        } else if let Some(id) = clip_id {
            let clip = timeline
                .clip(&id)
                .ok_or_else(|| format!("clip {id:?} is not on the active timeline"))?;
            let media = project
                .media_by_id(&clip.media_id)
                .ok_or_else(|| format!("clip {id:?} has no media"))?;
            if !media.has_audio || clip.muted == Some(true) {
                return Err(format!("clip {id:?} has no audible stream"));
            }
            let origin = start.unwrap_or(clip.start).max(clip.start);
            let target_end = end
                .unwrap_or(clip.start + clip.duration)
                .min(clip.start + clip.duration)
                .max(origin);
            let speed = clip.speed.max(0.0625);
            (
                format!("clip:{id}"),
                origin,
                target_end,
                vec![Source {
                    path: media.path.clone(),
                    source_start: clip.source_start + (origin - clip.start) * speed,
                    source_duration: (target_end - origin) * speed,
                    timeline_start: origin,
                    speed,
                    volume: clip.volume,
                    has_filters: !clip.filters.is_empty(),
                    curved_speed: clip.speed_curve.is_some(),
                }],
            )
        } else {
            let origin = start.unwrap_or(0.0).max(0.0);
            let target_end = end.unwrap_or_else(|| timeline.duration()).max(origin);
            let mut sources = Vec::new();
            for clip in &timeline.clips {
                if !matches!(clip.kind, ClipKind::Video | ClipKind::Audio) {
                    continue;
                }
                let Some(track) = timeline.track(&clip.track_id) else {
                    continue;
                };
                let Some(media) = project.media_by_id(&clip.media_id) else {
                    continue;
                };
                if track.muted || clip.muted == Some(true) || !media.has_audio {
                    continue;
                }
                let from = clip.start.max(origin);
                let to = (clip.start + clip.duration).min(target_end);
                if to <= from {
                    continue;
                }
                let speed = clip.speed.max(0.0625);
                sources.push(Source {
                    path: media.path.clone(),
                    source_start: clip.source_start + (from - clip.start) * speed,
                    source_duration: (to - from) * speed,
                    timeline_start: from,
                    speed,
                    volume: clip.volume,
                    has_filters: !clip.filters.is_empty(),
                    curved_speed: clip.speed_curve.is_some(),
                });
            }
            ("timeline:active".to_owned(), origin, target_end, sources)
        }
    };

    let count = ((target_end - origin) / bucket_seconds.max(0.02)).ceil() as usize;
    let count = count.max(1);
    let mut power = vec![0.0f64; count];
    let mut peak = vec![0.0f64; count];
    let mut zcr_power = vec![0.0f64; count];
    let mut warnings = Vec::new();
    for source in sources {
        if source.has_filters {
            warnings.push(format!(
                "{} has audio filters; analysis uses the decoded source before those filters",
                source.path
            ));
        }
        if source.curved_speed {
            warnings.push(format!(
                "{} has a speed curve; analysis maps it using the clip's mean speed",
                source.path
            ));
        }
        let decoded = analyze_file(
            Path::new(&source.path),
            source.source_start,
            Some(source.source_duration),
            bucket_seconds * source.speed,
        )?;
        for bucket in decoded {
            let at = source.timeline_start + bucket.start / source.speed;
            if at < origin || at >= target_end {
                continue;
            }
            let index = ((at - origin) / bucket_seconds.max(0.02)).floor() as usize;
            if index >= count {
                continue;
            }
            let contribution = bucket.rms * source.volume.max(0.0);
            let squared = contribution * contribution;
            power[index] += squared;
            peak[index] = peak[index].max(bucket.peak * source.volume.max(0.0));
            zcr_power[index] += bucket.zcr * squared;
        }
    }
    let width = bucket_seconds.clamp(0.02, 2.0);
    let features: Vec<FeatureBucket> = (0..count)
        .map(|index| {
            let start = index as f64 * width;
            let end = ((index + 1) as f64 * width).min(target_end - origin);
            FeatureBucket {
                start,
                end,
                rms: power[index].sqrt().min(1.0),
                peak: peak[index].min(1.0),
                zcr: if power[index] > 0.0 {
                    zcr_power[index] / power[index]
                } else {
                    0.0
                },
            }
        })
        .collect();
    warnings.sort();
    warnings.dedup();
    Ok(summarize(
        target,
        origin,
        features,
        width,
        silence_threshold_db,
        min_silence,
        include_buckets,
        warnings,
    ))
}

/// What a silence scan found, and the cuts it proposes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceScan {
    /// The spans to cut, latest first, in timeline seconds before any cut.
    pub cuts: Vec<AudioInterval>,
    /// Total seconds those spans cover.
    pub removed_seconds: f64,
    /// Caveats from the analysis, e.g. clips whose filters it ignored.
    pub warnings: Vec<String>,
}

/// Finds the silence worth cutting in the active timeline's audible mix, or in
/// one clip's stretch of it: analyses the audio, then shrinks every silent
/// run by `padding` so speech keeps a breath of room. Whatever has no sound
/// under it counts as silent, so a picture-only stretch is found too - scope
/// it with `clip_id` or a time range.
pub fn scan_silence(
    project: &Project,
    clip_id: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    silence_threshold_db: f64,
    min_silence: f64,
    padding: f64,
) -> Result<SilenceScan, String> {
    let analysis = analyze_audio(
        project,
        None,
        clip_id,
        start,
        end,
        0.05,
        silence_threshold_db,
        min_silence,
        false,
    )?;
    let cuts = silence_cuts(&analysis.silence, padding, 0.05);
    Ok(SilenceScan {
        removed_seconds: cuts.iter().map(|cut| cut.duration).sum(),
        cuts,
        warnings: analysis.warnings,
    })
}

/// The edit that removes these cuts: one `rippleDeleteRange` each, on the
/// named lanes or on every lane when `track_ids` is empty. Apply them in the
/// order given - latest first - as one transaction.
pub fn silence_commands(cuts: &[AudioInterval], track_ids: &[String]) -> Vec<Command> {
    cuts.iter()
        .map(|cut| Command::RippleDeleteRange {
            start: cut.start,
            end: cut.end,
            track_ids: track_ids.to_vec(),
            marker_policy: Default::default(),
        })
        .collect()
}

/// Where a scene scan looks: its label, the file, the source span, and how
/// source seconds become the seconds the caller wants.
type SceneTarget = (String, String, f64, Option<f64>, Box<dyn Fn(f64) -> f64>);

/// What a scene scan found.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneScan {
    /// What was analysed: `media:<id>` or `clip:<id>`.
    pub target: String,
    /// The cuts, earliest first: source seconds for a media item, timeline
    /// seconds for a clip.
    pub cuts: Vec<SceneCut>,
    /// Caveats, e.g. a speed curve the mapping ignores.
    pub warnings: Vec<String>,
}

/// Finds the cuts in a bin item's video, or in the stretch of it one
/// timeline clip shows, by comparing consecutive frames.
pub fn scan_scenes(
    project: &Project,
    media_id: Option<String>,
    clip_id: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    threshold: f64,
    min_scene: f64,
) -> Result<SceneScan, String> {
    let mut warnings = Vec::new();
    let (label, path, source_start, source_end, to_target): SceneTarget = match (media_id, clip_id)
    {
        (Some(_), Some(_)) => return Err("choose mediaId or clipId, not both".to_owned()),
        (Some(id), None) => {
            let media = project
                .media_by_id(&id)
                .ok_or_else(|| format!("media {id:?} is not in the bin"))?;
            if matches!(media.kind, MediaKind::Audio) {
                return Err(format!("media {id:?} has no picture"));
            }
            (
                format!("media:{id}"),
                media.path.clone(),
                start.unwrap_or(0.0).max(0.0),
                end.or(media.duration),
                Box::new(|time| time),
            )
        }
        (None, Some(id)) => {
            let clip = project
                .active()
                .clip(&id)
                .ok_or_else(|| format!("clip {id:?} is not on the active timeline"))?;
            if !matches!(clip.kind, ClipKind::Video) {
                return Err(format!("clip {id:?} is not a video clip"));
            }
            let media = project
                .media_by_id(&clip.media_id)
                .ok_or_else(|| format!("clip {id:?} has no media"))?;
            if clip.speed_curve.is_some() || clip.reverse {
                warnings.push(
                    "the clip has a speed curve or plays in reverse; cut times use its mean speed"
                        .to_owned(),
                );
            }
            let speed = clip.speed.max(0.0625);
            let (clip_start, clip_end, clip_source) =
                (clip.start, clip.start + clip.duration, clip.source_start);
            let from = start.unwrap_or(clip_start).clamp(clip_start, clip_end);
            let to = end.unwrap_or(clip_end).clamp(from, clip_end);
            (
                format!("clip:{id}"),
                media.path.clone(),
                clip_source + (from - clip_start) * speed,
                Some(clip_source + (to - clip_start) * speed),
                Box::new(move |time| clip_start + (time - clip_source) / speed),
            )
        }
        (None, None) => return Err("give a mediaId or a clipId".to_owned()),
    };
    let cuts = detect_scenes(
        Path::new(&path),
        source_start,
        source_end,
        threshold,
        min_scene,
    )?
    .into_iter()
    .map(|cut| SceneCut {
        time: to_target(cut.time),
        score: cut.score,
    })
    .collect();
    Ok(SceneScan {
        target: label,
        cuts,
        warnings,
    })
}

/// The edit that drops a "Cut N" marker at each of a clip's cuts.
pub fn marker_commands(cuts: &[SceneCut]) -> Vec<Command> {
    cuts.iter()
        .enumerate()
        .map(|(index, cut)| Command::AddMarker {
            time: cut.time,
            name: Some(format!("Cut {}", index + 1)),
            clip_id: None,
        })
        .collect()
}

/// What a beat scan found.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatScan {
    /// What was analysed: `media:<id>`, `clip:<id>` or `timeline:active`.
    pub target: String,
    /// The estimated tempo, when the beats are regular enough to have one.
    pub bpm: Option<f64>,
    /// Where the beats fall, earliest first: source seconds for a media
    /// item, timeline seconds otherwise.
    pub beats: Vec<f64>,
    /// Caveats from the analysis.
    pub warnings: Vec<String>,
}

/// Finds the beats in a bin item, one clip, or the timeline's audible mix:
/// the places where the sound suddenly gets louder, kept apart by at least a
/// beat's worth of time. `sensitivity` is `0.0..=1.0`; higher finds softer
/// onsets and more of them.
pub fn scan_beats(
    project: &Project,
    media_id: Option<String>,
    clip_id: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    sensitivity: f64,
) -> Result<BeatScan, String> {
    let analysis = analyze_audio(
        project, media_id, clip_id, start, end, 0.02, -60.0, 1.0, true,
    )?;
    let envelope: Vec<(f64, f64)> = analysis
        .buckets
        .unwrap_or_default()
        .iter()
        .map(|bucket| (bucket.start, bucket.rms_db))
        .collect();
    let (beats, bpm) = beats_from_envelope(&envelope, analysis.bucket_seconds, sensitivity);
    Ok(BeatScan {
        target: analysis.target,
        bpm,
        beats,
        warnings: analysis.warnings,
    })
}

/// The beats in a loudness envelope of `(time, dB)` points `step` seconds
/// apart, and the tempo they imply. Onset strength is how much louder each
/// point is than the one before; a beat is an onset that is a local peak,
/// clearly above the typical onset, and at least 0.25 s after the last beat.
pub fn beats_from_envelope(
    envelope: &[(f64, f64)],
    step: f64,
    sensitivity: f64,
) -> (Vec<f64>, Option<f64>) {
    if envelope.len() < 4 || step <= 0.0 {
        return (Vec::new(), None);
    }
    let strength: Vec<f64> = std::iter::once(0.0)
        .chain(
            envelope
                .windows(2)
                .map(|pair| (pair[1].1 - pair[0].1).max(0.0)),
        )
        .collect();
    let mean = strength.iter().sum::<f64>() / strength.len() as f64;
    let deviation =
        (strength.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / strength.len() as f64).sqrt();
    // From 1 deviation above the mean at the least sensitive to 0.25 at the
    // most, and never below a 6 dB jump: quiet noise is not a beat.
    let bar = (mean + deviation * (1.0 - sensitivity.clamp(0.0, 1.0) * 0.75)).max(6.0);
    let gap = (0.25 / step).ceil() as usize;
    let mut picked: Vec<usize> = Vec::new();
    for index in 1..strength.len() - 1 {
        let local_peak =
            strength[index] >= strength[index - 1] && strength[index] > strength[index + 1];
        if !local_peak || strength[index] < bar {
            continue;
        }
        match picked.last().copied() {
            Some(last) if index - last < gap => {
                // Too close: the stronger of the two is the beat.
                if strength[index] > strength[last] {
                    picked.pop();
                    picked.push(index);
                }
            }
            _ => picked.push(index),
        }
    }
    let beats: Vec<f64> = picked.iter().map(|&index| envelope[index].0).collect();
    (beats.clone(), tempo_of(&beats))
}

/// The tempo of a run of beats: the median gap, folded into 70-180 BPM, when
/// at least two thirds of the gaps agree with it to within a tenth.
fn tempo_of(beats: &[f64]) -> Option<f64> {
    if beats.len() < 4 {
        return None;
    }
    let mut gaps: Vec<f64> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();
    gaps.sort_by(f64::total_cmp);
    let median = gaps[gaps.len() / 2];
    let agreeing = gaps
        .iter()
        .filter(|gap| (*gap / median - 1.0).abs() < 0.1)
        .count();
    if (agreeing as f64) < gaps.len() as f64 * 2.0 / 3.0 {
        return None;
    }
    let mut bpm = 60.0 / median;
    while bpm < 70.0 {
        bpm *= 2.0;
    }
    while bpm > 180.0 {
        bpm /= 2.0;
    }
    Some((bpm * 10.0).round() / 10.0)
}

/// The edit that drops a "Beat N" marker at each beat.
pub fn beat_marker_commands(beats: &[f64]) -> Vec<Command> {
    beats
        .iter()
        .enumerate()
        .map(|(index, time)| Command::AddMarker {
            time: *time,
            name: Some(format!("Beat {}", index + 1)),
            clip_id: None,
        })
        .collect()
}

/// The spans worth cutting for a set of silent intervals: each shrunk by
/// `padding` seconds at both ends, so speech keeps a breath of room, with the
/// ones left under `minimum` seconds dropped. Latest first, which is the
/// order to cut in: a ripple only moves what comes after it, so earlier
/// times stay where the analysis found them.
pub fn silence_cuts(silence: &[AudioInterval], padding: f64, minimum: f64) -> Vec<AudioInterval> {
    let padding = padding.max(0.0);
    let mut cuts: Vec<AudioInterval> = silence
        .iter()
        .filter_map(|interval| {
            let start = interval.start + padding;
            let end = interval.end - padding;
            (end - start >= minimum.max(0.05)).then_some(AudioInterval {
                start,
                end,
                duration: end - start,
            })
        })
        .collect();
    cuts.sort_by(|a, b| b.start.total_cmp(&a.start));
    cuts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_cuts_need_the_threshold_and_room_between_them() {
        // 10 fps scores: a hard cut at 1.0, a flash right after it at 1.1,
        // low motion, then another real cut at 3.0.
        let scores = [(0.1, 0.01), (1.0, 0.6), (1.1, 0.5), (2.0, 0.08), (3.0, 0.4)];
        let cuts = cuts_from_scores(&scores, 0.1, 0.15, 0.5);
        let times: Vec<f64> = cuts.iter().map(|c| c.time).collect();
        assert_eq!(times, vec![1.0, 3.0]);
    }

    #[test]
    fn a_steady_pulse_is_found_as_beats_with_a_tempo() {
        // A 20 ms envelope at -50 dB with a 20 dB click every half second.
        let envelope: Vec<(f64, f64)> = (0..500)
            .map(|i| {
                let time = i as f64 * 0.02;
                let on_beat = (time / 0.5).fract() < 0.04 || (time / 0.5).fract() > 0.99;
                (time, if on_beat { -30.0 } else { -50.0 })
            })
            .collect();
        let (beats, bpm) = beats_from_envelope(&envelope, 0.02, 0.5);
        assert!(beats.len() >= 18, "{beats:?}");
        for beat in &beats {
            let off = (beat / 0.5).fract().min(1.0 - (beat / 0.5).fract());
            assert!(off < 0.08, "{beat} is not on the pulse");
        }
        assert_eq!(bpm, Some(120.0));
        // Silence has no beats.
        let flat: Vec<(f64, f64)> = (0..100).map(|i| (i as f64 * 0.02, -50.0)).collect();
        assert!(beats_from_envelope(&flat, 0.02, 1.0).0.is_empty());
    }

    #[test]
    fn cuts_keep_padding_drop_short_remnants_and_run_latest_first() {
        let silence = |start: f64, end: f64| AudioInterval {
            start,
            end,
            duration: end - start,
        };
        let cuts = silence_cuts(
            &[silence(1.0, 3.0), silence(5.0, 5.4), silence(8.0, 10.0)],
            0.25,
            0.3,
        );
        let spans: Vec<(f64, f64)> = cuts.iter().map(|c| (c.start, c.end)).collect();
        assert_eq!(spans, vec![(8.25, 9.75), (1.25, 2.75)]);
    }

    #[test]
    fn silence_runs_are_merged_and_short_runs_are_ignored() {
        let features = vec![
            FeatureBucket {
                start: 0.0,
                end: 0.1,
                rms: 0.0,
                peak: 0.0,
                zcr: 0.0,
            },
            FeatureBucket {
                start: 0.1,
                end: 0.2,
                rms: 0.0,
                peak: 0.0,
                zcr: 0.0,
            },
            FeatureBucket {
                start: 0.2,
                end: 0.3,
                rms: 0.2,
                peak: 0.3,
                zcr: 0.08,
            },
            FeatureBucket {
                start: 0.3,
                end: 0.4,
                rms: 0.0,
                peak: 0.0,
                zcr: 0.0,
            },
        ];
        let result = summarize(
            "test".into(),
            4.0,
            features,
            0.1,
            -35.0,
            0.15,
            false,
            vec![],
        );
        assert_eq!(result.silence.len(), 1);
        assert!((result.silence[0].start - 4.0).abs() < 1e-9);
        assert!((result.silence[0].end - 4.2).abs() < 1e-9);
        assert!((result.silence[0].duration - 0.2).abs() < 1e-9);
        assert!(result.buckets.is_none());
        assert!(result.active_fraction > 0.2 && result.active_fraction < 0.3);
    }
}
