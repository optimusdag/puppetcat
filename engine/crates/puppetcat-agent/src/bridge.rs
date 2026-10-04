// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Routing the agent's tools through a running window.
//!
//! Left alone, the window and `puppetcat-agent-mcp` are two processes that
//! each open the project themselves, take turns by file lock, and never see
//! each other's edits live. This module lets them be one: the window hosts
//! the very same MCP server on a local socket, with an engine that borrows
//! the window's own session for the length of each tool call, and the
//! stand-alone server forwards a call there whenever a window is running.
//! Edits then appear in the window as they happen, share its undo history and
//! draw with its renderer.
//!
//! Three small pieces:
//!
//! - [`SessionProvider`] - what the window implements: hand the session over
//!   for a call, take it back after.
//! - [`host`] - the window side: listen on a socket, serve each connection.
//! - [`Forwarder`] - the stand-alone side: a cached connection to that
//!   socket, tried on each call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use puppetcat_host::{AppDirs, Session};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse};
use rmcp::service::{RoleClient, RunningService};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::AgentEngine;
use crate::mcp::ConcatAgentServer;

/// Where the window listens: beside the recents list, so it is per user and
/// per `AppDirs`, like everything else the window keeps.
pub fn socket_path(dirs: &AppDirs) -> PathBuf {
    dirs.config.join("window-mcp.sock")
}

/// How the window lends its session to the agent's tools.
///
/// Both calls block, and are made off the window's event loop; an
/// implementation hands the work to that loop and waits for it.
pub trait SessionProvider: Send + Sync + 'static {
    /// Takes the window's session for one tool call: the window stops
    /// accepting input until [`SessionProvider::release`]. `Ok(None)` when the
    /// window has no project open. An error means the window cannot lend it
    /// (already lent, or closing), and the call is refused.
    fn acquire(&self) -> Result<Option<Session>, String>;

    /// Takes the session back after the call. What comes back is the session
    /// that went out, or a different one if the call opened or created a
    /// project (the window then switches to it), or `None` if the call closed
    /// it.
    fn release(&self, session: Option<Session>);
}

/// What a window-hosted server shares between its connections: the engine,
/// the way to borrow the window's session, and a gate so only one call has
/// the session at a time.
#[derive(Clone)]
pub struct Hosted {
    pub(crate) engine: Arc<AgentEngine>,
    pub(crate) provider: Arc<dyn SessionProvider>,
    pub(crate) gate: Arc<Mutex<()>>,
}

impl Hosted {
    /// Runs `call` with the window's session lent to the engine, and hands it
    /// back afterwards - however the call ends. The whole thing runs as its
    /// own task, so a client that disconnects mid-call cannot leave the
    /// window waiting on a session that never comes back.
    pub(crate) async fn leased<F, T>(&self, call: F) -> Result<T, String>
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let hosted = self.clone();
        tokio::spawn(async move {
            let _one_at_a_time = hosted.gate.lock().await;
            let provider = Arc::clone(&hosted.provider);
            let session = tokio::task::spawn_blocking(move || provider.acquire())
                .await
                .map_err(|error| error.to_string())??;
            hosted.engine.lend_in(session);
            let result = call.await;
            let back = hosted.engine.lend_out();
            let provider = Arc::clone(&hosted.provider);
            let _ = tokio::task::spawn_blocking(move || provider.release(back)).await;
            Ok(result)
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

/// A listening window-side bridge. Dropping it stops listening and removes
/// the socket, so a window that quits is not mistaken for one that is up.
pub struct WindowBridge {
    path: PathBuf,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for WindowBridge {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Starts serving the agent's tools on `socket`, working on whatever session
/// `provider` lends. Replaces a stale socket left by a window that crashed.
/// The returned handle keeps it running.
pub fn host(
    socket: &Path,
    engine: Arc<AgentEngine>,
    provider: Arc<dyn SessionProvider>,
) -> std::io::Result<WindowBridge> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("puppetcat-mcp-bridge")
        .enable_all()
        .build()?;
    // A socket file left behind is only stale if nobody answers on it.
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "another window already hosts the agent's tools",
            ));
        }
        let _ = std::fs::remove_file(socket);
    }
    let listener = {
        let _enter = runtime.enter();
        UnixListener::bind(socket)?
    };
    let hosted = Hosted {
        engine: Arc::clone(&engine),
        provider,
        gate: Arc::new(Mutex::new(())),
    };
    runtime.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let server = ConcatAgentServer::hosted(hosted.clone());
            tokio::spawn(async move {
                let (read, write) = stream.into_split();
                if let Ok(service) = server.serve((read, write)).await {
                    let _ = service.waiting().await;
                }
            });
        }
    });
    Ok(WindowBridge {
        path: socket.to_owned(),
        runtime: Some(runtime),
    })
}

/// The stand-alone server's way to a window: a connection to its socket,
/// opened when first needed and kept for the calls that follow.
pub struct Forwarder {
    socket: PathBuf,
    client: Mutex<Option<RunningService<RoleClient, ()>>>,
}

/// What forwarding a call came to.
pub enum Forwarded {
    /// No window answered: run the call here.
    NoWindow,
    /// The window ran it.
    Done(CallToolResponse),
    /// The window was reached but the call did not complete. Not retried
    /// here: it may have run.
    Failed(String),
}

impl Forwarder {
    /// A forwarder for the window of `dirs`.
    pub fn new(dirs: &AppDirs) -> Self {
        Self {
            socket: socket_path(dirs),
            client: Mutex::new(None),
        }
    }

    /// A forwarder for a window listening on `socket`.
    pub fn at(socket: PathBuf) -> Self {
        Self {
            socket,
            client: Mutex::new(None),
        }
    }

    /// Sends `request` to the window, if one is running.
    pub async fn forward(&self, request: &CallToolRequestParams) -> Forwarded {
        let mut slot = self.client.lock().await;
        if slot.as_ref().is_some_and(|client| client.is_closed()) {
            *slot = None;
        }
        if slot.is_none() {
            let Ok(stream) = UnixStream::connect(&self.socket).await else {
                return Forwarded::NoWindow;
            };
            let (read, write) = stream.into_split();
            match ().serve((read, write)).await {
                Ok(client) => *slot = Some(client),
                Err(_) => return Forwarded::NoWindow,
            }
        }
        let client = slot.as_ref().expect("connected above");
        match client.call_tool_once(request.clone()).await {
            Ok(response) => Forwarded::Done(response),
            Err(error) => {
                *slot = None;
                Forwarded::Failed(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use puppetcat_host::export::Exporter;
    use rmcp::model::CallToolRequestParams;

    use super::*;

    /// A stand-in for the window: owns a session and lends it out.
    struct FakeWindow {
        session: StdMutex<Option<Session>>,
        acquired: StdMutex<u32>,
        released: StdMutex<u32>,
    }

    impl SessionProvider for FakeWindow {
        fn acquire(&self) -> Result<Option<Session>, String> {
            *self.acquired.lock().unwrap() += 1;
            Ok(self.session.lock().unwrap().take())
        }
        fn release(&self, session: Option<Session>) {
            *self.released.lock().unwrap() += 1;
            *self.session.lock().unwrap() = session;
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pc-bridge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn call(name: &str, arguments: serde_json::Value) -> CallToolRequestParams {
        serde_json::from_value(serde_json::json!({ "name": name, "arguments": arguments }))
            .expect("a tool call")
    }

    fn text_of(response: &CallToolResponse) -> String {
        match response {
            CallToolResponse::Complete(result) => {
                serde_json::to_string(result).expect("serialises")
            }
            other => format!("{other:?}"),
        }
    }

    /// A project on disk and a session open on it, as the window would hold.
    fn window_with_project(root: &Path) -> Arc<FakeWindow> {
        let info = puppetcat_host::projects::create(
            &root.join("projects").to_string_lossy(),
            "bridge",
            1920,
            1080,
            30,
            1,
        )
        .expect("creates");
        let session = Session::open_info(&info).expect("opens");
        Arc::new(FakeWindow {
            session: StdMutex::new(Some(session)),
            acquired: StdMutex::new(0),
            released: StdMutex::new(0),
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_edits_the_windows_own_session_and_it_is_handed_back() {
        let root = scratch("host");
        let window = window_with_project(&root);
        let engine = Arc::new(
            AgentEngine::for_window(AppDirs::under(&root.join("app")), Exporter::new())
                .expect("engine"),
        );
        let socket = std::env::temp_dir().join(format!("pcb-{}.sock", std::process::id()));
        let _bridge = host(&socket, Arc::clone(&engine), window.clone()).expect("hosts");

        let client = Forwarder::at(socket.clone());
        let added = client
            .forward(&call(
                "add_marker",
                serde_json::json!({"time": 4.0, "name": "from the agent"}),
            ))
            .await;
        assert!(matches!(added, Forwarded::Done(_)), "the window ran it");

        // The edit is in the window's session, which is back with the window.
        let back = window.session.lock().unwrap().take().expect("handed back");
        let markers = &back.project().active().markers;
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].name, "from the agent");
        *window.session.lock().unwrap() = Some(back);
        assert_eq!(*window.acquired.lock().unwrap(), 1);
        assert_eq!(*window.released.lock().unwrap(), 1);
        assert!(
            !engine.has_project(),
            "the engine keeps nothing between calls"
        );

        // And an undo goes through the same history.
        let undone = client.forward(&call("undo", serde_json::json!({}))).await;
        assert!(matches!(undone, Forwarded::Done(_)));
        let back = window.session.lock().unwrap().take().expect("handed back");
        assert!(back.project().active().markers.is_empty());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&socket);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_stand_alone_server_goes_to_the_window_when_there_is_one_and_works_alone_otherwise()
    {
        let root = scratch("fwd");
        let window = window_with_project(&root);
        let hosted_engine = Arc::new(
            AgentEngine::for_window(AppDirs::under(&root.join("app")), Exporter::new())
                .expect("engine"),
        );
        let socket = std::env::temp_dir().join(format!("pcf-{}.sock", std::process::id()));

        // No window yet: the call is run by the stand-alone engine, which has
        // no project, and says so.
        let local = Arc::new(
            AgentEngine::for_window(AppDirs::under(&root.join("local")), Exporter::new())
                .expect("engine"),
        );
        let forwarder = Forwarder::at(socket.clone());
        assert!(matches!(
            forwarder
                .forward(&call("get_state", serde_json::json!({})))
                .await,
            Forwarded::NoWindow
        ));
        assert!(local.get_state().is_err(), "no project of its own");

        // A window comes up: the same forwarder now reaches it.
        let _bridge = host(&socket, hosted_engine, window.clone()).expect("hosts");
        let state = forwarder
            .forward(&call("get_state", serde_json::json!({})))
            .await;
        let Forwarded::Done(response) = state else {
            panic!("the window should have answered");
        };
        assert!(
            text_of(&response).contains("bridge"),
            "it is the window's project"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&socket);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stand_alone_server_hands_a_clients_calls_to_the_window() {
        let root = scratch("e2e");
        let window = window_with_project(&root);
        let hosted_engine = Arc::new(
            AgentEngine::for_window(AppDirs::under(&root.join("app")), Exporter::new())
                .expect("engine"),
        );
        let socket = std::env::temp_dir().join(format!("pce-{}.sock", std::process::id()));
        let _bridge = host(&socket, hosted_engine, window.clone()).expect("hosts");

        // The stand-alone server, as the MCP binary builds it, and a client
        // speaking real MCP to it.
        let local = Arc::new(
            AgentEngine::for_window(AppDirs::under(&root.join("local")), Exporter::new())
                .expect("engine"),
        );
        let server =
            ConcatAgentServer::forwarding(local.clone(), Arc::new(Forwarder::at(socket.clone())));
        let (near, far) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            if let Ok(service) = server.serve(far).await {
                let _ = service.waiting().await;
            }
        });
        let client = ().serve(near).await.expect("connects");
        let response = client
            .call_tool_once(call(
                "add_marker",
                serde_json::json!({"time": 2.5, "name": "via the relay"}),
            ))
            .await
            .expect("the call completes");
        assert!(matches!(response, CallToolResponse::Complete(_)));

        assert!(
            !local.has_project(),
            "the stand-alone engine did nothing itself"
        );
        let back = window.session.lock().unwrap().take().expect("handed back");
        assert_eq!(back.project().active().markers[0].name, "via the relay");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&socket);
    }
}
