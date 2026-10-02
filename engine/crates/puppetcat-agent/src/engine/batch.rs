// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Batch export: many stretches of one timeline, each to its own file, from
//! a single preflighted plan. Everything that can be wrong with the plan -
//! reversed or overlapping ranges, clashing or existing filenames - is
//! rejected before the first frame renders.

use std::path::PathBuf;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use super::{AgentEngine, ExportRangeArgs, no_project};
use crate::export::{BatchItem, BatchStatus};

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

/// One planned file.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedExport {
    /// Position from 1.
    pub index: usize,
    /// Label.
    pub name: String,
    /// Output file.
    pub output: String,
    /// Exported stretch including handles.
    pub range: (f64, f64),
    /// Will be skipped because the file exists and policy is `skip`.
    pub skip: bool,
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
        if !matches!(
            req.collision.as_str(),
            "fail" | "skip" | "overwrite" | "suffix"
        ) {
            return Err("collision must be fail, skip, overwrite or suffix".to_owned());
        }
        if req.head < 0.0 || req.tail < 0.0 {
            return Err("head and tail cannot be negative".to_owned());
        }
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
            args = pair_markers(timeline);
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
        let mut order: Vec<usize> = (0..resolved.len()).collect();
        order.sort_by(|a, b| resolved[*a].1.0.total_cmp(&resolved[*b].1.0));
        for pair in order.windows(2) {
            let (first, second) = (&resolved[pair[0]], &resolved[pair[1]]);
            if second.1.0 < first.1.1 {
                return Err(format!(
                    "ranges \"{}\" and \"{}\" overlap",
                    first.0, second.0
                ));
            }
        }

        let dir = PathBuf::from(&req.output_dir);
        if !dir.is_absolute() {
            return Err(format!("{} is not an absolute path", req.output_dir));
        }
        let mut used: Vec<PathBuf> = Vec::new();
        let mut planned = Vec::new();
        let mut clashes = Vec::new();
        for (index, (name, range)) in resolved.iter().enumerate() {
            let stem = expand(&req.template, name, index + 1, &project_name)?;
            let stem = puppetcat_host::projects::folder_name(&stem);
            let mut path = dir.join(format!("{stem}.mp4"));
            if used.contains(&path) {
                return Err(format!("two ranges would write {}", path.display()));
            }
            let mut skip = false;
            if path.exists() {
                match req.collision.as_str() {
                    "fail" => clashes.push(path.display().to_string()),
                    "skip" => skip = true,
                    "suffix" => {
                        let mut n = 2;
                        while path.exists() || used.contains(&path) {
                            path = dir.join(format!("{stem}-{n}.mp4"));
                            n += 1;
                        }
                    }
                    _ => {}
                }
            }
            used.push(path.clone());
            planned.push(PlannedExport {
                index: index + 1,
                name: name.clone(),
                output: path.to_string_lossy().into_owned(),
                range: (
                    (range.0 - req.head).max(0.0),
                    (range.1 + req.tail).min(duration),
                ),
                skip,
            });
        }
        if !clashes.is_empty() {
            return Err(format!(
                "these files already exist (use collision skip, suffix or overwrite): {}",
                clashes.join(", ")
            ));
        }
        if req.dry_run {
            return Ok(BatchPlan {
                started: false,
                items: planned,
            });
        }

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

fn expand(template: &str, name: &str, index: usize, project: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..]
            .find('}')
            .ok_or_else(|| format!("unclosed {{ in template \"{template}\""))?
            + open;
        match &rest[open + 1..close] {
            "name" => out.push_str(name),
            "index" => out.push_str(&format!("{index:02}")),
            "project" => out.push_str(project),
            other => return Err(format!("unknown template field {{{other}}}")),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    if out.trim().is_empty() {
        return Err("the filename template produced an empty name".to_owned());
    }
    Ok(out)
}

/// Pairs `<label> start|in` with the next `<label> end|out`, by name.
fn pair_markers(timeline: &puppetcat_project::model::Timeline) -> Vec<BatchRangeArg> {
    let mut markers: Vec<_> = timeline.markers.iter().collect();
    markers.sort_by(|a, b| a.time.total_cmp(&b.time));
    let split = |name: &str| -> Option<(String, bool)> {
        let lower = name.trim().to_lowercase();
        for (suffix, is_start) in [
            (" start", true),
            (" in", true),
            (" end", false),
            (" out", false),
        ] {
            if let Some(label) = lower.strip_suffix(suffix) {
                return Some((label.trim().to_owned(), is_start));
            }
        }
        None
    };
    let mut pairs = Vec::new();
    for (i, start) in markers.iter().enumerate() {
        let Some((label, true)) = split(&start.name) else {
            continue;
        };
        if let Some(end) = markers[i + 1..]
            .iter()
            .find(|m| split(&m.name) == Some((label.clone(), false)))
        {
            pairs.push(BatchRangeArg {
                name: Some(label),
                from_marker: Some(start.id.clone()),
                to_marker: Some(end.id.clone()),
                ..Default::default()
            });
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_expand_known_fields_and_reject_unknown_ones() {
        assert_eq!(
            expand("{index}-{name}", "intro", 3, "P").unwrap(),
            "03-intro"
        );
        assert!(expand("{nope}", "a", 1, "P").is_err());
        assert!(expand("{name", "a", 1, "P").is_err());
    }
}
