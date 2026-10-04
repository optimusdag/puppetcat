// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Reusable picture layouts: save one from a clip or from numbers, stamp it
//! onto clips chosen by id, track or time span, and carry the set between
//! projects as a plain JSON file. The window's Layouts sheet and the agent's
//! layout tools both work through these.

use std::path::Path;

use puppetcat_project::Command;
use puppetcat_project::model::{ClipKind, Crop, LayoutTemplate, Project, Timeline};

use crate::session::Session;

/// Explicit layout values; anything omitted keeps the neutral default or,
/// with a source clip, that clip's value.
#[derive(Debug, Default, Clone)]
pub struct LayoutOverrides {
    /// Multiplier over the fitted size.
    pub scale: Option<f64>,
    /// Horizontal offset as a fraction of frame width.
    pub offset_x: Option<f64>,
    /// Vertical offset as a fraction of frame height.
    pub offset_y: Option<f64>,
    /// Clockwise degrees.
    pub rotation: Option<f64>,
    /// 0..1.
    pub opacity: Option<f64>,
    /// Mirror left to right.
    pub flip_h: Option<bool>,
    /// Mirror top to bottom.
    pub flip_v: Option<bool>,
    /// Blend mode name.
    pub blend: Option<String>,
    /// Fractions cut from each source edge.
    pub crop: Option<Crop>,
}

/// Which clips a layout lands on.
#[derive(Debug, Default, Clone)]
pub struct ClipSelector {
    /// Explicit clip ids.
    pub clip_ids: Vec<String>,
    /// Every visual clip on this track...
    pub track_id: Option<String>,
    /// ...that ends after this time...
    pub from: Option<f64>,
    /// ...and starts before this one.
    pub to: Option<f64>,
}

/// What applying (or previewing) a layout did.
#[derive(Debug, Clone)]
pub struct Applied {
    /// Whether the project changed.
    pub committed: bool,
    /// The clips the layout lands on.
    pub clip_ids: Vec<String>,
    /// Of those, how many actually change.
    pub changed: usize,
}

/// A layout named `name`, starting from `from_clip` when given and then
/// overridden by `values`.
pub fn build_layout(
    project: &Project,
    name: &str,
    from_clip: Option<&str>,
    values: &LayoutOverrides,
) -> Result<LayoutTemplate, String> {
    let mut layout = LayoutTemplate {
        name: name.trim().to_owned(),
        scale: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        flip_h: false,
        flip_v: false,
        blend: String::new(),
        crop: None,
    };
    if layout.name.is_empty() {
        return Err("a layout needs a name".to_owned());
    }
    if let Some(id) = from_clip {
        let clip = project
            .active()
            .clips
            .iter()
            .find(|clip| clip.id == id)
            .ok_or_else(|| format!("no clip {id} on the active timeline"))?;
        layout.scale = clip.scale;
        layout.offset_x = clip.offset_x;
        layout.offset_y = clip.offset_y;
        layout.rotation = clip.rotation;
        layout.opacity = clip.opacity;
        layout.flip_h = clip.flip_h;
        layout.flip_v = clip.flip_v;
        layout.blend = clip.blend.clone();
        layout.crop = clip.crop;
    }
    layout.scale = values.scale.unwrap_or(layout.scale);
    layout.offset_x = values.offset_x.unwrap_or(layout.offset_x);
    layout.offset_y = values.offset_y.unwrap_or(layout.offset_y);
    layout.rotation = values.rotation.unwrap_or(layout.rotation);
    layout.opacity = values.opacity.unwrap_or(layout.opacity);
    layout.flip_h = values.flip_h.unwrap_or(layout.flip_h);
    layout.flip_v = values.flip_v.unwrap_or(layout.flip_v);
    layout.blend = values.blend.clone().unwrap_or(layout.blend);
    if values.crop.is_some() {
        layout.crop = values.crop;
    }
    Ok(layout)
}

/// The visual clips a selector names, in the order found.
pub fn select_clips(timeline: &Timeline, selector: &ClipSelector) -> Result<Vec<String>, String> {
    let mut ids: Vec<String> = selector.clip_ids.clone();
    for id in &ids {
        if !timeline.clips.iter().any(|clip| &clip.id == id) {
            return Err(format!("no clip {id} on the active timeline"));
        }
    }
    if let Some(track) = &selector.track_id {
        if timeline.track(track).is_none() {
            return Err(format!("no track {track} on the active timeline"));
        }
        for clip in &timeline.clips {
            if &clip.track_id == track
                && selector
                    .from
                    .is_none_or(|from| clip.start + clip.duration > from)
                && selector.to.is_none_or(|to| clip.start < to)
                && !ids.contains(&clip.id)
            {
                ids.push(clip.id.clone());
            }
        }
    }
    ids.retain(|id| {
        timeline
            .clips
            .iter()
            .any(|clip| &clip.id == id && !matches!(clip.kind, ClipKind::Audio))
    });
    if ids.is_empty() {
        return Err("the selector matched no visual clips".to_owned());
    }
    Ok(ids)
}

/// Stamps layout `name` onto `ids` as one undo step, or - for a dry run -
/// reports what it would change.
pub fn apply_layout(
    session: &mut Session,
    name: &str,
    ids: Vec<String>,
    dry_run: bool,
) -> Result<Applied, String> {
    let before = session.project().active().clone();
    let staged = session.stage(vec![Command::ApplyLayoutTemplate {
        name: name.to_owned(),
        clip_ids: ids.clone(),
    }])?;
    let changed = staged
        .project
        .active()
        .clips
        .iter()
        .filter(|after| ids.contains(&after.id))
        .filter(|after| {
            before
                .clips
                .iter()
                .find(|earlier| earlier.id == after.id)
                .is_some_and(|earlier| earlier != *after)
        })
        .count();
    let committed = !dry_run && staged.applied;
    if !dry_run {
        session.commit(staged);
    }
    Ok(Applied {
        committed,
        clip_ids: ids,
        changed,
    })
}

/// Writes `layouts` to `path` as JSON. Refuses to replace a file unless
/// `overwrite`. Returns how many were written.
pub fn write_layouts(
    layouts: &[LayoutTemplate],
    path: &str,
    overwrite: bool,
) -> Result<usize, String> {
    if Path::new(path).exists() && !overwrite {
        return Err(format!(
            "{path} already exists; pass overwrite to replace it"
        ));
    }
    let bytes = serde_json::to_vec_pretty(layouts).map_err(|e| e.to_string())?;
    let partial = format!("{path}.part");
    std::fs::write(&partial, bytes).map_err(|e| format!("could not write {partial}: {e}"))?;
    std::fs::rename(&partial, path).map_err(|e| format!("could not install {path}: {e}"))?;
    Ok(layouts.len())
}

/// Reads layouts from a file written by [`write_layouts`] and saves them into
/// the project as one undo step. A name already in the project is skipped
/// unless `overwrite`. Returns the names imported.
pub fn import_layouts(
    session: &mut Session,
    path: &str,
    overwrite: bool,
) -> Result<Vec<String>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {path}: {e}"))?;
    let incoming: Vec<LayoutTemplate> =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path} is not a layout file: {e}"))?;
    let existing: Vec<String> = session
        .project()
        .layouts
        .iter()
        .map(|layout| layout.name.clone())
        .collect();
    let chosen: Vec<LayoutTemplate> = incoming
        .into_iter()
        .filter(|layout| overwrite || !existing.contains(&layout.name))
        .collect();
    let names = chosen.iter().map(|layout| layout.name.clone()).collect();
    let staged = session.stage(
        chosen
            .into_iter()
            .map(|layout| Command::SaveLayoutTemplate { layout })
            .collect(),
    )?;
    session.commit(staged);
    Ok(names)
}
