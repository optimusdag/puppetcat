// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Audio facts for agents: loudness, silence/dead-air intervals and a
//! deliberately labelled speech-vs-music heuristic. Decoding stays in the
//! engine through `puppetcat-media`; no shelling out to ffmpeg or parsing its
//! stderr.

#![allow(missing_docs)] // Serialized analysis fields are the public wire schema.

use std::path::Path;

use puppetcat_media::{AudioDecoder, AudioOptions, SampleFormat};
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
pub(crate) struct FeatureBucket {
    pub start: f64,
    pub end: f64,
    pub rms: f64,
    pub peak: f64,
    pub zcr: f64,
}

pub(crate) fn analyze_file(
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

pub(crate) fn summarize(
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

#[cfg(test)]
mod tests {
    use super::*;

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
