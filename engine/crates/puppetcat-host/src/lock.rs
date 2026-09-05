// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Who owns a project folder right now, across processes.
//!
//! Two independent slots live beside the project: `.puppetcat-window.lock`
//! for whichever `puppetcat` window has it open, `.puppetcat-agent.lock` for
//! whichever `puppetcat-agent` session has it open. Each is a plain OS
//! advisory lock on an otherwise-empty file - the operating system releases
//! it the instant its holding process exits, crashes or is killed, which is
//! the whole reason to build this on file locks rather than a heartbeat or a
//! PID check: there is no window in which a dead holder still looks alive.
//!
//! The two slots are independent on purpose. An agent must always be able to
//! take its slot, even while a window holds its own - there is no way to
//! force another live process to give up a lock it holds, so one shared lock
//! could never express "the agent takes priority over the window". Whether
//! an editor should currently accept input is a decision its caller makes by
//! combining what it holds with what [`SlotLock::is_held`] reports for the
//! other slot; this module only ever answers "is this one slot free".

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Which editor a lock file speaks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The `puppetcat` window.
    Window,
    /// A `puppetcat-agent` session - the MCP server, today.
    Agent,
}

impl Slot {
    fn file_name(self) -> &'static str {
        match self {
            Slot::Window => ".puppetcat-window.lock",
            Slot::Agent => ".puppetcat-agent.lock",
        }
    }
}

/// A held slot. Dropping it releases the lock, on a clean close and on a
/// crash alike, since both close the file descriptor the OS locked. The
/// file itself is never read back; it is kept only so the lock it holds
/// lives as long as this does.
pub struct SlotLock(#[allow(dead_code)] File);

impl SlotLock {
    /// Takes `slot` for the project at `root`, if nothing else already holds
    /// it. Never blocks: a slot already held elsewhere comes back as `None`
    /// rather than waiting, so a caller can poll this from an event loop.
    pub fn try_acquire(root: &Path, slot: Slot) -> Option<SlotLock> {
        let file = open(root, slot).ok()?;
        file.try_lock().ok().map(|()| SlotLock(file))
    }

    /// Whether `slot` is currently held by someone else, without taking it -
    /// acquires and immediately drops a probe lock, so this never leaves a
    /// lock behind and never blocks.
    pub fn is_held(root: &Path, slot: Slot) -> bool {
        Self::try_acquire(root, slot).is_none()
    }
}

fn open(root: &Path, slot: Slot) -> io::Result<File> {
    // Never truncated: the lock is the fact that this handle exists, not
    // anything written through it, and truncating a file someone else has
    // open would be a pointless write to a shared sentinel.
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(slot.file_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("puppetcat-lock-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_free_slot_can_be_taken_and_then_reports_held() {
        let dir = scratch("free");
        assert!(!SlotLock::is_held(&dir, Slot::Window));
        let guard = SlotLock::try_acquire(&dir, Slot::Window).expect("a free slot acquires");
        assert!(SlotLock::is_held(&dir, Slot::Window));
        drop(guard);
        assert!(
            !SlotLock::is_held(&dir, Slot::Window),
            "dropping releases it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_slot_refuses_a_second_holder() {
        let dir = scratch("held");
        let _first = SlotLock::try_acquire(&dir, Slot::Agent).expect("the first holder acquires");
        assert!(SlotLock::try_acquire(&dir, Slot::Agent).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_two_slots_are_independent() {
        let dir = scratch("independent");
        let _window = SlotLock::try_acquire(&dir, Slot::Window).expect("the window slot acquires");
        assert!(
            SlotLock::try_acquire(&dir, Slot::Agent).is_some(),
            "holding Window must not block Agent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
