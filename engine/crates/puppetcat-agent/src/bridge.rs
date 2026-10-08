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
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::AgentEngine;
use crate::mcp::PuppetCatAgentServer;

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
#[cfg(unix)]
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
            let server = PuppetCatAgentServer::hosted(hosted.clone());
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

/// Starts serving the agent's tools on `socket`. Not available off Unix, where
/// the bridge's local sockets do not exist: the window works without it and the
/// stand-alone server does its own work, as it always did.
#[cfg(not(unix))]
pub fn host(
    _socket: &Path,
    _engine: Arc<AgentEngine>,
    _provider: Arc<dyn SessionProvider>,
) -> std::io::Result<WindowBridge> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the agent bridge needs Unix sockets",
    ))
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
    #[cfg(not(unix))]
    pub async fn forward(&self, _request: &CallToolRequestParams) -> Forwarded {
        Forwarded::NoWindow
    }

    /// Sends `request` to the window, if one is running.
    #[cfg(unix)]
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
            Ok(mut response) => {
                // The window link negotiates an older protocol, so its reply
                // arrives with `resultType` stripped. Restore it: the outer
                // peer may have negotiated 2026-07-28, which requires it, and
                // rmcp only strips it for legacy peers.
                if let rmcp::model::CallToolResponse::Complete(result) = &mut response {
                    result
                        .result_type
                        .get_or_insert(rmcp::model::ResultType::COMPLETE);
                }
                Forwarded::Done(response)
            }
            Err(error) => {
                *slot = None;
                Forwarded::Failed(error.to_string())
            }
        }
    }
}

#[cfg(all(test, unix))]
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
        let server = PuppetCatAgentServer::forwarding(
            local.clone(),
            Arc::new(Forwarder::at(socket.clone())),
        );
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

    // ── protocol and parity between the stand-alone and window servers ──

    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

    const LEGACY: &str = "2024-11-05";
    const CURRENT: &str = "2026-07-28";

    /// A bare JSON-RPC peer: what a client such as Claude Code puts on the
    /// wire, without rmcp's client deciding what to send or tolerate.
    struct Raw {
        reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
        writer: Box<dyn AsyncWrite + Unpin + Send>,
        next_id: u64,
        /// Speak 2026-07-28 the way its clients do: no `initialize`, the
        /// version and identity carried in each request's `_meta`.
        inline: bool,
    }

    impl Raw {
        fn over<S>(stream: S) -> Self
        where
            S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        {
            let (read, write) = tokio::io::split(stream);
            Self {
                reader: BufReader::new(Box::new(read)),
                writer: Box::new(write),
                next_id: 1,
                inline: false,
            }
        }

        async fn send(&mut self, message: Value) {
            let mut line = message.to_string();
            line.push('\n');
            self.writer
                .write_all(line.as_bytes())
                .await
                .expect("writes");
        }

        async fn request(&mut self, method: &str, params: Value) -> Value {
            let id = self.next_id;
            self.next_id += 1;
            let mut params = params;
            if self.inline {
                params["_meta"] = json!({
                    "io.modelcontextprotocol/protocolVersion": CURRENT,
                    "io.modelcontextprotocol/clientInfo": {"name": "parity", "version": "0"},
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "io.modelcontextprotocol/logLevel": "info",
                });
            }
            self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
                .await;
            loop {
                let mut line = String::new();
                let read = self.reader.read_line(&mut line).await.expect("reads");
                assert!(read > 0, "the server closed on {method}");
                let message: Value = serde_json::from_str(&line).expect("json-rpc");
                if message.get("id") == Some(&json!(id)) {
                    return message;
                }
            }
        }

        /// Initializes as a client speaking `version`; returns the version
        /// the server settled on. (The server settles on 2024-11-05 whatever
        /// is asked; 2026-07-28 clients skip this and use `inline`.)
        async fn initialize(&mut self, version: &str) -> String {
            let reply = self
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": version,
                        "capabilities": {},
                        "clientInfo": {"name": "parity", "version": "0"},
                    }),
                )
                .await;
            self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await;
            reply["result"]["protocolVersion"]
                .as_str()
                .unwrap_or_else(|| panic!("initialize failed: {reply}"))
                .to_owned()
        }

        async fn call(&mut self, name: &str, arguments: Value) -> Value {
            self.request("tools/call", json!({"name": name, "arguments": arguments}))
                .await
        }
    }

    /// One way a client can reach the agent's tools.
    #[derive(Clone, Copy, Debug)]
    enum Path_ {
        /// The stand-alone server working on its own project.
        StandAlone,
        /// The stand-alone server, with a window running: it forwards.
        Forwarded,
        /// Straight to the window's server.
        Window,
    }

    const PATHS: [Path_; 3] = [Path_::StandAlone, Path_::Forwarded, Path_::Window];

    /// Everything one path needs to stay up while a test talks to it.
    struct Rig {
        raw: Raw,
        root: PathBuf,
        socket: Option<PathBuf>,
        _bridge: Option<WindowBridge>,
        window: Option<Arc<FakeWindow>>,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
            if let Some(socket) = &self.socket {
                let _ = std::fs::remove_file(socket);
            }
        }
    }

    async fn rig(path: Path_, tag: &str, version: &str) -> Rig {
        let root = scratch(tag);
        let socket = std::env::temp_dir().join(format!("pcp-{tag}-{}.sock", std::process::id()));
        let engine = |name: &str| {
            Arc::new(
                AgentEngine::for_window(AppDirs::under(&root.join(name)), Exporter::new())
                    .expect("engine"),
            )
        };
        let mut bridge = None;
        let mut window = None;
        let server = match path {
            Path_::StandAlone => PuppetCatAgentServer::new(engine("local")),
            Path_::Forwarded | Path_::Window => {
                let lent = window_with_project(&root);
                bridge = Some(host(&socket, engine("app"), lent.clone()).expect("hosts"));
                window = Some(lent);
                PuppetCatAgentServer::forwarding(
                    engine("local"),
                    Arc::new(Forwarder::at(socket.clone())),
                )
            }
        };
        let mut raw = match path {
            Path_::Window => Raw::over(UnixStream::connect(&socket).await.expect("connects")),
            _ => {
                let (near, far) = tokio::io::duplex(1 << 20);
                tokio::spawn(async move {
                    if let Ok(service) = server.serve(far).await {
                        let _ = service.waiting().await;
                    }
                });
                Raw::over(near)
            }
        };
        if version == CURRENT {
            raw.inline = true;
        } else {
            raw.initialize(version).await;
        }
        if matches!(path, Path_::StandAlone) {
            let made = raw
                .call(
                    "create_project",
                    json!({
                        "location": root.join("projects").to_string_lossy(),
                        "name": "bridge",
                        "width": 1920,
                        "height": 1080,
                        "rateNum": 30,
                        "rateDen": 1,
                    }),
                )
                .await;
            assert!(made.get("error").is_none(), "create_project: {made}");
            assert_ne!(
                made["result"]["isError"],
                json!(true),
                "create_project: {made}"
            );
        }
        Rig {
            raw,
            root,
            socket: Some(socket),
            _bridge: bridge,
            window,
        }
    }

    /// The JSON a value has, with every leaf reduced to its type, so two
    /// answers can be compared for shape where the contents (ids, paths,
    /// timestamps) legitimately differ.
    fn shape(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), shape(v))).collect())
            }
            Value::Array(items) => Value::Array(items.first().map(shape).into_iter().collect()),
            Value::String(_) => json!("string"),
            Value::Number(_) => json!("number"),
            Value::Bool(_) => json!("bool"),
            Value::Null => Value::Null,
        }
    }

    /// The structured part of a tool result: its text content parsed as JSON
    /// when it is, else the raw text.
    fn payload(reply: &Value) -> Value {
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        serde_json::from_str(text).unwrap_or_else(|_| json!(text))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_forwarded_result_carries_result_type_for_a_current_client() {
        let mut rig = rig(Path_::Forwarded, "rt-fwd", CURRENT).await;
        let reply = rig.raw.call("get_state", json!({})).await;
        assert_eq!(
            reply["result"]["resultType"],
            json!("complete"),
            "a 2026-07-28 client rejects a result without it: {reply}"
        );
        assert!(
            rig.window
                .as_ref()
                .unwrap()
                .acquired
                .lock()
                .unwrap()
                .to_owned()
                > 0,
            "the call really went through the window"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_forwarded_result_leaves_result_type_out_for_a_legacy_client() {
        let mut rig = rig(Path_::Forwarded, "rt-legacy", LEGACY).await;
        let reply = rig.raw.call("get_state", json!({})).await;
        assert!(
            reply["result"].get("resultType").is_none(),
            "a legacy client keeps the old wire shape: {reply}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_path_agrees_on_result_type_for_every_protocol_version() {
        for version in [LEGACY, CURRENT] {
            for path in PATHS {
                let mut rig = rig(path, &format!("rtv-{path:?}-{version}"), version).await;
                // A success and a tool error: both are tool results.
                for (name, arguments) in [
                    ("get_state", json!({})),
                    ("remove_marker", json!({"markerId": "no-such-marker"})),
                ] {
                    let reply = rig.raw.call(name, arguments).await;
                    let result = reply
                        .get("result")
                        .unwrap_or_else(|| panic!("{path:?}/{version}/{name}: {reply}"));
                    let expected = (version == CURRENT).then(|| json!("complete"));
                    assert_eq!(
                        result.get("resultType").cloned(),
                        expected,
                        "{path:?} / {version} / {name}: {reply}"
                    );
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_stand_alone_and_window_servers_offer_identical_tools() {
        let mut lists = Vec::new();
        for path in PATHS {
            let mut rig = rig(path, &format!("tools-{path:?}"), CURRENT).await;
            let reply = rig.raw.request("tools/list", json!({})).await;
            let mut tools = reply["result"]["tools"]
                .as_array()
                .unwrap_or_else(|| panic!("{path:?}: {reply}"))
                .clone();
            tools.sort_by_key(|tool| tool["name"].as_str().unwrap_or_default().to_owned());
            assert!(!tools.is_empty());
            lists.push((path, tools));
        }
        let (first_path, first) = &lists[0];
        for (path, tools) in &lists[1..] {
            let names = |list: &Vec<Value>| -> Vec<String> {
                list.iter()
                    .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
                    .collect()
            };
            assert_eq!(
                names(tools),
                names(first),
                "{path:?} and {first_path:?} expose different tool names"
            );
            // Descriptions and input schemas are what an agent reads.
            assert_eq!(
                tools, first,
                "{path:?} and {first_path:?} describe a tool differently"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_stand_alone_and_window_servers_describe_themselves_alike() {
        let mut infos = Vec::new();
        for path in PATHS {
            let tag = format!("info-{path:?}");
            let root = scratch(&tag);
            let socket =
                std::env::temp_dir().join(format!("pcp-{tag}-{}.sock", std::process::id()));
            let engine = Arc::new(
                AgentEngine::for_window(AppDirs::under(&root.join("app")), Exporter::new())
                    .expect("engine"),
            );
            let window = window_with_project(&root);
            let _bridge = host(&socket, engine.clone(), window).expect("hosts");
            let mut raw = match path {
                Path_::Window => Raw::over(UnixStream::connect(&socket).await.expect("connects")),
                _ => {
                    let server = PuppetCatAgentServer::new(engine);
                    let (near, far) = tokio::io::duplex(1 << 16);
                    tokio::spawn(async move {
                        if let Ok(service) = server.serve(far).await {
                            let _ = service.waiting().await;
                        }
                    });
                    Raw::over(near)
                }
            };
            let reply = raw
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": LEGACY,
                        "capabilities": {},
                        "clientInfo": {"name": "parity", "version": "0"},
                    }),
                )
                .await;
            let result = &reply["result"];
            infos.push((
                path,
                json!({
                    "protocolVersion": result["protocolVersion"],
                    "capabilities": result["capabilities"],
                    "instructions": result["instructions"],
                    "serverInfo": result["serverInfo"],
                }),
            ));
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&socket);
        }
        for (path, info) in &infos[1..] {
            assert_eq!(info, &infos[0].1, "{path:?} introduces itself differently");
        }
    }

    /// The same edit, made through each path, must answer in the same shape:
    /// an agent should not be able to tell which one it reached.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_calls_answer_in_the_same_shape_on_every_path() {
        let script: [(&str, Value); 5] = [
            ("get_state", json!({})),
            ("add_marker", json!({"time": 2.5, "name": "parity"})),
            ("get_state", json!({})),
            ("get_timeline_summary", json!({})),
            // Errors are part of the contract too.
            ("remove_marker", json!({"markerId": "no-such-marker"})),
        ];
        let mut transcripts: Vec<(Path_, Vec<Value>)> = Vec::new();
        for path in PATHS {
            let mut rig = rig(path, &format!("shape-{path:?}"), CURRENT).await;
            let mut seen = Vec::new();
            for (name, arguments) in &script {
                let reply = rig.raw.call(name, arguments.clone()).await;
                assert!(reply.get("error").is_none(), "{path:?}/{name}: {reply}");
                seen.push(json!({
                    "tool": name,
                    "isError": reply["result"]["isError"].as_bool().unwrap_or(false),
                    "contentKinds": reply["result"]["content"]
                        .as_array()
                        .map(|items| items.iter().map(|i| i["type"].clone()).collect::<Vec<_>>()),
                    "payload": shape(&payload(&reply)),
                }));
            }
            transcripts.push((path, seen));
        }
        let (first_path, first) = &transcripts[0];
        for (path, seen) in &transcripts[1..] {
            for (a, b) in first.iter().zip(seen) {
                assert_eq!(
                    a, b,
                    "{first_path:?} and {path:?} answered {} differently",
                    a["tool"]
                );
            }
        }
    }

    /// A tool that exists on one path but whose call is refused as unknown on
    /// another would split the two. Call every tool by name on each path with
    /// empty arguments: whatever the answer, the method must be known.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_listed_tool_is_callable_on_every_path() {
        let mut names: Vec<String> = Vec::new();
        {
            let mut rig = rig(Path_::StandAlone, "names", CURRENT).await;
            let reply = rig.raw.request("tools/list", json!({})).await;
            for tool in reply["result"]["tools"].as_array().expect("tools") {
                names.push(tool["name"].as_str().expect("name").to_owned());
            }
        }
        // Tools with side effects outside the project, or that never return
        // quickly, are left out; they are covered by the schema comparison.
        let skip = [
            "export_project",
            "export_batch",
            "create_project",
            "open_project",
            "close_project",
            "render_contact_sheet",
        ];
        for path in [Path_::Forwarded, Path_::Window] {
            let mut rig = rig(path, &format!("callable-{path:?}"), CURRENT).await;
            for name in names.iter().filter(|n| !skip.contains(&n.as_str())) {
                let reply = rig.raw.call(name, json!({})).await;
                let unknown = reply["error"]["message"].as_str().is_some_and(|m| {
                    m.to_lowercase().contains("not found") || m.contains("unknown tool")
                });
                assert!(!unknown, "{path:?} does not know tool {name}: {reply}");
            }
        }
    }
}
