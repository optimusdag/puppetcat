// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The MCP server binary: an `AgentEngine` behind `rmcp`'s stdio transport.
//! No window, no GPU - just this process and whatever it's told to open.

use std::sync::Arc;

use puppetcat_agent::AgentEngine;
use puppetcat_agent::mcp::ConcatAgentServer;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Arc::new(AgentEngine::new()?);
    // A running PuppetCat window takes the tool calls itself, so edits show up
    // there live; with none running, they are handled here as ever.
    let forwarder = Arc::new(puppetcat_agent::bridge::Forwarder::new(&engine.dirs()));
    let server = ConcatAgentServer::forwarding(engine, forwarder);

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
