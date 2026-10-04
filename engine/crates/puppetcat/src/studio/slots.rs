// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Template slots: bin items marked as stand-ins that someone else's file will
//! take the place of, which is what makes a project a template.
//!
//! The commands are the engine's `SetMediaPlaceholder` and `FillSlot` - the
//! same ones the agent reaches through `apply_raw_command`.

use puppetcat_project::Command;

use super::Studio;
use crate::host::spawn;

impl Studio {
    /// The selected bin items.
    fn selected_media(&self) -> Vec<String> {
        self.project()
            .media
            .iter()
            .filter(|item| self.media_selected.contains(&item.id))
            .map(|item| item.id.clone())
            .collect()
    }

    /// Whether every selected bin item is a slot (and there is one).
    pub fn selection_is_slots(&self) -> bool {
        let selected = self.selected_media();
        !selected.is_empty()
            && selected.iter().all(|id| {
                self.project()
                    .media
                    .iter()
                    .any(|item| &item.id == id && item.placeholder)
            })
    }

    /// Marks the selected bin items as slots, or - if they all already are -
    /// turns them back into ordinary media.
    pub fn toggle_slots(&mut self) {
        let ids = self.selected_media();
        if ids.is_empty() {
            self.notify("Select media in the bin first", true);
            return;
        }
        let placeholder = !self.selection_is_slots();
        let commands: Vec<Command> = ids
            .into_iter()
            .map(|media_id| Command::SetMediaPlaceholder {
                media_id,
                placeholder,
            })
            .collect();
        self.apply_all(commands);
    }

    /// The one selected slot, if exactly one slot is selected.
    pub fn fillable_slot(&self) -> Option<String> {
        let selected = self.selected_media();
        match selected.as_slice() {
            [id] => self
                .project()
                .media
                .iter()
                .any(|item| &item.id == id && item.placeholder)
                .then(|| id.clone()),
            _ => None,
        }
    }

    /// Swaps a file of the user's into the selected slot, in place: every clip
    /// that used the slot now plays the file.
    pub fn fill_slot(&mut self) {
        let Some(media_id) = self.fillable_slot() else {
            self.notify("Select one template slot in the bin first", true);
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Choose the file for this slot")
            .pick_file()
        else {
            return;
        };
        let path = path.to_string_lossy().into_owned();
        spawn(
            move || puppetcat_host::media::probe(&path),
            move |studio, _, _, result| match result {
                Ok(summary) => {
                    if studio.apply_all(vec![Command::FillSlot {
                        media_id,
                        item: summary.to_new_media(),
                    }]) {
                        studio.notify("Slot filled", false);
                    }
                }
                Err(error) => studio.notify(&error, true),
            },
        );
    }
}
