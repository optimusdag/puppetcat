// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Lets an agent drive a full editing session with no window and no GPU:
//! open or create a project, edit it through the same [`puppetcat_project::Command`]
//! the window applies, render a still to see the result, export.
//!
//! `mcp` is the only module here that knows MCP exists. Everything else -
//! [`AgentEngine`], the snapshot renderer, the export job - would work the
//! same behind a CLI or an HTTP server; that boundary is deliberate, the
//! same way `puppetcat_host` stays ignorant of the window that happens to be
//! its one caller today.

pub mod analysis;
pub mod dto;
pub mod engine;
pub mod export;
pub mod mcp;
pub mod query;
pub mod snapshot;

pub use engine::AgentEngine;
