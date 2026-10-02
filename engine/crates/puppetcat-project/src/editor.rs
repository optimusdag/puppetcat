// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The editing session: a project, its id mint, and undo.
//!
//! Undo is whole-state snapshots. The document is small - a heavy project is
//! a few hundred kilobytes - so cloning per edit is cheaper than maintaining
//! inverse operations and impossible to get wrong. The depth cap bounds the
//! memory; two hundred undos of a large project is tens of megabytes, which
//! an editor holding gigabytes of frame data does not notice.

use std::collections::VecDeque;

use serde_json::Value;

use crate::commands::{Command, CommandError, IdMint, Outcome, apply};
use crate::doc::{DocumentSettings, from_document, to_document};
use crate::model::Project;

const UNDO_DEPTH: usize = 200;

/// Each editor starts its revision in its own epoch, so a plan built against
/// one open of a project can never match a later reopen of it.
fn next_epoch() -> u64 {
    static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    (EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1) << 32
}

/// One editing session: the project, the id mint that keeps its ids unique,
/// and the undo/redo stacks. This is the object a host holds per open
/// project; everything else in the crate is reachable through it.
pub struct Editor {
    project: Project,
    mint: IdMint,
    /// Oldest snapshot at the front: overflowing the depth cap evicts from
    /// the front in constant time, where a Vec would shuffle the whole stack.
    undo: VecDeque<Project>,
    redo: Vec<Project>,
    /// Bumped by every successful mutation, undo and redo, so a caller can
    /// detect that the state moved since it last looked.
    revision: u64,
}

/// Why staging a plan failed.
#[derive(Debug)]
pub enum StageError {
    /// A command could not be built from the plan's own context.
    Build(String),
    /// A command was rejected by the editor.
    Command(CommandError),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(message) => f.write_str(message),
            Self::Command(error) => write!(f, "{error}"),
        }
    }
}

/// A plan applied to a copy of the session: nothing is live until
/// [`Editor::commit`] takes it.
pub struct Staged {
    /// The project as it would be after every command.
    pub project: Project,
    mint: IdMint,
    /// Per command, the id it minted, if any.
    pub created_ids: Vec<Option<String>>,
    /// Whether any command changed anything.
    pub applied: bool,
}

impl Editor {
    /// A fresh, empty project.
    pub fn new() -> Self {
        Self {
            project: Project::new(),
            mint: IdMint::default(),
            undo: VecDeque::new(),
            redo: Vec::new(),
            revision: next_epoch(),
        }
    }

    /// Restores a project from a document, adopting every id it uses so the
    /// mint can never re-issue one. Returns None when the document holds
    /// nothing recognisable.
    pub fn from_document(document: &Value) -> Option<Self> {
        let project = from_document(document)?;
        let mut mint = IdMint::default();
        mint.adopt_project(&project);
        Some(Self {
            project,
            mint,
            undo: VecDeque::new(),
            redo: Vec::new(),
            revision: next_epoch(),
        })
    }

    /// The current state, read-only: all mutation goes through
    /// [`Editor::apply`] so nothing can change without being undoable.
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Applies one command, recording the state before it for undo.
    ///
    /// A command that fails leaves the project and history untouched, and
    /// the snapshot is only kept when the command reports it actually
    /// changed something ([`Outcome::applied`]), so undo never replays a
    /// no-op - a missing id, a value already in place.
    pub fn apply(&mut self, command: Command) -> Result<Outcome, CommandError> {
        let before = self.project.clone();
        let outcome = apply(&mut self.project, &mut self.mint, command)?;
        if outcome.applied {
            self.undo.push_back(before);
            if self.undo.len() > UNDO_DEPTH {
                self.undo.pop_front();
            }
            self.redo.clear();
            self.revision += 1;
        }
        Ok(outcome)
    }

    /// The change counter; differs after every edit, undo and redo.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Applies `commands` in order to a copy, leaving the session untouched.
    /// The first failing command aborts the whole plan.
    pub fn stage(&self, commands: Vec<Command>) -> Result<Staged, CommandError> {
        let mut commands = commands.into_iter();
        let count = commands.len();
        self.stage_with(count, |_, _| Ok(commands.next().expect("counted")))
            .map_err(|error| match error {
                StageError::Command(error) => error,
                StageError::Build(_) => unreachable!("building cannot fail here"),
            })
    }

    /// Like [`Editor::stage`], but each command is built just in time from
    /// the ids the earlier ones minted - what lets a plan refer to a clip it
    /// creates itself. `build` gets the index and the ids minted so far.
    pub fn stage_with(
        &self,
        count: usize,
        mut build: impl FnMut(usize, &[Option<String>]) -> Result<Command, String>,
    ) -> Result<Staged, StageError> {
        let mut project = self.project.clone();
        let mut mint = self.mint.clone();
        let mut created_ids = Vec::with_capacity(count);
        let mut applied = false;
        for index in 0..count {
            let command = build(index, &created_ids).map_err(StageError::Build)?;
            let outcome = apply(&mut project, &mut mint, command).map_err(StageError::Command)?;
            applied |= outcome.applied;
            created_ids.push(outcome.created_id);
        }
        Ok(Staged {
            project,
            mint,
            created_ids,
            applied,
        })
    }

    /// Makes a staged plan live as one undo step. The caller must have
    /// staged it from the current revision; a plan that changed nothing is
    /// ignored.
    pub fn commit(&mut self, staged: Staged) {
        if !staged.applied {
            return;
        }
        let before = std::mem::replace(&mut self.project, staged.project);
        self.mint = staged.mint;
        self.undo.push_back(before);
        if self.undo.len() > UNDO_DEPTH {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.revision += 1;
    }

    /// Steps back one edit. Returns whether anything changed.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop_back() {
            Some(previous) => {
                self.redo
                    .push(std::mem::replace(&mut self.project, previous));
                self.revision += 1;
                true
            }
            None => false,
        }
    }

    /// Steps forward again. Returns whether anything changed.
    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo
                    .push_back(std::mem::replace(&mut self.project, next));
                self.revision += 1;
                true
            }
            None => false,
        }
    }

    /// Whether [`Editor::undo`] would do anything - what the UI's undo
    /// button greys out on.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether [`Editor::redo`] would do anything; the redo button's twin
    /// of [`Editor::can_undo`].
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The full document for saving.
    pub fn to_document(&self, settings: &DocumentSettings) -> Value {
        to_document(settings, &self.project)
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_plan_changes_nothing_until_committed_and_undoes_as_one_step() {
        let mut editor = Editor::new();
        let start = editor.revision();
        let staged = editor
            .stage(vec![Command::AddTrack, Command::AddTrack])
            .expect("stages");
        assert!(staged.applied);
        assert_eq!(editor.revision(), start);
        let tracks = editor.project().active().tracks.len();
        assert_eq!(staged.project.active().tracks.len(), tracks + 2);

        editor.commit(staged);
        assert_ne!(editor.revision(), start);
        assert_eq!(editor.project().active().tracks.len(), tracks + 2);
        assert!(editor.undo());
        assert_eq!(editor.project().active().tracks.len(), tracks);
    }
}
