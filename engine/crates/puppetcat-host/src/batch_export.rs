// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! Starting an export, or a batch of them, and polling it: one call to start,
//! repeated calls to check in on. [`crate::export`] itself is a blocking call
//! plus a progress callback - this is the seam that runs it on its own thread
//! and remembers where it got to. The agent's export tools and the window's
//! batch sheet both run through it.

use std::sync::{Arc, Mutex};

use crate::Session;
use crate::export::{ExportClip, ExportRequest, ExportSpec, Exporter, Progress, request, run};
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
    batch: Arc<Mutex<BatchStatus>>,
}

/// One planned file in a batch export.
pub struct BatchItem {
    /// Label used in status and filenames.
    pub name: String,
    /// The file the item writes.
    pub output: String,
    /// The timeline stretch exported, in seconds.
    pub range: (f64, f64),
    /// Already satisfied (the collision policy said skip).
    pub skip: bool,
    /// The render request, built from the session at start time.
    pub request: ExportRequest,
}

/// Where one batch item stands.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchItemStatus {
    /// Position in the batch, from 1.
    pub index: usize,
    /// Item label.
    pub name: String,
    /// The file it writes.
    pub output: String,
    /// Start and end seconds.
    pub range: (f64, f64),
    /// `pending`, `running`, `done`, `skipped`, `failed` or `cancelled`.
    pub state: &'static str,
    /// Frames done in the current stage while running.
    pub frame: i64,
    /// Frames total in the current stage while running.
    pub total: i64,
    /// Why it failed.
    pub error: Option<String>,
}

/// The batch as a whole. Failed or cancelled items can be resumed by
/// repeating the batch with the `skip` collision policy: finished outputs
/// are left alone and only the rest render.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchStatus {
    /// Whether the batch is still rendering.
    pub running: bool,
    /// Per-item state, in order.
    pub items: Vec<BatchItemStatus>,
}

impl ExportJob {
    /// An idle job: nothing running, no status yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A job that shares `exporter`'s slot, so it and whoever else holds a
    /// clone of that exporter can never render at the same time.
    pub fn with_exporter(exporter: Exporter) -> Self {
        Self {
            exporter,
            ..Self::default()
        }
    }

    /// The last known progress. Never blocks on the render thread.
    pub fn status(&self) -> ExportStatus {
        self.status.lock().expect("export status mutex").clone()
    }

    /// The last known batch progress.
    pub fn batch_status(&self) -> BatchStatus {
        self.batch.lock().expect("batch status mutex").clone()
    }

    /// Asks the running export or batch to stop at the next frame.
    pub fn cancel(&self) {
        self.exporter.cancel();
    }

    /// Renders `items` one after another on one thread, holding the single
    /// export slot for the whole batch so nothing else can start mid-way.
    /// A failing item does not stop the rest; cancelling does.
    pub fn start_batch(&self, items: Vec<BatchItem>) -> Result<(), String> {
        let job = self.exporter.begin()?;
        let status = self.batch.clone();
        *status.lock().expect("batch status mutex") = BatchStatus {
            running: true,
            items: items
                .iter()
                .enumerate()
                .map(|(index, item)| BatchItemStatus {
                    index: index + 1,
                    name: item.name.clone(),
                    output: item.output.clone(),
                    range: item.range,
                    state: if item.skip { "skipped" } else { "pending" },
                    frame: 0,
                    total: 0,
                    error: None,
                })
                .collect(),
        };
        std::thread::spawn(move || {
            let cancel = job.cancel_handle();
            for (index, item) in items.into_iter().enumerate() {
                if item.skip {
                    continue;
                }
                let set = |update: &dyn Fn(&mut BatchItemStatus)| {
                    let mut state = status.lock().expect("batch status mutex");
                    update(&mut state.items[index]);
                };
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    set(&|entry| entry.state = "cancelled");
                    continue;
                }
                set(&|entry| entry.state = "running");
                let result = run(&item.request, &cancel, |progress: Progress| {
                    set(&|entry| {
                        entry.frame = progress.frame;
                        entry.total = progress.total;
                    });
                });
                match result {
                    Ok(_) => set(&|entry| entry.state = "done"),
                    Err(error) if cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                        let _ = error;
                        set(&|entry| entry.state = "cancelled");
                    }
                    Err(error) => set(&|entry| {
                        entry.state = "failed";
                        entry.error = Some(error.clone());
                    }),
                }
            }
            status.lock().expect("batch status mutex").running = false;
        });
        Ok(())
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
