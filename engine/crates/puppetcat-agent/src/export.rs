// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Starting an export and polling it, the way a thin agent tool needs to:
//! one call to start, repeated calls to check in on. `puppetcat_host::export`
//! itself is a blocking call plus a progress callback - this is the seam
//! that runs it on its own thread and remembers where it got to.

use std::sync::{Arc, Mutex};

use puppetcat_host::export::{ExportClip, ExportSpec, Exporter, Progress, request, run};
use puppetcat_host::Session;
use serde::Serialize;

/// A snapshot of one export's progress, for `get_export_status`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStatus {
    /// Whether a render is in flight right now.
    pub running: bool,
    /// What the exporter is doing: "video", "audio", "mux". `None` before
    /// the first progress report arrives.
    pub stage: Option<&'static str>,
    /// Frames done in the current stage.
    pub frame: i64,
    /// Frames total in the current stage.
    pub total: i64,
    /// Set once the render fails; `output` stays `None` when this is `Some`.
    pub error: Option<String>,
    /// The path written, once the render finishes successfully.
    pub output: Option<String>,
}

/// The one export this server can run at a time.
#[derive(Default)]
pub struct ExportJob {
    exporter: Exporter,
    status: Arc<Mutex<ExportStatus>>,
}

impl ExportJob {
    /// An idle job: nothing running, no status yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last known progress. Never blocks on the render thread.
    pub fn status(&self) -> ExportStatus {
        self.status.lock().expect("export status mutex").clone()
    }

    /// Starts rendering `session` to `spec.output` on its own thread. Errs
    /// immediately if an export is already running; the render itself
    /// reports back only through `status()`.
    pub fn start(
        &self,
        session: &Session,
        spec: ExportSpec,
        titles: Vec<ExportClip>,
    ) -> Result<(), String> {
        let job = self.exporter.begin()?;
        let export_request = request(session, &spec, titles);
        let status = self.status.clone();
        *status.lock().expect("export status mutex") = ExportStatus {
            running: true,
            ..ExportStatus::default()
        };

        std::thread::spawn(move || {
            let cancel = job.cancel_handle();
            let result = run(&export_request, &cancel, |progress: Progress| {
                let mut state = status.lock().expect("export status mutex");
                state.stage = Some(progress.stage);
                state.frame = progress.frame;
                state.total = progress.total;
            });
            let mut state = status.lock().expect("export status mutex");
            state.running = false;
            match result {
                Ok(output) => state.output = Some(output),
                Err(error) => state.error = Some(error),
            }
            // `job` drops here, releasing the single export slot.
        });

        Ok(())
    }
}
