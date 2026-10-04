// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Project and media lifecycle: the filesystem-touching operations an agent
//! otherwise does with `mv` and hand-edited JSON. Every one resolves exact
//! paths, refuses to overwrite by default, and leaves the project untouched
//! on failure.

use std::path::{Path, PathBuf};

use puppetcat_host::lifecycle::{self, Relink};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use super::{AgentEngine, no_project};

/// One media item to repoint.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RelinkEntry {
    /// The bin item.
    pub media_id: String,
    /// The file that replaces it.
    pub path: String,
}

/// The outcome of [`AgentEngine::relink_media`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelinkReport {
    /// Whether the project was changed.
    pub committed: bool,
    /// Echo of the request flag.
    pub dry_run: bool,
    /// Per-item verdicts. Any non-`ok` item blocks the whole relink.
    pub items: Vec<lifecycle::RelinkItem>,
}

/// The outcome of [`AgentEngine::move_output`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveOutputReport {
    /// Where the file is now.
    pub destination: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Whether the copy was verified byte-for-byte before the source was removed.
    pub verified: bool,
}

impl AgentEngine {
    /// Sets where exports go and saves it. The folder must exist unless
    /// `create` is set.
    pub fn set_export_directory(&self, directory: &str, create: bool) -> Result<String, String> {
        let path = PathBuf::from(directory);
        if !path.is_absolute() {
            return Err(format!("{directory} is not an absolute path"));
        }
        if create {
            std::fs::create_dir_all(&path)
                .map_err(|error| format!("could not create {directory}: {error}"))?;
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("{directory} does not exist: {error}"))?;
        if !canonical.is_dir() {
            return Err(format!("{} is not a folder", canonical.display()));
        }
        let text = canonical.to_string_lossy().into_owned();
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        session.set_export_folder(text.clone());
        session.save(None, None, None)?;
        Ok(text)
    }

    /// Renames the project (its display name and manifest), not its folder.
    pub fn rename_project(&self, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a project name cannot be blank".to_owned());
        }
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        session.save(Some(name), None, None)?;
        Ok(name.to_owned())
    }

    /// Repoints media at replacement files. Either explicit `entries`, or a
    /// bulk `old_root` -> `new_root` prefix swap over every item under
    /// `old_root`. Each replacement is probed and checked against what the
    /// item was; if any is missing, unreadable or incompatible nothing
    /// changes. The commit is one undo step.
    pub fn relink_media(
        &self,
        entries: Vec<RelinkEntry>,
        roots: Option<(String, String)>,
        dry_run: bool,
    ) -> Result<RelinkReport, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        let mut wanted: Vec<Relink> = entries
            .into_iter()
            .map(|entry| Relink {
                media_id: entry.media_id,
                path: entry.path,
            })
            .collect();
        if let Some((old_root, new_root)) = roots {
            wanted.extend(lifecycle::swap_root(
                session.project(),
                &old_root,
                &new_root,
            ));
        }
        if wanted.is_empty() {
            return Err("nothing to relink: no entries and no media under oldRoot".to_owned());
        }
        let items = lifecycle::check_relinks(session.project(), &wanted)?;
        let all_ok = items.iter().all(|item| item.status == "ok");
        let mut committed = false;
        if all_ok && !dry_run {
            let staged = session.stage(lifecycle::relink_commands(wanted))?;
            committed = staged.applied;
            session.commit(staged);
        }
        Ok(RelinkReport {
            committed,
            dry_run,
            items,
        })
    }

    /// Moves the open project's folder to `destination` (an exact new folder
    /// path whose parent exists), then reopens it there. Refuses an existing
    /// destination. Media inside the old folder - the generated black asset,
    /// for one - is repointed so nothing dangles. The move is a same-volume
    /// rename; across volumes it fails and changes nothing.
    pub fn move_project(&self, destination: &str) -> Result<puppetcat_host::ProjectInfo, String> {
        let plan = {
            let mut guard = self.session_lock();
            let session = guard.as_mut().ok_or_else(no_project)?;
            let plan = lifecycle::plan_move(session.path(), destination)?;
            session.save(None, None, None)?;
            plan
        };
        let old_path = plan.old_root.to_string_lossy().into_owned();

        // Release the lock for the duration of the rename, then reopen at
        // whichever path holds the project.
        *self.session_lock() = None;
        let renamed = std::fs::rename(&plan.old_root, &plan.target);
        let reopen_at = if renamed.is_ok() {
            &plan.target
        } else {
            &plan.old_root
        };
        let info = self.open_project(&reopen_at.to_string_lossy())?;
        if let Err(error) = renamed {
            return Err(format!(
                "could not move to {}: {error}; the project is unchanged",
                plan.target.display()
            ));
        }
        let _ = puppetcat_host::projects::forget(&self.dirs.config, &old_path);

        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        let commands = lifecycle::repoint_after_move(session.project(), &plan);
        if !commands.is_empty() {
            let staged = session.stage(commands)?;
            session.commit(staged);
            session.save(None, None, None)?;
        }
        Ok(info)
    }

    /// Moves a rendered file. Never overwrites unless `overwrite`. Copies and
    /// verifies size (and bytes when `verify`) before removing the source,
    /// so an interrupted move leaves the original intact.
    pub fn move_output(
        &self,
        source: &str,
        destination: &str,
        overwrite: bool,
        verify: bool,
    ) -> Result<MoveOutputReport, String> {
        let (source, destination) = (Path::new(source), Path::new(destination));
        if !source.is_file() {
            return Err(format!("{} is not a file", source.display()));
        }
        if destination.exists() && !overwrite {
            return Err(format!(
                "{} already exists; pass overwrite to replace it",
                destination.display()
            ));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        let name = destination
            .file_name()
            .ok_or("destination has no file name")?
            .to_string_lossy();
        let partial = destination.with_file_name(format!(".{name}.part"));
        let result = std::fs::copy(source, &partial)
            .map_err(|error| format!("could not copy: {error}"))
            .and_then(|bytes| {
                let expected = std::fs::metadata(source).map_err(|e| e.to_string())?.len();
                if bytes != expected {
                    return Err(format!("copied {bytes} of {expected} bytes"));
                }
                if verify
                    && std::fs::read(source).map_err(|e| e.to_string())?
                        != std::fs::read(&partial).map_err(|e| e.to_string())?
                {
                    return Err("the copy does not match the source".to_owned());
                }
                std::fs::rename(&partial, destination).map_err(|e| e.to_string())?;
                Ok(bytes)
            });
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                return Err(error);
            }
        };
        std::fs::remove_file(source)
            .map_err(|error| format!("moved, but could not remove the source: {error}"))?;
        Ok(MoveOutputReport {
            destination: destination.to_string_lossy().into_owned(),
            bytes,
            verified: verify,
        })
    }
}
