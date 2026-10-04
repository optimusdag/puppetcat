// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! This window as the home of the agent's tools.
//!
//! When the window starts it serves the same MCP tools the stand-alone
//! `puppetcat-agent-mcp` does, on a local socket (see `puppetcat_agent::bridge`).
//! An MCP session that finds the socket sends its calls here, so they work on
//! the project in this window - edits appear as they are made, undo covers
//! them, and they draw with this machine's GPU - instead of on a second copy
//! opened by a second process. With no window running, the stand-alone server
//! does the work itself, as it always has.
//!
//! The tools run off the event loop; each borrows the window's session from
//! it for the length of one call. The two functions below are that borrowing.

use std::sync::Arc;

use puppetcat_agent::AgentEngine;
use puppetcat_agent::bridge::{SessionProvider, WindowBridge};
use puppetcat_host::Session;

use crate::host::Shell;
use crate::studio::Studio;

/// Lends the window's session to the agent's tool calls.
struct WindowSessions;

/// Runs `work` on the event-loop thread with the window's state and waits for
/// it, publishing afterwards. An error if the window is gone.
fn on_event_loop<T: Send + 'static>(
    work: impl FnOnce(&mut Studio) -> T + Send + 'static,
) -> Result<T, String> {
    let (done, result) = std::sync::mpsc::channel();
    slint::invoke_from_event_loop(move || {
        Shell::with(|shell, app| {
            let value = work(&mut shell.studio.borrow_mut());
            shell.studio.borrow().publish(&app, &shell.models);
            let _ = done.send(value);
        });
    })
    .map_err(|error| error.to_string())?;
    result
        .recv()
        .map_err(|_| "the window is closing".to_owned())
}

impl SessionProvider for WindowSessions {
    fn acquire(&self) -> Result<Option<Session>, String> {
        on_event_loop(|studio| studio.lend_session())?
    }

    fn release(&self, session: Option<Session>) {
        let _ = on_event_loop(move |studio| studio.return_session(session));
    }
}

/// Starts serving the agent's tools from this window. `None` - with the
/// reason on stderr - when it cannot: another window already hosts them, or
/// the socket cannot be made. The window works the same without.
pub fn start(studio: &Studio) -> Option<WindowBridge> {
    let dirs = studio.host.dirs.clone();
    let engine = match AgentEngine::for_window(dirs.clone(), studio.host.exporter.clone()) {
        Ok(engine) => Arc::new(engine),
        Err(error) => {
            eprintln!("puppetcat: the agent bridge could not start: {error}");
            return None;
        }
    };
    let socket = puppetcat_agent::bridge::socket_path(&dirs);
    match puppetcat_agent::bridge::host(&socket, engine, Arc::new(WindowSessions)) {
        Ok(bridge) => Some(bridge),
        Err(error) => {
            eprintln!(
                "puppetcat: the agent bridge is not running ({}): {error}",
                socket.display()
            );
            None
        }
    }
}
