// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The Layouts sheet: a picture's placement saved under a name and stamped onto
//! other clips, or carried to another project as a file.
//!
//! Building a layout, choosing its clips, applying it and the file format are
//! `puppetcat_host::layouts`, which the agent's layout tools call too.

use puppetcat_host::layouts::{self, ClipSelector, LayoutOverrides};
use puppetcat_project::Command;
use puppetcat_project::model::{ClipKind, LayoutTemplate};

use super::Studio;
use crate::ui::{LayoutRowData, LayoutsData};

/// The Layouts sheet's state.
#[derive(Default)]
pub struct LayoutsState {
    pub open: bool,
    pub name: String,
}

fn summary(layout: &LayoutTemplate) -> String {
    let mut text = format!(
        "\u{d7}{:.2} \u{b7} x {:+.2} \u{b7} y {:+.2}",
        layout.scale, layout.offset_x, layout.offset_y
    );
    if layout.rotation.abs() > 0.01 {
        text.push_str(&format!(" \u{b7} {:.0}\u{b0}", layout.rotation));
    }
    if layout.opacity < 0.995 {
        text.push_str(&format!(" \u{b7} {:.0}%", layout.opacity * 100.0));
    }
    if layout.flip_h || layout.flip_v {
        text.push_str(" \u{b7} flipped");
    }
    if layout.crop.is_some() {
        text.push_str(" \u{b7} cropped");
    }
    text
}

impl Studio {
    /// The selected clips a layout can land on: the picture ones.
    fn layout_targets(&self) -> Vec<String> {
        self.selection
            .iter()
            .filter(|id| {
                self.clip(id)
                    .is_some_and(|clip| clip.kind != ClipKind::Audio)
            })
            .cloned()
            .collect()
    }

    /// The layout the selected clip would save as.
    pub fn save_layout(&mut self) {
        let Some(id) = self.sole_selection() else {
            self.notify("Select the clip whose placement to save", true);
            return;
        };
        let layout = match layouts::build_layout(
            self.project(),
            &self.layouts.name,
            Some(&id),
            &LayoutOverrides::default(),
        ) {
            Ok(layout) => layout,
            Err(error) => {
                self.notify(&error, true);
                return;
            }
        };
        let name = layout.name.clone();
        if self.apply_all(vec![Command::SaveLayoutTemplate { layout }]) {
            self.layouts.name.clear();
            self.notify(&format!("Saved layout {name:?}"), false);
        }
    }

    /// Stamps layout number `index` onto the selected pictures.
    pub fn apply_layout(&mut self, index: usize) {
        let Some(name) = self
            .project()
            .layouts
            .get(index)
            .map(|layout| layout.name.clone())
        else {
            return;
        };
        let ids = self.layout_targets();
        if ids.is_empty() {
            self.notify("Select a video or image clip first", true);
            return;
        }
        let selector = ClipSelector {
            clip_ids: ids,
            ..ClipSelector::default()
        };
        self.echo = None;
        self.last_commit = None;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let applied = layouts::select_clips(session.project().active(), &selector)
            .and_then(|ids| layouts::apply_layout(session, &name, ids, false));
        match applied {
            Ok(applied) => {
                self.after_change();
                self.notify(
                    &format!(
                        "Applied {name:?} to {} clip{}",
                        applied.changed,
                        if applied.changed == 1 { "" } else { "s" }
                    ),
                    false,
                );
            }
            Err(error) => self.notify(&error, true),
        }
    }

    pub fn remove_layout(&mut self, index: usize) {
        if let Some(name) = self
            .project()
            .layouts
            .get(index)
            .map(|layout| layout.name.clone())
        {
            self.apply_all(vec![Command::RemoveLayoutTemplate { name }]);
        }
    }

    pub fn export_layouts(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export layouts")
            .set_file_name("layouts.json")
            .save_file()
        else {
            return;
        };
        let result = layouts::write_layouts(
            &self.project().layouts.clone(),
            &path.to_string_lossy(),
            true,
        );
        match result {
            Ok(count) => self.notify(
                &format!(
                    "Exported {count} layout{}",
                    if count == 1 { "" } else { "s" }
                ),
                false,
            ),
            Err(error) => self.notify(&error, true),
        }
    }

    pub fn import_layouts(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import layouts")
            .add_filter("Layouts", &["json"])
            .pick_file()
        else {
            return;
        };
        self.echo = None;
        self.last_commit = None;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match layouts::import_layouts(session, &path.to_string_lossy(), false) {
            Ok(names) => {
                self.after_change();
                self.notify(
                    &format!(
                        "Imported {} layout{}",
                        names.len(),
                        if names.len() == 1 { "" } else { "s" }
                    ),
                    false,
                );
            }
            Err(error) => self.notify(&error, true),
        }
    }

    pub fn layout_rows(&self) -> Vec<LayoutRowData> {
        self.project()
            .layouts
            .iter()
            .map(|layout| LayoutRowData {
                name: layout.name.as_str().into(),
                summary: summary(layout).into(),
            })
            .collect()
    }

    pub fn layouts_data(&self) -> LayoutsData {
        let targets = self.layout_targets();
        let selection = match targets.as_slice() {
            [] => "Select a clip".to_owned(),
            [id] => self
                .clip(id)
                .map_or_else(String::new, |clip| clip.name.clone()),
            many => format!("{} clips selected", many.len()),
        };
        LayoutsData {
            open: self.layouts.open,
            name: self.layouts.name.as_str().into(),
            has_selection: !targets.is_empty(),
            selection: selection.into(),
        }
    }
}
