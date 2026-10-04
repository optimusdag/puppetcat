// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Reusable picture layouts: save one from a clip or from numbers, stamp it
//! onto clips chosen by id, track or time span, and carry the set between
//! projects as a plain JSON file.

use puppetcat_host::layouts;
use puppetcat_project::Command;
use puppetcat_project::model::{Crop, LayoutTemplate};
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

pub use puppetcat_host::layouts::ClipSelector;

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
        let overrides = layouts::LayoutOverrides {
            scale: values.scale,
            offset_x: values.offset_x,
            offset_y: values.offset_y,
            rotation: values.rotation,
            opacity: values.opacity,
            flip_h: values.flip_h,
            flip_v: values.flip_v,
            blend: values.blend,
            crop: values.crop.map(|crop| Crop {
                left: crop.left,
                top: crop.top,
                right: crop.right,
                bottom: crop.bottom,
            }),
        };
        let layout = layouts::build_layout(session.project(), name, from_clip, &overrides)?;
        session.apply(Command::SaveLayoutTemplate {
            layout: layout.clone(),
        })?;
        Ok(layout)
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
        let ids = layouts::select_clips(session.project().active(), &selector)?;
        let applied = layouts::apply_layout(session, name, ids, dry_run)?;
        Ok(ApplyLayoutReport {
            committed: applied.committed,
            dry_run,
            clip_ids: applied.clip_ids,
            changed: applied.changed,
        })
    }

    /// Writes every layout to `path` as JSON. Refuses to replace a file
    /// unless `overwrite`.
    pub fn export_layout_templates(&self, path: &str, overwrite: bool) -> Result<usize, String> {
        let layouts = self.list_layout_templates()?;
        layouts::write_layouts(&layouts, path, overwrite)
    }

    /// Reads layouts from a file written by `export_layout_templates`. A
    /// name already in the project is skipped unless `overwrite`. One undo
    /// step. Returns the names imported.
    pub fn import_layout_templates(
        &self,
        path: &str,
        overwrite: bool,
    ) -> Result<Vec<String>, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        layouts::import_layouts(session, path, overwrite)
    }
}
