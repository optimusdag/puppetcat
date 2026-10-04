// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Ripple editing: deleting, trimming and inserting so that the time after the
//! edit closes up or opens out with it.
//!
//! With Ripple on, the edits that normally leave a hole - Delete, dragging a
//! clip's edge, dropping media from the bin - become the engine's ripple
//! commands, under the three policies the agent's `apply_edit_plan` names:
//! which lanes follow, what becomes of linked audio, what markers do. Insert
//! gap is the same machinery as a one-off.

use puppetcat_project::Command;
use puppetcat_project::commands::{LinkPolicy, MarkerPolicy, RipplePolicy};

use super::Studio;
use crate::ui::{BookendsData, GapData};

/// The ripple settings and the insert-gap sheet.
pub struct RippleState {
    /// Delete, trim and drop-from-bin ripple.
    pub on: bool,
    /// Every lane follows, not just the lanes the edit touched.
    pub all_lanes: bool,
    /// Linked audio and picture move together.
    pub linked: bool,
    /// Free markers follow the edit.
    pub markers: bool,
    pub gap_open: bool,
    pub gap_seconds: f32,
}

/// The fade-bookends sheet.
pub struct BookendsState {
    pub open: bool,
    pub duration: f32,
    pub fade: f32,
    pub shift_all: bool,
    pub shift_markers: bool,
    pub error: String,
}

impl Default for BookendsState {
    fn default() -> Self {
        // The defaults the agent's add_fade_to_black tool starts from.
        Self {
            open: false,
            duration: 1.0,
            fade: 0.5,
            shift_all: true,
            shift_markers: true,
            error: String::new(),
        }
    }
}

impl Default for RippleState {
    fn default() -> Self {
        Self {
            on: false,
            all_lanes: false,
            linked: true,
            markers: true,
            gap_open: false,
            gap_seconds: 1.0,
        }
    }
}

impl Studio {
    /// The three policies the settings stand for.
    pub fn ripple_policies(&self) -> (RipplePolicy, LinkPolicy, MarkerPolicy) {
        (
            if self.ripple.all_lanes {
                RipplePolicy::AllTracks
            } else {
                RipplePolicy::SelectedTracks
            },
            if self.ripple.linked {
                LinkPolicy::Preserve
            } else {
                LinkPolicy::Detach
            },
            if self.ripple.markers {
                MarkerPolicy::ShiftFree
            } else {
                MarkerPolicy::AttachedOnly
            },
        )
    }

    /// A ripple row of the Edit menu was chosen. Returns whether `id` was one.
    pub fn ripple_option(&mut self, id: &str) -> bool {
        match id {
            "ripple" => self.ripple.on = !self.ripple.on,
            "ripple-all" => self.ripple.all_lanes = !self.ripple.all_lanes,
            "ripple-linked" => self.ripple.linked = !self.ripple.linked,
            "ripple-markers" => self.ripple.markers = !self.ripple.markers,
            "insert-gap" => self.ripple.gap_open = true,
            _ => return false,
        }
        true
    }

    /// Deletes `clip_ids` and closes the time they held.
    pub fn ripple_delete(&mut self, clip_ids: Vec<String>) {
        let (ripple_policy, link_policy, marker_policy) = self.ripple_policies();
        self.apply(Command::RippleDelete {
            clip_ids,
            ripple_policy,
            link_policy,
            marker_policy,
        });
    }

    /// The lanes an insert-gap makes room on: those of the selected clips, or
    /// every unlocked lane when nothing is selected.
    fn gap_lanes(&self) -> Vec<String> {
        let selected: Vec<String> = self
            .selection
            .iter()
            .filter_map(|id| self.clip(id).map(|clip| clip.track_id.clone()))
            .collect();
        if selected.is_empty() {
            self.timeline()
                .tracks
                .iter()
                .filter(|track| !self.locked(&track.id))
                .map(|track| track.id.clone())
                .collect()
        } else {
            selected
        }
    }

    /// Opens `seconds` of empty time at the playhead.
    pub fn apply_gap(&mut self) {
        let (ripple_policy, link_policy, marker_policy) = self.ripple_policies();
        let applied = self.apply_all(vec![Command::InsertGap {
            at: f64::from(self.playhead),
            duration: f64::from(self.ripple.gap_seconds),
            track_ids: self.gap_lanes(),
            ripple_policy,
            link_policy,
            marker_policy,
        }]);
        if applied {
            self.ripple.gap_open = false;
            self.notify(
                &format!("Opened {:.1} s at the playhead", self.ripple.gap_seconds),
                false,
            );
        }
    }

    /// The insert-gap sheet as the window draws it.
    pub fn gap_data(&self) -> GapData {
        let scope = if self.ripple.all_lanes {
            "Every lane makes room.".to_owned()
        } else if self.selection.is_empty() {
            "Every unlocked lane makes room. Select clips to limit it to their lanes.".to_owned()
        } else {
            "The selected clips' lanes make room.".to_owned()
        };
        GapData {
            open: self.ripple.gap_open,
            seconds: self.ripple.gap_seconds,
            at: format!("At {:.2} s", self.playhead).into(),
            scope: scope.into(),
        }
    }
}

impl Studio {
    /// Adds black at the head and tail of the film, cross-faded in.
    pub fn apply_bookends(&mut self) {
        self.echo = None;
        self.last_commit = None;
        let state = &self.bookends;
        let (duration, fade, all, markers) = (
            f64::from(state.duration),
            f64::from(state.fade),
            state.shift_all,
            state.shift_markers,
        );
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match puppetcat_host::bookends::add_fade_to_black(
            session, None, duration, fade, all, markers,
        ) {
            Ok(_) => {
                self.after_change();
                self.bookends.open = false;
                self.bookends.error.clear();
                self.notify("Added fades at both ends", false);
            }
            Err(error) => self.bookends.error = error,
        }
    }

    pub fn bookends_data(&self) -> BookendsData {
        BookendsData {
            open: self.bookends.open,
            duration: self.bookends.duration,
            fade: self.bookends.fade,
            shift_all: self.bookends.shift_all,
            shift_markers: self.bookends.shift_markers,
            error: self.bookends.error.as_str().into(),
        }
    }
}
