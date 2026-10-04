// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Lending the window's session to an agent's tool call, and taking it back.
//!
//! An agent whose tools run through this window (see `crate::bridge`) edits
//! the very session the window shows: it is taken out of the window for the
//! length of one call, the window waits behind its overlay, and what comes
//! back is shown and saved like any other edit. What it hands back may also
//! be a different project - the call opened or created one - in which case
//! the window switches to it, or none, if the call closed it.

use puppetcat_host::{Session, projects};

use super::{BlockedBy, Studio};

impl Studio {
    /// Hands the session to an agent's call. `Ok(None)` when no project is
    /// open. Refused while a session is already out.
    pub fn lend_session(&mut self) -> Result<Option<Session>, String> {
        if self.lent.is_some() {
            return Err("this window's project is already in use by another call".to_owned());
        }
        self.pause();
        // Whatever the pointer was dragging is abandoned: the project it
        // edits is about to change under it.
        self.echo = None;
        self.gesture = super::Gesture::None;
        self.autosave.stop();
        let session = self.session.take();
        self.lent = Some(session.as_ref().map(|session| session.path().to_owned()));
        self.blocked = BlockedBy::Bridge;
        Ok(session)
    }

    /// Takes the session back from the agent's call.
    pub fn return_session(&mut self, returned: Option<Session>) {
        let Some(lent_path) = self.lent.take() else {
            return;
        };
        self.blocked = BlockedBy::Nothing;
        match (returned, lent_path) {
            // The usual case: the same project, edited. Show what changed.
            (Some(session), Some(path)) if projects::same_path(session.path(), &path) => {
                self.session = Some(session);
                self.dirty = true;
                self.after_change();
            }
            // A different project, or one where there was none: the call
            // opened or created it. Save it, let go of it, and open it here.
            (Some(mut session), previous) => {
                let path = session.path().to_owned();
                if let Err(error) = session.save(None, None, None) {
                    self.notify(&format!("Could not save {path}: {error}"), true);
                }
                drop(session);
                if previous.is_some() {
                    self.close_project();
                }
                self.open_recent(&path);
            }
            // The call closed the project.
            (None, Some(_)) => self.close_project(),
            (None, None) => {}
        }
    }
}
