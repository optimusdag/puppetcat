// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Batch export: many stretches of one timeline, each to its own file, from
//! a single preflighted plan. Everything that can be wrong with the plan -
//! reversed or overlapping ranges, clashing or existing filenames - is
//! rejected before the first frame renders.

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use super::{AgentEngine, ExportRangeArgs, no_project};
use crate::export::{BatchItem, BatchStatus};
pub use puppetcat_host::batch_plan::PlannedExport;
use puppetcat_host::batch_plan::{self, PlanOptions};

/// One requested stretch.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchRangeArg {
    /// Label for `{name}`; defaults to the start marker's name.
    pub name: Option<String>,
    /// Start in seconds.
    pub start: Option<f64>,
    /// End in seconds.
    pub end: Option<f64>,
    /// Start marker (id or name).
    pub from_marker: Option<String>,
    /// End marker (id or name).
    pub to_marker: Option<String>,
}

/// The whole batch request.
#[derive(Debug, Clone)]
pub struct BatchRequest {
    /// Explicit stretches. Empty means "derive them from marker names".
    pub ranges: Vec<BatchRangeArg>,
    /// Pair markers named `<label> start`/`<label> end` (or `in`/`out`).
    pub pair_by_name: bool,
    /// Filename without extension: `{name}`, `{index}` (two digits),
    /// `{project}`. Default `{name}`.
    pub template: String,
    /// Where files go; created if missing.
    pub output_dir: String,
    /// x264 CRF.
    pub crf: u8,
    /// x264 preset.
    pub preset: String,
    /// `fail` (default), `skip`, `overwrite` or `suffix`.
    pub collision: String,
    /// Seconds added before each start, clamped to the timeline.
    pub head: f64,
    /// Seconds added after each end, clamped to the timeline.
    pub tail: f64,
    /// Plan only.
    pub dry_run: bool,
}

/// What `start_batch_export` returns.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchPlan {
    /// Whether rendering started.
    pub started: bool,
    /// The preflighted items.
    pub items: Vec<PlannedExport>,
}

impl AgentEngine {
    /// Preflights and starts a batch export; poll `batch_export_status`.
    pub fn start_batch_export(&self, req: BatchRequest) -> Result<BatchPlan, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        let timeline = session.project().active();
        let duration = timeline.duration();
        let project_name = session.settings().name.clone();

        let mut args = req.ranges.clone();
        if args.is_empty() {
            if !req.pair_by_name {
                return Err("give ranges, or set pairByName to derive them from markers".to_owned());
            }
            args = batch_plan::pair_markers(timeline)
                .into_iter()
                .map(|pair| BatchRangeArg {
                    name: Some(pair.label),
                    from_marker: Some(pair.from_marker),
                    to_marker: Some(pair.to_marker),
                    ..Default::default()
                })
                .collect();
            if args.is_empty() {
                return Err(
                    "no marker pairs found: name them \"<label> start\" and \"<label> end\""
                        .to_owned(),
                );
            }
        }

        // Resolve and validate every range before anything touches disk.
        let mut resolved: Vec<(String, (f64, f64))> = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            let range = ExportRangeArgs {
                start: arg.start,
                end: arg.end,
                from_marker: arg.from_marker.clone(),
                to_marker: arg.to_marker.clone(),
            }
            .resolve(timeline)
            .map_err(|e| format!("range {}: {e}", index + 1))?
            .ok_or_else(|| format!("range {}: name a start or an end", index + 1))?;
            let (start, end) = (range.0, range.1.min(duration));
            if end <= start {
                return Err(format!(
                    "range {}: empty ({start:.2}s to {end:.2}s)",
                    index + 1
                ));
            }
            let name = arg
                .name
                .clone()
                .or_else(|| arg.from_marker.clone())
                .unwrap_or_else(|| format!("range-{}", index + 1));
            resolved.push((name, (start, end)));
        }
        let planned = batch_plan::plan_files(
            &resolved,
            duration,
            &PlanOptions {
                template: &req.template,
                output_dir: &req.output_dir,
                collision: &req.collision,
                head: req.head,
                tail: req.tail,
                project_name: &project_name,
            },
        )?;
        if req.dry_run {
            return Ok(BatchPlan {
                started: false,
                items: planned,
            });
        }

        let dir = std::path::PathBuf::from(&req.output_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let titles: Vec<_> = self
            .titles
            .clips(
                session.project(),
                session.settings().width,
                session.settings().height,
            )
            .into_iter()
            .map(|title| title.clip)
            .collect();
        let scratch = puppetcat_host::export::temp_folder_preference(&self.dirs);
        let items = planned
            .iter()
            .map(|item| BatchItem {
                name: item.name.clone(),
                output: item.output.clone(),
                range: item.range,
                skip: item.skip,
                request: puppetcat_host::export::request(
                    session,
                    &puppetcat_host::export::ExportSpec {
                        output: item.output.clone(),
                        scratch_dir: scratch.clone(),
                        crf: req.crf,
                        preset: req.preset.clone(),
                        range: Some(item.range),
                    },
                    titles.clone(),
                ),
            })
            .collect();
        self.export.start_batch(items)?;
        Ok(BatchPlan {
            started: true,
            items: planned,
        })
    }

    /// Per-item progress of the latest batch.
    pub fn batch_export_status(&self) -> BatchStatus {
        self.export.batch_status()
    }

    /// Stops the running export or batch at the next frame.
    pub fn cancel_export(&self) {
        self.export.cancel();
    }
}
