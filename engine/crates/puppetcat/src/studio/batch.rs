// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Batch export: every stretch between a pair of markers named `<label> start`
//! and `<label> end` rendered to its own file.
//!
//! Planning (pairing the markers, naming the files, refusing overlaps and
//! clashes) and running (one file after another, a failure not stopping the
//! rest) are `puppetcat_host::batch_plan` and `batch_export`, the same code the
//! agent's `start_batch_export` uses. This is the window's half: the sheet, the
//! folder picker, and polling the run.

use puppetcat_host::batch_export::{BatchItem, BatchStatus, ExportJob};
use puppetcat_host::batch_plan::{self, PlanOptions, PlannedExport};
use puppetcat_host::export::{self, ExportSpec};

use super::Studio;
use super::cleanup::clock;
use crate::ui::{BatchData, BatchRowData};

/// x264 CRF for the three quality choices, as the single export's sheet has.
const CRF: [u8; 3] = [16, 20, 26];
const COLLISIONS: [&str; 4] = ["fail", "skip", "overwrite", "suffix"];
const POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// The batch sheet's state.
pub struct BatchState {
    pub open: bool,
    pub folder: String,
    pub template: String,
    pub quality: usize,
    pub collision: usize,
    pub head: f32,
    pub tail: f32,
    /// What the settings plan to write, or why they cannot.
    pub plan: Result<Vec<PlannedExport>, String>,
    /// How many marker pairs the timeline has.
    pub pairs: usize,
    /// The run in flight, or the last one.
    pub job: Option<ExportJob>,
    pub status: BatchStatus,
    pub poll: slint::Timer,
}

impl Default for BatchState {
    fn default() -> Self {
        Self {
            open: false,
            folder: String::new(),
            template: "{name}".to_owned(),
            quality: 1,
            collision: 0,
            head: 0.0,
            tail: 0.0,
            plan: Err(String::new()),
            pairs: 0,
            job: None,
            status: BatchStatus::default(),
            poll: slint::Timer::default(),
        }
    }
}

impl Studio {
    pub fn open_batch(&mut self) {
        if self.batch.job.is_none() || !self.batch.status.running {
            self.batch.status = BatchStatus::default();
        }
        self.batch.open = true;
        if self.batch.folder.is_empty() {
            self.batch.folder = self.export.folder.clone();
        }
        self.replan_batch();
    }

    pub fn batch_browse(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Export the files to")
            .pick_folder()
        {
            self.batch.folder = folder.to_string_lossy().into_owned();
            self.replan_batch();
        }
    }

    /// Plans what the current settings would write.
    pub fn replan_batch(&mut self) {
        // Read the timeline first, so what is learnt can be stored after.
        let (duration, pair_count, resolved) = {
            let timeline = self.timeline();
            let duration = timeline.duration();
            let pairs = batch_plan::pair_markers(timeline);
            let resolved: Vec<(String, (f64, f64))> = pairs
                .iter()
                .filter_map(|pair| {
                    let start = batch_plan::marker_time(timeline, &pair.from_marker)?;
                    let end = batch_plan::marker_time(timeline, &pair.to_marker)?.min(duration);
                    (end > start).then(|| (pair.label.clone(), (start, end)))
                })
                .collect();
            (duration, pairs.len(), resolved)
        };
        self.batch.pairs = pair_count;
        if pair_count == 0 {
            self.batch.plan = Err(
                "No marker pairs: name two markers \u{201c}intro start\u{201d} and \u{201c}intro end\u{201d} \
                 (or in / out) to mark a stretch."
                    .to_owned(),
            );
            return;
        }
        if self.batch.folder.trim().is_empty() {
            self.batch.plan = Err("Choose a folder for the files.".to_owned());
            return;
        }
        let name = self
            .session
            .as_ref()
            .map(|session| session.settings().name.clone())
            .unwrap_or_default();
        self.batch.plan = batch_plan::plan_files(
            &resolved,
            duration,
            &PlanOptions {
                template: &self.batch.template,
                output_dir: &self.batch.folder,
                collision: COLLISIONS[self.batch.collision.min(3)],
                head: f64::from(self.batch.head),
                tail: f64::from(self.batch.tail),
                project_name: &name,
            },
        );
    }

    pub fn start_batch(&mut self) {
        let Ok(plan) = self.batch.plan.clone() else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if plan.is_empty() {
            return;
        }
        if let Err(error) = std::fs::create_dir_all(&self.batch.folder) {
            self.batch.plan = Err(format!("Could not create the folder: {error}"));
            return;
        }
        let (width, height) = self.output_size();
        let titles: Vec<_> = self
            .host
            .titles
            .clips(session.project(), width, height)
            .into_iter()
            .map(|title| title.clip)
            .collect();
        let scratch = self
            .prefs
            .temp_export_folder
            .clone()
            .filter(|folder| !folder.is_empty());
        let items: Vec<BatchItem> = plan
            .iter()
            .map(|item| BatchItem {
                name: item.name.clone(),
                output: item.output.clone(),
                range: item.range,
                skip: item.skip,
                request: export::request(
                    session,
                    &ExportSpec {
                        output: item.output.clone(),
                        scratch_dir: scratch.clone(),
                        crf: CRF[self.batch.quality.min(2)],
                        preset: "medium".into(),
                        range: Some(item.range),
                    },
                    titles.clone(),
                ),
            })
            .collect();
        // The window's one export slot, shared with the single export, so the
        // two can never render at once.
        let job = ExportJob::with_exporter(self.host.exporter.clone());
        if let Err(error) = job.start_batch(items) {
            self.batch.plan = Err(error);
            return;
        }
        self.pause();
        self.batch.status = job.batch_status();
        self.batch.job = Some(job);
        self.batch.poll.start(slint::TimerMode::Repeated, POLL, || {
            crate::host::Shell::with(|shell, app| {
                shell.studio.borrow_mut().poll_batch();
                shell.studio.borrow().publish(&app, &shell.models);
            });
        });
    }

    fn poll_batch(&mut self) {
        let Some(job) = self.batch.job.as_ref() else {
            self.batch.poll.stop();
            return;
        };
        let status = job.batch_status();
        let finished = !status.running;
        self.batch.status = status;
        if finished {
            self.batch.poll.stop();
            let done = self
                .batch
                .status
                .items
                .iter()
                .filter(|item| item.state == "done")
                .count();
            let failed = self
                .batch
                .status
                .items
                .iter()
                .filter(|item| item.state == "failed")
                .count();
            if failed > 0 {
                self.notify(&format!("Exported {done}, {failed} failed"), true);
            } else {
                self.notify(
                    &format!("Exported {done} file{}", if done == 1 { "" } else { "s" }),
                    false,
                );
            }
        }
    }

    pub fn cancel_batch(&mut self) {
        if let Some(job) = &self.batch.job {
            job.cancel();
        }
    }

    pub fn batch_rows(&self) -> Vec<BatchRowData> {
        let file = |output: &str| {
            std::path::Path::new(output)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        if self.batch.status.running || !self.batch.status.items.is_empty() {
            return self
                .batch
                .status
                .items
                .iter()
                .map(|item| BatchRowData {
                    name: item.name.as_str().into(),
                    range: format!("{} – {}", clock(item.range.0), clock(item.range.1)).into(),
                    file: file(&item.output).into(),
                    state: item.state.into(),
                })
                .collect();
        }
        self.batch
            .plan
            .as_ref()
            .map(|plan| {
                plan.iter()
                    .map(|item| BatchRowData {
                        name: item.name.as_str().into(),
                        range: format!("{} – {}", clock(item.range.0), clock(item.range.1)).into(),
                        file: file(&item.output).into(),
                        state: if item.skip { "skipped" } else { "pending" }.into(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn batch_data(&self) -> BatchData {
        let running = self.batch.status.running;
        let counted: Vec<_> = self
            .batch
            .status
            .items
            .iter()
            .filter(|item| item.state != "skipped")
            .collect();
        let progress = if counted.is_empty() {
            0.0
        } else {
            counted
                .iter()
                .map(|item| match item.state {
                    "done" | "failed" | "cancelled" => 1.0,
                    "running" if item.total > 0 => item.frame as f32 / item.total as f32,
                    _ => 0.0,
                })
                .sum::<f32>()
                / counted.len() as f32
        };
        let current = counted
            .iter()
            .position(|item| item.state == "running")
            .map_or(0, |index| index + 1);
        let (summary, error, can_start) = match &self.batch.plan {
            Ok(plan) => (
                format!(
                    "{} marker pair{}",
                    self.batch.pairs,
                    if self.batch.pairs == 1 { "" } else { "s" }
                ),
                String::new(),
                !plan.is_empty() && !running,
            ),
            Err(error) => (String::new(), error.clone(), false),
        };
        BatchData {
            open: self.batch.open,
            folder: self.batch.folder.as_str().into(),
            template: self.batch.template.as_str().into(),
            quality: self.batch.quality as i32,
            collision: self.batch.collision as i32,
            head: self.batch.head,
            tail: self.batch.tail,
            summary: summary.into(),
            error: error.into(),
            running,
            progress,
            stage: format!("Rendering {current} of {}", counted.len()).into(),
            can_start,
        }
    }
}
