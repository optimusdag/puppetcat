// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Keeping a project's files in step with the disk: relinking media that
//! moved, and moving the project's own folder.
//!
//! The checks and the commands they propose live here so the window's dialogs
//! and the agent's tools behave identically: every replacement is probed and
//! compared with what it replaces, and nothing changes unless all of them
//! pass.

use std::path::{Path, PathBuf};

use puppetcat_project::model::MediaItem;
use puppetcat_project::{Command, Project};
use serde::Serialize;

use crate::media::{MediaSummary, probe};

/// One media item to repoint at a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Relink {
    /// The bin item.
    pub media_id: String,
    /// The file that replaces it.
    pub path: String,
}

/// The verdict on one item of a relink.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelinkItem {
    /// The bin item.
    pub media_id: String,
    /// Where it pointed.
    pub old_path: String,
    /// Where it would point.
    pub new_path: String,
    /// `ok`, `missing`, `unreadable` or `incompatible`.
    pub status: &'static str,
    /// Why, when not `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The bin items whose files are not on disk, as `(id, path)`. A placeholder
/// slot has no file by design and is not missing.
pub fn missing_media(project: &Project) -> Vec<(String, String)> {
    project
        .media
        .iter()
        .filter(|item| !item.placeholder && !Path::new(&item.path).is_file())
        .map(|item| (item.id.clone(), item.path.clone()))
        .collect()
}

/// Turns `old_root` -> `new_root` into one replacement per item under
/// `old_root`.
pub fn swap_root(project: &Project, old_root: &str, new_root: &str) -> Vec<Relink> {
    let old_root = old_root.trim_end_matches('/');
    let new_root = new_root.trim_end_matches('/');
    project
        .media
        .iter()
        .filter_map(|item| {
            item.path
                .strip_prefix(&format!("{old_root}/"))
                .map(|rest| Relink {
                    media_id: item.id.clone(),
                    path: format!("{new_root}/{rest}"),
                })
        })
        .collect()
}

/// Checks every replacement: the file is there, it can be read, and it is
/// what the item was. Any failure is reported against its item; the caller
/// decides what a failure means (the agent and the window both refuse the
/// whole relink).
pub fn check_relinks(project: &Project, wanted: &[Relink]) -> Result<Vec<RelinkItem>, String> {
    wanted
        .iter()
        .map(|entry| {
            let media = project
                .media
                .iter()
                .find(|item| item.id == entry.media_id)
                .ok_or_else(|| format!("unknown media {}", entry.media_id))?;
            let (status, reason) = if !Path::new(&entry.path).is_file() {
                ("missing", Some(format!("{} is not a file", entry.path)))
            } else {
                match probe(&entry.path) {
                    Err(error) => ("unreadable", Some(error)),
                    Ok(summary) => match incompatibility(media, &summary) {
                        Some(reason) => ("incompatible", Some(reason)),
                        None => ("ok", None),
                    },
                }
            };
            Ok(RelinkItem {
                media_id: entry.media_id.clone(),
                old_path: media.path.clone(),
                new_path: entry.path.clone(),
                status,
                reason,
            })
        })
        .collect()
}

/// The edit that applies a relink: one `SetMediaPath` each.
pub fn relink_commands(wanted: Vec<Relink>) -> Vec<Command> {
    wanted
        .into_iter()
        .map(|entry| Command::SetMediaPath {
            media_id: entry.media_id,
            path: entry.path,
        })
        .collect()
}

/// Why a replacement is not the file the item was, if it is not.
pub fn incompatibility(media: &MediaItem, summary: &MediaSummary) -> Option<String> {
    if summary.kind != media.kind {
        return Some(format!(
            "kind changed from {:?} to {:?}",
            media.kind, summary.kind
        ));
    }
    if media.has_audio && summary.audio.is_none() {
        return Some("the original had audio and the replacement does not".to_owned());
    }
    if let (Some(old), Some(new)) = (media.duration, summary.duration)
        && new + 0.05 < old
    {
        return Some(format!(
            "the replacement is shorter ({new:.2}s) than the original ({old:.2}s)"
        ));
    }
    None
}

/// Where a project folder is moving from and to.
#[derive(Clone, Debug)]
pub struct MovePlan {
    /// The project's folder, resolved.
    pub old_root: PathBuf,
    /// Where it is going: an exact new folder, its parent resolved.
    pub target: PathBuf,
}

/// Checks a move before anything is closed or renamed: the destination is
/// absolute, does not exist, has a parent that does, and is not inside the
/// project itself.
pub fn plan_move(project_path: &str, destination: &str) -> Result<MovePlan, String> {
    let target = PathBuf::from(destination);
    if !target.is_absolute() {
        return Err(format!("{destination} is not an absolute path"));
    }
    if target.exists() {
        return Err(format!("{destination} already exists"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| format!("{destination} has no parent folder"))?;
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("{} does not exist: {error}", parent.display()))?;
    let target = parent.join(target.file_name().ok_or("destination has no name")?);
    let old_root = Path::new(project_path)
        .canonicalize()
        .map_err(|error| format!("could not resolve {project_path}: {error}"))?;
    if target.starts_with(&old_root) {
        return Err("a project cannot be moved inside itself".to_owned());
    }
    Ok(MovePlan { old_root, target })
}

/// The edit that repoints media living inside the old folder at the new one,
/// so nothing dangles after the move.
pub fn repoint_after_move(project: &Project, plan: &MovePlan) -> Vec<Command> {
    let old_prefix = format!("{}/", plan.old_root.display());
    let new_prefix = format!("{}/", plan.target.display());
    project
        .media
        .iter()
        .filter_map(|item| {
            item.path
                .strip_prefix(&old_prefix)
                .map(|rest| Command::SetMediaPath {
                    media_id: item.id.clone(),
                    path: format!("{new_prefix}{rest}"),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_must_stay_outside_the_project_and_not_overwrite() {
        let base = std::env::temp_dir().join(format!("puppetcat-move-{}", std::process::id()));
        let project = base.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();

        let inside = project.join("inner");
        assert!(plan_move(&project_path, &inside.to_string_lossy()).is_err());
        assert!(plan_move(&project_path, "relative/dir").is_err());
        assert!(plan_move(&project_path, &project_path).is_err(), "exists");
        let fine = base.join("moved");
        let plan = plan_move(&project_path, &fine.to_string_lossy()).expect("plans");
        assert!(plan.target.ends_with("moved"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_root_swap_only_touches_items_under_the_old_root() {
        let mut project = Project::new();
        project.media.push(media("m1", "/old/a/x.mp4"));
        project.media.push(media("m2", "/elsewhere/y.mp4"));
        let swaps = swap_root(&project, "/old/", "/new");
        assert_eq!(
            swaps,
            vec![Relink {
                media_id: "m1".to_owned(),
                path: "/new/a/x.mp4".to_owned()
            }]
        );
    }

    fn media(id: &str, path: &str) -> MediaItem {
        serde_json::from_value(serde_json::json!({
            "id": id, "path": path, "name": "n", "kind": "video",
            "hasAudio": false
        }))
        .expect("a media item")
    }
}
