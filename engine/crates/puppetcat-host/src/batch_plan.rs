// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Planning a batch export: many stretches of one timeline, each to its own
//! file. Everything that can be wrong with the plan - overlapping ranges,
//! clashing or existing filenames, a bad template - is rejected here, before
//! the first frame renders, and the window and the agent plan identically.

use std::path::PathBuf;

use puppetcat_project::model::Timeline;
use serde::Serialize;

/// One planned file.
#[derive(Debug, Clone, Serialize)]
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

/// How the files are named and where they go.
#[derive(Debug, Clone)]
pub struct PlanOptions<'a> {
    /// Filename without extension: `{name}`, `{index}` (two digits),
    /// `{project}`.
    pub template: &'a str,
    /// Where files go; must be absolute.
    pub output_dir: &'a str,
    /// `fail`, `skip`, `overwrite` or `suffix`.
    pub collision: &'a str,
    /// Seconds added before each start, clamped to the timeline.
    pub head: f64,
    /// Seconds added after each end, clamped to the timeline.
    pub tail: f64,
    /// The project's name, for `{project}`.
    pub project_name: &'a str,
}

/// The collision policies a plan understands.
pub const COLLISIONS: [&str; 4] = ["fail", "skip", "overwrite", "suffix"];

/// Checks the stretches and names the files. `resolved` is `(label, (start,
/// end))` in seconds, already clamped to the timeline; `duration` bounds the
/// handles.
pub fn plan_files(
    resolved: &[(String, (f64, f64))],
    duration: f64,
    options: &PlanOptions<'_>,
) -> Result<Vec<PlannedExport>, String> {
    if !COLLISIONS.contains(&options.collision) {
        return Err("collision must be fail, skip, overwrite or suffix".to_owned());
    }
    if options.head < 0.0 || options.tail < 0.0 {
        return Err("head and tail cannot be negative".to_owned());
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

    let dir = PathBuf::from(options.output_dir);
    if !dir.is_absolute() {
        return Err(format!("{} is not an absolute path", options.output_dir));
    }
    let mut used: Vec<PathBuf> = Vec::new();
    let mut planned = Vec::new();
    let mut clashes = Vec::new();
    for (index, (name, range)) in resolved.iter().enumerate() {
        let stem = expand(options.template, name, index + 1, options.project_name)?;
        let stem = crate::projects::folder_name(&stem);
        let mut path = dir.join(format!("{stem}.mp4"));
        if used.contains(&path) {
            return Err(format!("two ranges would write {}", path.display()));
        }
        let mut skip = false;
        if path.exists() {
            match options.collision {
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
                (range.0 - options.head).max(0.0),
                (range.1 + options.tail).min(duration),
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
    Ok(planned)
}

/// Expands `{name}`, `{index}` and `{project}` in a filename template.
pub fn expand(template: &str, name: &str, index: usize, project: &str) -> Result<String, String> {
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

/// A start marker and the end marker it pairs with.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerPair {
    /// The shared label, lower-cased.
    pub label: String,
    /// The start marker's id.
    pub from_marker: String,
    /// The end marker's id.
    pub to_marker: String,
}

/// Pairs `<label> start|in` with the next `<label> end|out`, by name.
pub fn pair_markers(timeline: &Timeline) -> Vec<MarkerPair> {
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
            pairs.push(MarkerPair {
                label,
                from_marker: start.id.clone(),
                to_marker: end.id.clone(),
            });
        }
    }
    pairs
}

/// The time of a marker on `timeline`, by id.
pub fn marker_time(timeline: &Timeline, id: &str) -> Option<f64> {
    timeline
        .markers
        .iter()
        .find(|marker| marker.id == id)
        .map(|marker| marker.time)
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

    #[test]
    fn a_plan_rejects_overlaps_and_adds_clamped_handles() {
        let options = PlanOptions {
            template: "{name}",
            output_dir: "/tmp/puppetcat-batch-plan-nonexistent",
            collision: "fail",
            head: 1.0,
            tail: 1.0,
            project_name: "P",
        };
        let overlapping = [("a".to_owned(), (0.0, 5.0)), ("b".to_owned(), (4.0, 8.0))];
        assert!(plan_files(&overlapping, 10.0, &options).is_err());

        let fine = [("a".to_owned(), (0.5, 3.0)), ("b".to_owned(), (4.0, 9.5))];
        let plan = plan_files(&fine, 10.0, &options).expect("plans");
        assert_eq!(plan[0].range, (0.0, 4.0), "the head clamps at zero");
        assert_eq!(plan[1].range, (3.0, 10.0), "the tail clamps at the end");
        assert!(plan[0].output.ends_with("/a.mp4"));
    }
}
