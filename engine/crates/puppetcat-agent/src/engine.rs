// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The core wrapper an agent drives: one project open at a time, every edit
//! a `Command`, nothing here knowing MCP exists. The doctrine is
//! `puppetcat_host`'s own - no editing decision is made here, only session
//! lifecycle and the plumbing an agent needs that the window otherwise
//! provides for a human (seeing the result, waiting on an export).

use std::sync::Mutex;

use puppetcat_media::ReaderPool;
use puppetcat_project::Command;

use crate::export::{ExportJob, ExportStatus};

/// One open project's session, plus what an agent needs around it that a
/// human gets from the window: a place to render a still, and the one
/// export slot.
pub struct AgentEngine {
    dirs: puppetcat_host::AppDirs,
    pool: ReaderPool,
    titles: puppetcat_host::Titles,
    session: Mutex<Option<puppetcat_host::Session>>,
    export: ExportJob,
}

impl AgentEngine {
    /// Locates this machine's app directories and starts with no project
    /// open.
    pub fn new() -> Result<Self, String> {
        let dirs = puppetcat_host::AppDirs::locate()?;
        let titles = puppetcat_host::Titles::new(&dirs);
        Ok(Self {
            dirs,
            pool: ReaderPool::with_defaults(),
            titles,
            session: Mutex::new(None),
            export: ExportJob::new(),
        })
    }

    fn session_lock(&self) -> std::sync::MutexGuard<'_, Option<puppetcat_host::Session>> {
        self.session.lock().expect("session mutex")
    }

    /// Creates a new project folder and opens it as the active session.
    pub fn create_project(
        &self,
        location: &str,
        name: &str,
        width: u32,
        height: u32,
        rate_num: i64,
        rate_den: i64,
    ) -> Result<puppetcat_host::ProjectInfo, String> {
        let info = puppetcat_host::projects::create(location, name, width, height, rate_num, rate_den)?;
        self.open_session(&info)?;
        Ok(info)
    }

    /// Opens an existing project folder as the active session, replacing
    /// whatever was open (unsaved edits are lost - call `save_project`
    /// first if they matter).
    pub fn open_project(&self, path: &str) -> Result<puppetcat_host::ProjectInfo, String> {
        let info = puppetcat_host::projects::open(path)?;
        self.open_session(&info)?;
        Ok(info)
    }

    fn open_session(&self, info: &puppetcat_host::ProjectInfo) -> Result<(), String> {
        let session = puppetcat_host::Session::open_info(info)?;
        let _ = puppetcat_host::projects::remember(&self.dirs.config, info);
        *self.session_lock() = Some(session);
        Ok(())
    }

    /// Saves the active session, then closes it. A no-op if nothing is open.
    pub fn close_project(&self) -> Result<(), String> {
        let mut guard = self.session_lock();
        if let Some(session) = guard.as_mut() {
            session.save(None, None, None)?;
        }
        *guard = None;
        Ok(())
    }

    /// Saves the active session in place.
    pub fn save_project(&self) -> Result<(), String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        session.save(None, None, None)
    }

    /// Every project this machine has opened before, most recent first.
    pub fn list_recent_projects(&self) -> Vec<puppetcat_host::ProjectInfo> {
        puppetcat_host::projects::list(&self.dirs.config)
    }

    /// The active session's current state - the project, undo/redo flags,
    /// settings - without applying anything.
    pub fn get_state(&self) -> Result<puppetcat_host::EditorView, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        Ok(session.view())
    }

    /// The one place every edit - curated tool or raw escape hatch alike -
    /// funnels through, same as `Session::apply` already is for the window.
    pub fn apply(&self, command: Command) -> Result<puppetcat_host::EditorView, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        session.apply(command)
    }

    /// Undoes the last applied edit.
    pub fn undo(&self) -> Result<puppetcat_host::EditorView, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        Ok(session.undo())
    }

    /// Redoes the last undone edit.
    pub fn redo(&self) -> Result<puppetcat_host::EditorView, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        Ok(session.redo())
    }

    /// Probes a file and applies it into the bin as one step - the agent's
    /// two-step import (probe, then `AddMedia`) collapsed into one call.
    pub fn import_media(&self, path: &str) -> Result<puppetcat_host::EditorView, String> {
        let summary = puppetcat_host::media::probe(path)?;
        self.apply(Command::AddMedia {
            item: summary.to_new_media(),
        })
    }

    /// Renders the active session at `time` to PNG bytes - the "see the
    /// timeline" capability the window has and an agent otherwise wouldn't.
    pub fn render_snapshot(
        &self,
        time: f64,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        crate::snapshot::render_still(session, &self.titles, &self.pool, time, width, height)
    }

    /// Starts exporting the active session. Errs immediately if one is
    /// already running; use `export_status` to poll the render itself.
    pub fn start_export(
        &self,
        output: &str,
        crf: u8,
        preset: &str,
    ) -> Result<(), String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        let titles = self
            .titles
            .clips(
                session.project(),
                session.settings().width,
                session.settings().height,
            )
            .into_iter()
            .map(|title| title.clip)
            .collect();
        let spec = puppetcat_host::export::ExportSpec {
            output: output.to_owned(),
            crf,
            preset: preset.to_owned(),
        };
        self.export.start(session, spec, titles)
    }

    /// The running (or most recently finished) export's last known progress.
    pub fn export_status(&self) -> ExportStatus {
        self.export.status()
    }
}

fn no_project() -> String {
    "no project is open - call create_project or open_project first".to_owned()
}
