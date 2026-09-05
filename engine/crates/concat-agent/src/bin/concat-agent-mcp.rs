// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The MCP server binary: an `AgentEngine` behind `rmcp`'s stdio transport.
//! No window, no GPU - just this process and whatever it's told to open.

use std::sync::Arc;

use concat_agent::AgentEngine;
use concat_agent::mcp::ConcatAgentServer;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Arc::new(AgentEngine::new()?);
    let server = ConcatAgentServer::new(engine);

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
