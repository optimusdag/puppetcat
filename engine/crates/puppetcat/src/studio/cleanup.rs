// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Cutting time out of the edit: delete a range, remove silence, mark the cuts
//! in a clip.
//!
//! The scanning and the commands it proposes live in `puppetcat_host::analysis`,
//! the same functions the agent's tools call, so a button here and a tool there
//! make the same edit. This file is only the window's half: where the pass
//! looks, running it off the event loop, and the sheet that previews it.

use puppetcat_host::analysis::{self, SilenceScan};
use puppetcat_project::Command;

use super::Studio;
use crate::host::spawn;
use crate::ui::{SilenceData, SilencePhase};

/// The defaults the agent's `remove_silence` tool starts from.
const THRESHOLD_DB: f32 = -35.0;
const MIN_SILENCE: f32 = 0.5;
const PADDING: f32 = 0.1;

/// How long a slider rests before the analysis re-runs, so dragging one does
/// not start an analysis for every pixel it passes.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(350);

/// Where a remove-silence pass looks.
#[derive(Clone, PartialEq, Debug)]
pub enum SilenceScope {
    /// Everything audible on the timeline.
    Timeline,
    /// The ruler selection, in seconds.
    Range(f64, f64),
    /// The stretch one clip covers.
    Clip(String),
}

/// The remove-silence sheet's state.
pub struct SilenceState {
    pub open: bool,
    pub scope: SilenceScope,
    pub threshold: f32,
    pub min_silence: f32,
    pub padding: f32,
    pub phase: SilencePhase,
    pub scan: Option<SilenceScan>,
    pub message: String,
    /// Which analysis the sheet is waiting for; a slower, older one that lands
    /// late is ignored.
    pub generation: u64,
    /// The edit's revision the scan was made against.
    pub revision: u64,
    pub settle: slint::Timer,
}

impl Default for SilenceState {
    fn default() -> Self {
        Self {
            open: false,
            scope: SilenceScope::Timeline,
            threshold: THRESHOLD_DB,
            min_silence: MIN_SILENCE,
            padding: PADDING,
            phase: SilencePhase::Ready,
            scan: None,
            message: String::new(),
            generation: 0,
            revision: 0,
            settle: slint::Timer::default(),
        }
    }
}

/// `m:ss.s` for a span edge.
fn clock(seconds: f64) -> String {
    let whole = seconds.max(0.0);
    format!("{}:{:04.1}", (whole / 60.0) as u32, whole % 60.0)
}

impl Studio {
    /// Makes several commands one edit, one Undo step.
    pub fn apply_all(&mut self, commands: Vec<Command>) -> bool {
        self.echo = None;
        self.last_commit = None;
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        match session.stage(commands) {
            Ok(staged) => {
                session.commit(staged);
                self.after_change();
                true
            }
            Err(error) => {
                self.notify(&error, true);
                false
            }
        }
    }

    /// The lanes a cut applies to: every lane, unless some are locked, in which
    /// case just the others - a locked lane is one the editor has said not to
    /// touch. Empty means all of them.
    fn cut_lanes(&self) -> Vec<String> {
        let tracks = &self.timeline().tracks;
        if tracks.iter().any(|track| self.locked(&track.id)) {
            tracks
                .iter()
                .filter(|track| !self.locked(&track.id))
                .map(|track| track.id.clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Cuts the ruler selection out of every unlocked lane and closes the gap.
    pub fn delete_range(&mut self) {
        let Some((start, end)) = self.range else {
            self.notify("Shift-drag along the ruler to select a range", true);
            return;
        };
        let lanes = self.cut_lanes();
        if !lanes.is_empty() || self.timeline().tracks.iter().all(|t| !self.locked(&t.id)) {
            let cut = Command::RippleDeleteRange {
                start,
                end,
                track_ids: lanes,
                marker_policy: Default::default(),
            };
            if self.apply_all(vec![cut]) {
                self.range = None;
                self.notify(
                    &format!("Cut {:.1} s from the timeline", end - start),
                    false,
                );
            }
        } else {
            self.notify("Every lane is locked", true);
        }
    }

    /// Opens the remove-silence sheet on `scope` and starts listening.
    pub fn open_silence(&mut self, scope: SilenceScope) {
        self.silence.open = true;
        self.silence.scope = scope;
        self.silence.scan = None;
        self.silence.message.clear();
        self.analyse_silence();
    }

    pub fn close_silence(&mut self) {
        self.silence.open = false;
        // A scan still running reports to a generation nobody waits for.
        self.silence.generation += 1;
        self.silence.settle.stop();
    }

    /// A slider moved: re-run the analysis once it has rested.
    pub fn silence_changed(&mut self) {
        self.silence.phase = SilencePhase::Analysing;
        self.silence
            .settle
            .start(slint::TimerMode::SingleShot, SETTLE, || {
                crate::host::Shell::with(|shell, app| {
                    shell.studio.borrow_mut().analyse_silence();
                    shell.studio.borrow().publish(&app, &shell.models);
                });
            });
    }

    fn analyse_silence(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let project = session.project().clone();
        self.silence.revision = session.revision();
        self.silence.generation += 1;
        self.silence.phase = SilencePhase::Analysing;
        self.silence.message.clear();
        let generation = self.silence.generation;
        let (clip, start, end) = match &self.silence.scope {
            SilenceScope::Timeline => (None, None, None),
            SilenceScope::Range(from, to) => (None, Some(*from), Some(*to)),
            SilenceScope::Clip(id) => (Some(id.clone()), None, None),
        };
        let (threshold, min_silence, padding) = (
            f64::from(self.silence.threshold),
            f64::from(self.silence.min_silence),
            f64::from(self.silence.padding),
        );
        spawn(
            move || {
                analysis::scan_silence(&project, clip, start, end, threshold, min_silence, padding)
            },
            move |studio, _, _, result| {
                if studio.silence.generation != generation || !studio.silence.open {
                    return;
                }
                match result {
                    Ok(scan) => {
                        studio.silence.message = scan.warnings.join(" ");
                        studio.silence.scan = Some(scan);
                        studio.silence.phase = SilencePhase::Ready;
                    }
                    Err(error) => {
                        studio.silence.scan = None;
                        studio.silence.message = error;
                        studio.silence.phase = SilencePhase::Failed;
                    }
                }
            },
        );
    }

    /// Makes the cuts the sheet is showing.
    pub fn apply_silence(&mut self) {
        let Some(scan) = self.silence.scan.clone() else {
            return;
        };
        let current = self.session.as_ref().map(|session| session.revision());
        if current != Some(self.silence.revision) {
            // The edit moved on under the scan; what it found no longer holds.
            self.analyse_silence();
            return;
        }
        if scan.cuts.is_empty() {
            return;
        }
        let commands = analysis::silence_commands(&scan.cuts, &self.cut_lanes());
        let (count, seconds) = (scan.cuts.len(), scan.removed_seconds);
        if self.apply_all(commands) {
            self.close_silence();
            self.range = None;
            self.notify(
                &format!(
                    "Removed {count} silence{} · {seconds:.1} s",
                    if count == 1 { "" } else { "s" }
                ),
                false,
            );
        }
    }

    /// Finds the cuts in a clip and drops a marker at each.
    pub fn detect_scenes(&mut self, clip_id: String) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let project = session.project().clone();
        let revision = session.revision();
        self.notify("Looking for cuts…", false);
        spawn(
            move || analysis::scan_scenes(&project, None, Some(clip_id), None, None, 0.15, 0.5),
            move |studio, _, _, result| match result {
                Err(error) => studio.notify(&error, true),
                Ok(scan) if scan.cuts.is_empty() => {
                    studio.notify("No cuts found in the clip", false)
                }
                Ok(scan) => {
                    if studio.session.as_ref().map(|s| s.revision()) != Some(revision) {
                        studio.notify("The timeline changed while looking; try again", true);
                        return;
                    }
                    let count = scan.cuts.len();
                    if studio.apply_all(analysis::marker_commands(&scan.cuts)) {
                        studio.notify(
                            &format!("Marked {count} cut{}", if count == 1 { "" } else { "s" }),
                            false,
                        );
                    }
                }
            },
        );
    }

    /// The sheet as the window draws it.
    pub fn silence_data(&self) -> SilenceData {
        let state = &self.silence;
        let scope = match &state.scope {
            SilenceScope::Timeline => "Whole timeline".to_owned(),
            SilenceScope::Range(from, to) => {
                format!("Selected range {} – {}", clock(*from), clock(*to))
            }
            SilenceScope::Clip(id) => self
                .clip(id)
                .map_or_else(|| "Clip".to_owned(), |clip| clip.name.clone()),
        };
        let (count, summary) = match &state.scan {
            Some(scan) if !scan.cuts.is_empty() => (
                scan.cuts.len(),
                format!(
                    "{} silence{} · {:.1} s will be removed",
                    scan.cuts.len(),
                    if scan.cuts.len() == 1 { "" } else { "s" },
                    scan.removed_seconds
                ),
            ),
            Some(_) => (0, "No silence found with these settings".to_owned()),
            None => (0, String::new()),
        };
        SilenceData {
            open: state.open,
            scope: scope.into(),
            threshold: state.threshold,
            min_silence: state.min_silence,
            padding: state.padding,
            phase: state.phase,
            summary: summary.into(),
            message: state.message.as_str().into(),
            count: count as i32,
            threshold_text: format!("{:.0} dB", state.threshold).into(),
            min_silence_text: format!("{:.2} s", state.min_silence).into(),
            padding_text: format!("{:.2} s", state.padding).into(),
        }
    }
}
