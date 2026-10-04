// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Keeping the project in step with the disk: relinking media whose files
//! moved, and renaming or moving the project itself.
//!
//! The checks - is each replacement there, is it readable, is it what it
//! replaces; is this move allowed - are `puppetcat_host::lifecycle`, the same
//! functions the agent's `relink_media` and `move_project` call. This file is
//! the window's half: the two sheets, the file pickers, and running the
//! checks off the event loop.

use std::path::Path;

use puppetcat_host::lifecycle::{self, Relink, RelinkItem};

use super::Studio;
use crate::host::spawn;
use crate::ui::{ProjectData, RelinkData, RelinkRowData};

/// How long a typed folder rests before the files are checked again.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

/// The relink sheet's state.
#[derive(Default)]
pub struct RelinkState {
    pub open: bool,
    pub old_root: String,
    pub new_root: String,
    /// Files the editor located by hand, one per item.
    pub located: Vec<Relink>,
    /// What the last check decided, item by item.
    pub items: Vec<RelinkItem>,
    /// The replacements those verdicts are about, in the same order.
    pub wanted: Vec<Relink>,
    pub busy: bool,
    pub generation: u64,
    pub settle: slint::Timer,
}

/// The project sheet's state.
#[derive(Default)]
pub struct ProjectSheet {
    pub open: bool,
    pub name: String,
    pub new_parent: String,
    pub error: String,
}

/// The folder every path in `paths` lives under, deepest first.
fn common_folder(paths: &[String]) -> String {
    let mut folders = paths
        .iter()
        .filter_map(|path| Path::new(path).parent().map(Path::to_path_buf));
    let Some(mut common) = folders.next() else {
        return String::new();
    };
    for folder in folders {
        while !folder.starts_with(&common) {
            if !common.pop() {
                return String::new();
            }
        }
    }
    common.to_string_lossy().into_owned()
}

impl Studio {
    // ── relink ──

    /// Opens the relink sheet on whatever is missing.
    pub fn open_relink(&mut self) {
        let missing = lifecycle::missing_media(self.project());
        if missing.is_empty() {
            self.notify("No media is missing", false);
            return;
        }
        let paths: Vec<String> = missing.iter().map(|(_, path)| path.clone()).collect();
        self.relink = RelinkState {
            open: true,
            old_root: common_folder(&paths),
            ..RelinkState::default()
        };
    }

    pub fn close_relink(&mut self) {
        self.relink.open = false;
        self.relink.generation += 1;
        self.relink.settle.stop();
    }

    /// A folder was typed: check the files again once typing rests.
    pub fn relink_roots_edited(&mut self) {
        self.relink.busy = true;
        self.relink
            .settle
            .start(slint::TimerMode::SingleShot, SETTLE, || {
                crate::host::Shell::with(|shell, app| {
                    shell.studio.borrow_mut().recheck_relink();
                    shell.studio.borrow().publish(&app, &shell.models);
                });
            });
    }

    /// Picks the folder the files live in now.
    pub fn relink_browse(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Where are the files now?")
            .pick_folder()
        {
            self.relink.new_root = folder.to_string_lossy().into_owned();
            self.recheck_relink();
        }
    }

    /// Picks the file for one missing item.
    pub fn relink_locate(&mut self, index: usize) {
        let missing = lifecycle::missing_media(self.project());
        let Some((media_id, old)) = missing.get(index).cloned() else {
            return;
        };
        let mut dialog = rfd::FileDialog::new().set_title("Locate the file");
        if let Some(parent) = Path::new(&old).parent()
            && parent.is_dir()
        {
            dialog = dialog.set_directory(parent);
        }
        if let Some(file) = dialog.pick_file() {
            self.relink
                .located
                .retain(|entry| entry.media_id != media_id);
            self.relink.located.push(Relink {
                media_id,
                path: file.to_string_lossy().into_owned(),
            });
            self.recheck_relink();
        }
    }

    /// Works out the replacements - the ones chosen by hand, then every other
    /// missing item the folder swap reaches - and checks them off the event
    /// loop.
    fn recheck_relink(&mut self) {
        let project = self.project().clone();
        let mut wanted = self.relink.located.clone();
        if !self.relink.new_root.trim().is_empty() {
            let missing: Vec<String> = lifecycle::missing_media(&project)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            for swap in lifecycle::swap_root(&project, &self.relink.old_root, &self.relink.new_root)
            {
                if missing.contains(&swap.media_id)
                    && !wanted.iter().any(|entry| entry.media_id == swap.media_id)
                {
                    wanted.push(swap);
                }
            }
        }
        self.relink.generation += 1;
        let generation = self.relink.generation;
        self.relink.busy = true;
        spawn(
            move || {
                let items = lifecycle::check_relinks(&project, &wanted)?;
                Ok::<_, String>((wanted, items))
            },
            move |studio, _, _, result| {
                if studio.relink.generation != generation || !studio.relink.open {
                    return;
                }
                studio.relink.busy = false;
                match result {
                    Ok((wanted, items)) => {
                        studio.relink.wanted = wanted;
                        studio.relink.items = items;
                    }
                    Err(error) => {
                        studio.relink.wanted.clear();
                        studio.relink.items.clear();
                        studio.notify(&error, true);
                    }
                }
            },
        );
    }

    /// Relinks everything that checked out, as one edit.
    pub fn apply_relink(&mut self) {
        let ready: Vec<Relink> = self
            .relink
            .wanted
            .iter()
            .zip(&self.relink.items)
            .filter(|(_, item)| item.status == "ok")
            .map(|(entry, _)| entry.clone())
            .collect();
        let count = ready.len();
        if count > 0 && self.apply_all(lifecycle::relink_commands(ready)) {
            self.close_relink();
            self.notify(
                &format!("Relinked {count} file{}", if count == 1 { "" } else { "s" }),
                false,
            );
        }
    }

    /// The sheet's rows: every missing item, with the verdict on its
    /// replacement once there is one.
    pub fn relink_rows(&self) -> Vec<RelinkRowData> {
        lifecycle::missing_media(self.project())
            .into_iter()
            .map(|(id, old_path)| {
                let name = self
                    .project()
                    .media
                    .iter()
                    .find(|item| item.id == id)
                    .map_or_else(|| id.clone(), |item| item.name.clone());
                let verdict = self.relink.items.iter().find(|item| item.media_id == id);
                RelinkRowData {
                    name: name.into(),
                    old_path: old_path.into(),
                    new_path: verdict
                        .map_or_else(String::new, |item| item.new_path.clone())
                        .into(),
                    status: verdict.map_or("missing", |item| item.status).into(),
                    reason: verdict
                        .and_then(|item| item.reason.clone())
                        .unwrap_or_default()
                        .into(),
                }
            })
            .collect()
    }

    pub fn relink_data(&self) -> RelinkData {
        let ok = self
            .relink
            .items
            .iter()
            .filter(|item| item.status == "ok")
            .count();
        let total = lifecycle::missing_media(self.project()).len();
        let blocked = self.relink.items.iter().any(|item| item.status != "ok");
        RelinkData {
            open: self.relink.open,
            old_root: self.relink.old_root.as_str().into(),
            new_root: self.relink.new_root.as_str().into(),
            summary: if self.relink.busy {
                "Checking…".to_owned()
            } else {
                format!("{ok} of {total} will be relinked")
            }
            .into(),
            busy: self.relink.busy,
            can_apply: ok > 0 && !blocked,
        }
    }

    // ── rename and move ──

    pub fn open_project_sheet(&mut self) {
        self.project_sheet = ProjectSheet {
            open: true,
            name: self.project_name.clone(),
            ..ProjectSheet::default()
        };
    }

    pub fn project_sheet_browse(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Move the project into")
            .pick_folder()
        {
            self.project_sheet.new_parent = folder.to_string_lossy().into_owned();
        }
    }

    /// Renames, then moves if a folder was chosen. A move saves and closes the
    /// project, renames its folder, reopens it there and repoints the media
    /// that lived inside - or, if the rename fails, reopens it where it was.
    pub fn apply_project_sheet(&mut self) {
        let name = self.project_sheet.name.trim().to_owned();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if !name.is_empty() && name != self.project_name {
            if let Err(error) = session.save(Some(&name), None, None) {
                self.project_sheet.error = error;
                return;
            }
            self.project_name = name;
        }
        if self.project_sheet.new_parent.trim().is_empty() {
            self.project_sheet.open = false;
            self.notify("Project renamed", false);
            return;
        }
        let path = session.path().to_owned();
        let folder_name = Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let destination = Path::new(self.project_sheet.new_parent.trim()).join(folder_name);
        let plan = match lifecycle::plan_move(&path, &destination.to_string_lossy()) {
            Ok(plan) => plan,
            Err(error) => {
                self.project_sheet.error = error;
                return;
            }
        };
        self.close_project();
        if self.session.is_some() {
            // Closing refused (the save failed): nothing has moved.
            return;
        }
        let renamed = std::fs::rename(&plan.old_root, &plan.target);
        let reopen_at = if renamed.is_ok() {
            &plan.target
        } else {
            &plan.old_root
        };
        self.open_recent(&reopen_at.to_string_lossy());
        self.project_sheet.open = false;
        match renamed {
            Err(error) => self.notify(
                &format!("Could not move the project: {error}; it is unchanged"),
                true,
            ),
            Ok(()) => {
                self.forget_recent(&plan.old_root.to_string_lossy());
                let commands = lifecycle::repoint_after_move(self.project(), &plan);
                if !commands.is_empty() {
                    self.apply_all(commands);
                    self.save(false);
                }
                self.notify("Project moved", false);
            }
        }
    }

    pub fn project_data(&self) -> ProjectData {
        let folder = self
            .session
            .as_ref()
            .map(|session| session.path().to_owned())
            .unwrap_or_default();
        ProjectData {
            open: self.project_sheet.open,
            name: self.project_sheet.name.as_str().into(),
            folder: folder.into(),
            new_parent: self.project_sheet.new_parent.as_str().into(),
            error: self.project_sheet.error.as_str().into(),
            changed: self.project_sheet.name.trim() != self.project_name
                || !self.project_sheet.new_parent.trim().is_empty(),
        }
    }
}
