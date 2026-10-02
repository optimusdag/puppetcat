// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The core wrapper an agent drives: one project open at a time, every edit
//! a `Command`, nothing here knowing MCP exists. The doctrine is
//! `puppetcat_host`'s own - no editing decision is made here, only session
//! lifecycle and the plumbing an agent needs that the window otherwise
//! provides for a human (seeing the result, waiting on an export).

use std::path::Path;
use std::sync::Mutex;

use puppetcat_host::{Slot, SlotLock};
use puppetcat_media::ReaderPool;
use puppetcat_project::Command;

use crate::export::{ExportJob, ExportStatus};

/// A session plus the cross-process claim that makes it safe to edit: for as
/// long as this exists, no `puppetcat` window will accept input on the same
/// project (see `puppetcat_host::lock`). Bundled with the session, not held
/// separately, so replacing or clearing one always releases the other in the
/// same step - an agent never holds the lock for a project it has stopped
/// editing.
struct OpenProject {
    session: puppetcat_host::Session,
    _lock: SlotLock,
}

impl std::ops::Deref for OpenProject {
    type Target = puppetcat_host::Session;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl std::ops::DerefMut for OpenProject {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

/// One open project's session, plus what an agent needs around it that a
/// human gets from the window: a place to render a still, and the one
/// export slot.
pub struct AgentEngine {
    dirs: puppetcat_host::AppDirs,
    pool: ReaderPool,
    titles: puppetcat_host::Titles,
    open: Mutex<Option<OpenProject>>,
    export: ExportJob,
}

impl AgentEngine {
    /// Locates this machine's app directories and starts with no project
    /// open.
    pub fn new() -> Result<Self, String> {
        Self::with_dirs(puppetcat_host::AppDirs::locate()?)
    }

    /// Starts with no project open, against `dirs` rather than this
    /// machine's real ones - the seam tests use so they never touch the
    /// real recents list (`puppetcat_host::AppDirs::under` builds a scratch
    /// one).
    fn with_dirs(dirs: puppetcat_host::AppDirs) -> Result<Self, String> {
        let titles = puppetcat_host::Titles::new(&dirs);
        Ok(Self {
            dirs,
            pool: ReaderPool::with_defaults(),
            titles,
            open: Mutex::new(None),
            export: ExportJob::new(),
        })
    }

    fn session_lock(&self) -> std::sync::MutexGuard<'_, Option<OpenProject>> {
        self.open.lock().expect("session mutex")
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
        let info =
            puppetcat_host::projects::create(location, name, width, height, rate_num, rate_den)?;
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
        let mut guard = self.session_lock();
        // Release this project's own lock first when it is the very path
        // being reopened, so reopening it never reads as some other agent
        // already holding it. A *different* open project is left alone
        // until the new one has actually opened, so a failed switch does
        // not lose it.
        if guard.as_ref().is_some_and(|open| {
            puppetcat_host::projects::same_path(open.session.path(), &info.path)
        }) {
            *guard = None;
        }
        let lock = SlotLock::try_acquire(Path::new(&info.path), Slot::Agent)
            .ok_or_else(|| "another agent session already has this project open".to_owned())?;
        let session = puppetcat_host::Session::open_info(info)?;
        let _ = puppetcat_host::projects::remember(&self.dirs.config, info);
        *guard = Some(OpenProject {
            session,
            _lock: lock,
        });
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
    ///
    /// `range` limits the export to part of the timeline; see
    /// [`ExportRangeArgs`].
    pub fn start_export(
        &self,
        output: &str,
        crf: u8,
        preset: &str,
        range: &ExportRangeArgs,
    ) -> Result<(), String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        let range = range.resolve(session.project().active())?;
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
            scratch_dir: puppetcat_host::export::temp_folder_preference(&self.dirs),
            crf,
            preset: preset.to_owned(),
            range,
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

/// Which part of the timeline an export covers, as an agent states it: by
/// seconds, by marker, or a mix (a marker for one end, seconds for the
/// other). Nothing at all is the whole timeline.
#[derive(Clone, Debug, Default)]
pub struct ExportRangeArgs {
    /// Seconds where the extract begins.
    pub start: Option<f64>,
    /// Seconds where the extract ends.
    pub end: Option<f64>,
    /// A marker (id or name) on the active timeline to begin at.
    pub from_marker: Option<String>,
    /// A marker (id or name) on the active timeline to end at.
    pub to_marker: Option<String>,
}

impl ExportRangeArgs {
    /// The `(start, end)` seconds this names on `timeline`, or `None` for
    /// the whole thing. An end left open runs to the end of the edit, a
    /// start left open begins at zero - but naming a marker or a time for
    /// either end makes it a range.
    fn resolve(
        &self,
        timeline: &puppetcat_project::model::Timeline,
    ) -> Result<Option<(f64, f64)>, String> {
        let marker_time = |key: &str| -> Result<f64, String> {
            let matches =
                |marker: &&puppetcat_project::model::Marker| marker.id == key || marker.name == key;
            timeline
                .markers
                .iter()
                .find(matches)
                .map(|marker| marker.time)
                .ok_or_else(|| format!("no marker \"{key}\" on the active timeline"))
        };
        if self.start.is_some() && self.from_marker.is_some() {
            return Err("give either start or fromMarker, not both".to_owned());
        }
        if self.end.is_some() && self.to_marker.is_some() {
            return Err("give either end or toMarker, not both".to_owned());
        }
        let start = match (&self.from_marker, self.start) {
            (Some(key), _) => Some(marker_time(key)?),
            (None, start) => start,
        };
        let end = match (&self.to_marker, self.end) {
            (Some(key), _) => Some(marker_time(key)?),
            (None, end) => end,
        };
        if start.is_none() && end.is_none() {
            return Ok(None);
        }
        let start = start.unwrap_or(0.0);
        let end = end.unwrap_or(f64::MAX);
        if end <= start {
            return Err(format!(
                "the export range is empty: it ends at {end:.2}s, before it starts at {start:.2}s"
            ));
        }
        Ok(Some((start, end)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_export_range_resolves_markers_and_seconds() {
        use puppetcat_project::model::{Marker, Project};
        let mut project = Project::new();
        for (id, name, time) in [("mk1", "intro", 2.0), ("mk2", "outro", 8.0)] {
            project.active_mut().markers.push(Marker {
                id: id.to_owned(),
                name: name.to_owned(),
                time,
                clip_id: None,
                offset: 0.0,
            });
        }
        let timeline = project.active();
        let args = |start, end, from: Option<&str>, to: Option<&str>| ExportRangeArgs {
            start,
            end,
            from_marker: from.map(str::to_owned),
            to_marker: to.map(str::to_owned),
        };
        assert_eq!(args(None, None, None, None).resolve(timeline), Ok(None));
        assert_eq!(
            args(None, None, Some("mk1"), Some("outro")).resolve(timeline),
            Ok(Some((2.0, 8.0)))
        );
        assert_eq!(
            args(Some(1.0), None, None, Some("mk2")).resolve(timeline),
            Ok(Some((1.0, 8.0)))
        );
        assert!(
            args(None, None, Some("nope"), None)
                .resolve(timeline)
                .is_err()
        );
        assert!(
            args(Some(1.0), None, Some("mk1"), None)
                .resolve(timeline)
                .is_err()
        );
        assert!(
            args(Some(5.0), Some(5.0), None, None)
                .resolve(timeline)
                .is_err()
        );
    }

    /// A scratch root for one test: a project location and, critically, its
    /// own `AppDirs` (see `engine` below) - the tests here must never touch
    /// this machine's real recents list.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "puppetcat-agent-engine-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// An `AgentEngine` rooted entirely under `root`, so `create_project`'s
    /// call to `projects::remember` writes a scratch recents list instead of
    /// this machine's real one.
    fn engine(root: &std::path::Path) -> AgentEngine {
        AgentEngine::with_dirs(puppetcat_host::AppDirs::under(&root.join("dirs")))
            .expect("builds against a scratch AppDirs")
    }

    /// Two `AgentEngine`s stand in for two separate `puppetcat-agent-mcp`
    /// processes: each opens its own file handle on the same lock file, so
    /// this exercises the same OS-level exclusion two real processes would
    /// hit, without actually spawning any.
    #[test]
    fn a_second_agent_cannot_open_a_project_the_first_already_has() {
        let root = scratch("two-agents");
        let first = engine(&root);
        let info = first
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Shared",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates and opens");

        let second = engine(&root);
        let error = second
            .open_project(&info.path)
            .expect_err("the first agent still has it open");
        assert!(error.contains("already"), "got: {error}");

        first.close_project().expect("releases the lock");
        second
            .open_project(&info.path)
            .expect("free once the first agent closes it");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The window's slot is independent of the agent's: a window watching
    /// this project must be able to see the agent's slot as held without
    /// that check ever depending on, or contending with, the window's own.
    #[test]
    fn the_agent_slot_never_blocks_the_window_slot() {
        let root = scratch("agent-vs-window");
        let agent = engine(&root);
        let info = agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Shared",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates and opens");

        assert!(puppetcat_host::SlotLock::is_held(
            Path::new(&info.path),
            Slot::Agent
        ));
        let window = puppetcat_host::SlotLock::try_acquire(Path::new(&info.path), Slot::Window);
        assert!(window.is_some(), "the window slot must still be free");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Reopening the very project already open here must not read as some
    /// other agent holding it - a client re-issuing `open_project` on its
    /// own project should just get a fresh session, not an error.
    #[test]
    fn reopening_the_same_project_here_succeeds() {
        let root = scratch("reopen-self");
        let agent = engine(&root);
        let info = agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Shared",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates and opens");
        agent
            .open_project(&info.path)
            .expect("reopening its own project is not a conflict");
        let _ = std::fs::remove_dir_all(&root);
    }
}
