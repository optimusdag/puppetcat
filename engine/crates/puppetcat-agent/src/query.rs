// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Compact, structured reads and invariant checks for agent callers. The
//! project document remains internal: callers ask for exactly the slice they
//! need instead of reading `puppetcat.json` or receiving the whole state.

#![allow(missing_docs)] // Serialized query fields are the public wire schema.

use std::collections::HashSet;
use std::path::Path;

use puppetcat_project::Project;
use puppetcat_project::model::{Clip, ClipKind, Marker, MediaItem, Timeline};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSummary {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub muted: bool,
    pub clip_count: usize,
    pub start: Option<f64>,
    pub end: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineSummary {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub duration: f64,
    pub track_count: usize,
    pub clip_count: usize,
    pub marker_count: usize,
    pub tracks: Vec<TrackSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clips: Option<Vec<Clip>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markers: Option<Vec<Marker>>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub severity: &'static str,
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: usize,
    pub warnings: usize,
    pub timelines: usize,
    pub media: usize,
    pub issues: Vec<ValidationIssue>,
}

#[allow(clippy::too_many_arguments)]
pub fn timeline_summary(
    project: &Project,
    timeline_id: Option<&str>,
    include_clips: bool,
    include_markers: bool,
    track_id: Option<&str>,
    start: Option<f64>,
    end: Option<f64>,
    limit: usize,
) -> Result<TimelineSummary, String> {
    let timeline = pick_timeline(project, timeline_id)?;
    let intersects = |clip: &&Clip| {
        track_id.is_none_or(|id| clip.track_id == id)
            && start.is_none_or(|at| clip.start + clip.duration > at)
            && end.is_none_or(|at| clip.start < at)
    };
    let mut selected: Vec<Clip> = timeline.clips.iter().filter(intersects).cloned().collect();
    selected.sort_by(|left, right| left.start.total_cmp(&right.start));
    let truncated = selected.len() > limit;
    selected.truncate(limit);
    let tracks = timeline
        .tracks
        .iter()
        .filter(|track| track_id.is_none_or(|id| track.id == id))
        .map(|track| {
            let clips: Vec<&Clip> = timeline
                .clips
                .iter()
                .filter(|clip| clip.track_id == track.id)
                .collect();
            TrackSummary {
                id: track.id.clone(),
                name: track.name.clone(),
                visible: track.visible,
                muted: track.muted,
                clip_count: clips.len(),
                start: clips.iter().map(|clip| clip.start).min_by(f64::total_cmp),
                end: clips
                    .iter()
                    .map(|clip| clip.start + clip.duration)
                    .max_by(f64::total_cmp),
            }
        })
        .collect();
    Ok(TimelineSummary {
        id: timeline.id.clone(),
        name: timeline.name.clone(),
        active: timeline.id == project.active_timeline_id,
        duration: timeline.duration(),
        track_count: timeline.tracks.len(),
        clip_count: timeline.clips.len(),
        marker_count: timeline.markers.len(),
        tracks,
        clips: include_clips.then_some(selected),
        markers: include_markers.then(|| timeline.markers.iter().take(limit).cloned().collect()),
        truncated,
    })
}

pub fn clip(project: &Project, clip_id: &str, timeline_id: Option<&str>) -> Result<Clip, String> {
    if let Some(id) = timeline_id {
        return pick_timeline(project, Some(id))?
            .clip(clip_id)
            .cloned()
            .ok_or_else(|| format!("clip {clip_id:?} is not on timeline {id:?}"));
    }
    project
        .timelines
        .iter()
        .find_map(|timeline| timeline.clip(clip_id).cloned())
        .ok_or_else(|| format!("clip {clip_id:?} is not in the project"))
}

pub fn media(project: &Project, media_id: &str) -> Result<MediaItem, String> {
    project
        .media_by_id(media_id)
        .cloned()
        .ok_or_else(|| format!("media {media_id:?} is not in the bin"))
}

pub fn validate(project: &Project, timeline_id: Option<&str>) -> Result<ValidationReport, String> {
    let timelines: Vec<&Timeline> = if let Some(id) = timeline_id {
        vec![pick_timeline(project, Some(id))?]
    } else {
        project.timelines.iter().collect()
    };
    let mut issues = Vec::new();
    if !project
        .timelines
        .iter()
        .any(|timeline| timeline.id == project.active_timeline_id)
    {
        issue(
            &mut issues,
            "error",
            "missing-active-timeline",
            "The active timeline id names no timeline.".to_owned(),
            None,
            Some(project.active_timeline_id.clone()),
        );
    }
    let media_ids: HashSet<&str> = project.media.iter().map(|item| item.id.as_str()).collect();
    for item in &project.media {
        if !item.path.is_empty() && !Path::new(&item.path).exists() {
            issue(
                &mut issues,
                "warning",
                "missing-media-file",
                format!("{} does not exist on disk.", item.path),
                None,
                Some(item.id.clone()),
            );
        }
    }
    for timeline in &timelines {
        let track_ids: HashSet<&str> = timeline
            .tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect();
        let clip_ids: HashSet<&str> = timeline.clips.iter().map(|clip| clip.id.as_str()).collect();
        for clip in &timeline.clips {
            let tid = Some(timeline.id.clone());
            let oid = Some(clip.id.clone());
            if !track_ids.contains(clip.track_id.as_str()) {
                issue(
                    &mut issues,
                    "error",
                    "missing-track",
                    format!("Clip {} names missing track {}.", clip.id, clip.track_id),
                    tid.clone(),
                    oid.clone(),
                );
            }
            if !matches!(clip.kind, ClipKind::Text | ClipKind::Layer)
                && !media_ids.contains(clip.media_id.as_str())
            {
                issue(
                    &mut issues,
                    "error",
                    "missing-media",
                    format!("Clip {} names missing media {}.", clip.id, clip.media_id),
                    tid.clone(),
                    oid.clone(),
                );
            }
            if !clip.start.is_finite() || clip.start < 0.0 {
                issue(
                    &mut issues,
                    "error",
                    "invalid-start",
                    format!("Clip {} has invalid start {}.", clip.id, clip.start),
                    tid.clone(),
                    oid.clone(),
                );
            }
            if !clip.duration.is_finite() || clip.duration <= 0.0 {
                issue(
                    &mut issues,
                    "error",
                    "invalid-duration",
                    format!("Clip {} has invalid duration {}.", clip.id, clip.duration),
                    tid.clone(),
                    oid.clone(),
                );
            }
            if let Some(transition) = &clip.transition_in
                && (transition.duration < 0.0 || transition.duration > clip.duration)
            {
                issue(
                    &mut issues,
                    "warning",
                    "transition-too-long",
                    format!(
                        "Clip {} transition is {}s for a {}s clip.",
                        clip.id, transition.duration, clip.duration
                    ),
                    tid,
                    oid,
                );
            }
        }
        for marker in &timeline.markers {
            if let Some(clip_id) = marker.clip_id.as_deref()
                && !clip_ids.contains(clip_id)
            {
                issue(
                    &mut issues,
                    "error",
                    "marker-missing-clip",
                    format!(
                        "Marker {} is attached to missing clip {}.",
                        marker.id, clip_id
                    ),
                    Some(timeline.id.clone()),
                    Some(marker.id.clone()),
                );
            }
            if !marker.time.is_finite() || marker.time < 0.0 {
                issue(
                    &mut issues,
                    "error",
                    "invalid-marker-time",
                    format!("Marker {} has invalid time {}.", marker.id, marker.time),
                    Some(timeline.id.clone()),
                    Some(marker.id.clone()),
                );
            }
        }
    }
    let errors = issues
        .iter()
        .filter(|issue| issue.severity == "error")
        .count();
    let warnings = issues.len() - errors;
    Ok(ValidationReport {
        valid: errors == 0,
        errors,
        warnings,
        timelines: timelines.len(),
        media: project.media.len(),
        issues,
    })
}

fn pick_timeline<'a>(
    project: &'a Project,
    timeline_id: Option<&str>,
) -> Result<&'a Timeline, String> {
    let id = timeline_id.unwrap_or(&project.active_timeline_id);
    project
        .timelines
        .iter()
        .find(|timeline| timeline.id == id)
        .ok_or_else(|| format!("timeline {id:?} is not in the project"))
}

fn issue(
    issues: &mut Vec<ValidationIssue>,
    severity: &'static str,
    code: &'static str,
    message: String,
    timeline_id: Option<String>,
    object_id: Option<String>,
) {
    issues.push(ValidationIssue {
        severity,
        code,
        message,
        timeline_id,
        object_id,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_project_has_a_small_valid_summary() {
        let project = Project::new();
        let summary = timeline_summary(&project, None, true, true, None, None, None, 10).unwrap();
        assert_eq!(summary.track_count, 4);
        assert_eq!(summary.clip_count, 0);
        assert!(!summary.truncated);
        let report = validate(&project, None).unwrap();
        assert!(report.valid);
        assert!(report.issues.is_empty());
    }
}
