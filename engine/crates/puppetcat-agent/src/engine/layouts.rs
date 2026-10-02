// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Reusable picture layouts: save one from a clip or from numbers, stamp it
//! onto clips chosen by id, track or time span, and carry the set between
//! projects as a plain JSON file.

use std::path::Path;

use puppetcat_project::Command;
use puppetcat_project::model::{ClipKind, Crop, LayoutTemplate};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use super::{AgentEngine, no_project};

/// Explicit layout values; anything omitted keeps the neutral default or,
/// with a source clip, that clip's value.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutValues {
    /// Multiplier over the fitted size.
    pub scale: Option<f64>,
    /// Horizontal offset as a fraction of frame width (positive is right).
    pub offset_x: Option<f64>,
    /// Vertical offset as a fraction of frame height (positive is down).
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
    pub crop: Option<CropValues>,
}

/// Source crop as fractions.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct CropValues {
    /// Off the left.
    pub left: f64,
    /// Off the top.
    pub top: f64,
    /// Off the right.
    pub right: f64,
    /// Off the bottom.
    pub bottom: f64,
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
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyLayoutReport {
    /// Whether the project changed.
    pub committed: bool,
    /// Echo of the request flag.
    pub dry_run: bool,
    /// The clips the layout lands on.
    pub clip_ids: Vec<String>,
    /// Of those, how many actually change.
    pub changed: usize,
}

impl AgentEngine {
    /// Every saved layout.
    pub fn list_layout_templates(&self) -> Result<Vec<LayoutTemplate>, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        Ok(session.project().layouts.clone())
    }

    /// Saves a layout named `name`, starting from `from_clip` when given and
    /// then overriding with `values`.
    pub fn save_layout_template(
        &self,
        name: &str,
        from_clip: Option<&str>,
        values: LayoutValues,
    ) -> Result<LayoutTemplate, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
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
            let clip = session
                .project()
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
        layout.blend = values.blend.unwrap_or(layout.blend);
        if let Some(crop) = values.crop {
            layout.crop = Some(Crop {
                left: crop.left,
                top: crop.top,
                right: crop.right,
                bottom: crop.bottom,
            });
        }
        let saved = layout.clone();
        session.apply(Command::SaveLayoutTemplate { layout })?;
        Ok(saved)
    }

    /// Deletes a layout by name.
    pub fn remove_layout_template(&self, name: &str) -> Result<(), String> {
        self.apply(Command::RemoveLayoutTemplate {
            name: name.to_owned(),
        })
        .map(|_| ())
    }

    /// Stamps layout `name` onto the selected clips as one undo step, or
    /// previews which clips it would touch.
    pub fn apply_layout_template(
        &self,
        name: &str,
        selector: ClipSelector,
        dry_run: bool,
    ) -> Result<ApplyLayoutReport, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        let timeline = session.project().active();
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
        let command = Command::ApplyLayoutTemplate {
            name: name.to_owned(),
            clip_ids: ids.clone(),
        };
        let staged = session.stage(vec![command])?;
        let changed = staged
            .project
            .active()
            .clips
            .iter()
            .filter(|after| ids.contains(&after.id))
            .filter(|after| {
                timeline
                    .clips
                    .iter()
                    .find(|before| before.id == after.id)
                    .is_some_and(|before| before != *after)
            })
            .count();
        let committed = !dry_run && staged.applied;
        if !dry_run {
            session.commit(staged);
        }
        Ok(ApplyLayoutReport {
            committed,
            dry_run,
            clip_ids: ids,
            changed,
        })
    }

    /// Writes every layout to `path` as JSON. Refuses to replace a file
    /// unless `overwrite`.
    pub fn export_layout_templates(&self, path: &str, overwrite: bool) -> Result<usize, String> {
        if Path::new(path).exists() && !overwrite {
            return Err(format!(
                "{path} already exists; pass overwrite to replace it"
            ));
        }
        let layouts = self.list_layout_templates()?;
        let bytes = serde_json::to_vec_pretty(&layouts).map_err(|e| e.to_string())?;
        let partial = format!("{path}.part");
        std::fs::write(&partial, bytes).map_err(|e| format!("could not write {partial}: {e}"))?;
        std::fs::rename(&partial, path).map_err(|e| format!("could not install {path}: {e}"))?;
        Ok(layouts.len())
    }

    /// Reads layouts from a file written by `export_layout_templates`. A
    /// name already in the project is skipped unless `overwrite`. One undo
    /// step. Returns the names imported.
    pub fn import_layout_templates(
        &self,
        path: &str,
        overwrite: bool,
    ) -> Result<Vec<String>, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("could not read {path}: {e}"))?;
        let incoming: Vec<LayoutTemplate> = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{path} is not a layout file: {e}"))?;
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
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
}
