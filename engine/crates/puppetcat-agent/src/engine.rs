// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The core wrapper an agent drives: one project open at a time, every edit
//! a `Command`, nothing here knowing MCP exists. The doctrine is
//! `puppetcat_host`'s own - no editing decision is made here, only session
//! lifecycle and the plumbing an agent needs that the window otherwise
//! provides for a human (seeing the result, waiting on an export).

use std::path::Path;
use std::sync::Mutex;

use puppetcat_core::frame::Frame;
use puppetcat_host::{Slot, SlotLock};
use puppetcat_media::ReaderPool;
use puppetcat_project::Command;
use puppetcat_project::model::ClipKind;

mod batch;
mod layouts;
mod lifecycle;
pub use batch::{BatchPlan, BatchRangeArg, BatchRequest};
pub use layouts::{ApplyLayoutReport, ClipSelector, LayoutValues};
pub use lifecycle::{MoveOutputReport, RelinkEntry, RelinkReport};

use crate::analysis::{self, AudioAnalysis, FeatureBucket};
use crate::export::{ExportJob, ExportStatus};
use crate::query::{self, TimelineSummary, ValidationReport};

/// What `apply_edit_plan` reports: enough to confirm a plan did what was
/// meant without re-reading the project.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanResult {
    /// Whether the plan changed the live project.
    pub committed: bool,
    /// Echo of the request flag.
    pub dry_run: bool,
    /// Revision the plan was staged from.
    pub old_revision: u64,
    /// Revision after the call; unchanged for a dry run or no-op.
    pub new_revision: u64,
    /// One entry per operation, in order: the id it minted, if any.
    pub created_ids: Vec<Option<String>>,
    /// Client reference name to the id it resolved to.
    pub refs: std::collections::BTreeMap<String, String>,
    /// Net clips added to the active timeline.
    pub clips_delta: i64,
    /// Net markers added to the active timeline.
    pub markers_delta: i64,
    /// Net media items added to the bin.
    pub media_delta: i64,
    /// Active timeline length before, seconds.
    pub duration_before: f64,
    /// Active timeline length after, seconds.
    pub duration_after: f64,
    /// Validation of the staged result, when requested.
    pub validation: Option<ValidationReport>,
}

/// Replaces every string equal to `$name` with the id that reference
/// resolved to. An unknown name is an error rather than a literal, so a typo
/// cannot silently address the wrong object.
fn substitute_refs(
    value: &mut serde_json::Value,
    refs: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(text) => {
            if let Some(name) = text.strip_prefix('$') {
                *text = refs
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("unknown reference ${name}; refs must be defined by an earlier operation that mints an id"))?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .try_for_each(|item| substitute_refs(item, refs)),
        serde_json::Value::Object(map) => map
            .values_mut()
            .try_for_each(|item| substitute_refs(item, refs)),
        _ => Ok(()),
    }
}

/// One diagnostic from [`AgentEngine::health_check`]. `code` is stable and
/// machine-readable; `remediation` says what to do when `ok` is false.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheck {
    /// What was checked.
    pub name: &'static str,
    /// Stable identifier of the outcome, e.g. `config_dir_unwritable`.
    pub code: &'static str,
    /// Whether the check passed.
    pub ok: bool,
    /// Human-readable detail.
    pub message: String,
    /// What to do about a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<&'static str>,
}

/// The result of [`AgentEngine::health_check`].
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    /// True when every check passed.
    pub healthy: bool,
    /// Per-check results.
    pub checks: Vec<HealthCheck>,
}

fn writable(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let probe = dir.join(format!(".puppetcat-health-{}", std::process::id()));
    std::fs::write(&probe, b"ok").map_err(|error| error.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn shape(project: &puppetcat_project::Project) -> (usize, usize, usize, f64) {
    let timeline = project.active();
    (
        timeline.clips.len(),
        timeline.markers.len(),
        project.media.len(),
        timeline.duration(),
    )
}

/// A session plus the cross-process claim that makes it safe to edit: for as
/// long as this exists, no `puppetcat` window will accept input on the same
/// project (see `puppetcat_host::lock`). Bundled with the session, not held
/// separately, so replacing or clearing one always releases the other in the
/// same step - an agent never holds the lock for a project it has stopped
/// editing.
struct OpenProject {
    session: puppetcat_host::Session,
    _lock: SlotLock,
}

impl std::ops::Deref for OpenProject {
    type Target = puppetcat_host::Session;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl std::ops::DerefMut for OpenProject {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

/// One open project's session, plus what an agent needs around it that a
/// human gets from the window: a place to render a still, and the one
/// export slot.
pub struct AgentEngine {
    dirs: puppetcat_host::AppDirs,
    pool: ReaderPool,
    titles: puppetcat_host::Titles,
    open: Mutex<Option<OpenProject>>,
    export: ExportJob,
}

impl AgentEngine {
    /// Locates this machine's app directories and starts with no project
    /// open.
    pub fn new() -> Result<Self, String> {
        Self::with_dirs(puppetcat_host::AppDirs::locate()?)
    }

    /// Starts with no project open, against `dirs` rather than this
    /// machine's real ones - the seam tests use so they never touch the
    /// real recents list (`puppetcat_host::AppDirs::under` builds a scratch
    /// one).
    fn with_dirs(dirs: puppetcat_host::AppDirs) -> Result<Self, String> {
        let titles = puppetcat_host::Titles::new(&dirs);
        Ok(Self {
            dirs,
            pool: ReaderPool::with_defaults(),
            titles,
            open: Mutex::new(None),
            export: ExportJob::new(),
        })
    }

    fn session_lock(&self) -> std::sync::MutexGuard<'_, Option<OpenProject>> {
        self.open.lock().expect("session mutex")
    }

    /// Creates a new project folder and opens it as the active session.
    pub fn create_project(
        &self,
        location: &str,
        name: &str,
        width: u32,
        height: u32,
        rate_num: i64,
        rate_den: i64,
    ) -> Result<puppetcat_host::ProjectInfo, String> {
        let info =
            puppetcat_host::projects::create(location, name, width, height, rate_num, rate_den)?;
        self.open_session(&info)?;
        Ok(info)
    }

    /// Opens an existing project folder as the active session, replacing
    /// whatever was open (unsaved edits are lost - call `save_project`
    /// first if they matter).
    pub fn open_project(&self, path: &str) -> Result<puppetcat_host::ProjectInfo, String> {
        let info = puppetcat_host::projects::open(path)?;
        self.open_session(&info)?;
        Ok(info)
    }

    fn open_session(&self, info: &puppetcat_host::ProjectInfo) -> Result<(), String> {
        let mut guard = self.session_lock();
        // Release this project's own lock first when it is the very path
        // being reopened, so reopening it never reads as some other agent
        // already holding it. A *different* open project is left alone
        // until the new one has actually opened, so a failed switch does
        // not lose it.
        if guard.as_ref().is_some_and(|open| {
            puppetcat_host::projects::same_path(open.session.path(), &info.path)
        }) {
            *guard = None;
        }
        let lock = SlotLock::try_acquire(Path::new(&info.path), Slot::Agent)
            .ok_or_else(|| "another agent session already has this project open".to_owned())?;
        let session = puppetcat_host::Session::open_info(info)?;
        let _ = puppetcat_host::projects::remember(&self.dirs.config, info);
        *guard = Some(OpenProject {
            session,
            _lock: lock,
        });
        Ok(())
    }

    /// Saves the active session, then closes it. A no-op if nothing is open.
    pub fn close_project(&self) -> Result<(), String> {
        let mut guard = self.session_lock();
        if let Some(session) = guard.as_mut() {
            session.save(None, None, None)?;
        }
        *guard = None;
        Ok(())
    }

    /// Saves the active session in place.
    pub fn save_project(&self) -> Result<(), String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        session.save(None, None, None)
    }

    /// Every project this machine has opened before, most recent first.
    pub fn list_recent_projects(&self) -> Vec<puppetcat_host::ProjectInfo> {
        puppetcat_host::projects::list(&self.dirs.config)
    }

    /// The active session's current state - the project, undo/redo flags,
    /// settings - without applying anything.
    pub fn get_state(&self) -> Result<puppetcat_host::EditorView, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        Ok(session.view())
    }

    /// The one place every edit - curated tool or raw escape hatch alike -
    /// funnels through, same as `Session::apply` already is for the window.
    pub fn apply(&self, command: Command) -> Result<puppetcat_host::EditorView, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        session.apply(command)
    }

    /// Runs a list of commands as one transaction against an expected
    /// revision. The plan is staged on a copy, validated, and only then
    /// committed as a single undo step; any failure - stale revision, a bad
    /// command, a newly introduced validation error - commits nothing.
    /// `dry_run` stages and reports without committing.
    pub fn apply_edit_plan(
        &self,
        expected_revision: u64,
        operations: Vec<serde_json::Value>,
        dry_run: bool,
        validate: bool,
    ) -> Result<EditPlanResult, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        let old_revision = session.revision();
        if expected_revision != old_revision {
            return Err(format!(
                "stale revision: expected {expected_revision}, project is at {old_revision}; \
                 re-read state and rebuild the plan"
            ));
        }
        let mut refs: std::collections::BTreeMap<String, String> = Default::default();
        let mut names: Vec<Option<String>> = Vec::with_capacity(operations.len());
        let count = operations.len();
        let mut pending = operations.into_iter();
        let staged = session.stage_with(count, |index, created| {
            // Record what the previous operation minted under its ref name.
            if let (Some(Some(name)), Some(Some(id))) = (names.last(), created.last()) {
                refs.insert(name.clone(), id.clone());
            }
            let mut value = pending.next().expect("counted");
            let name = match value
                .as_object_mut()
                .and_then(|object| object.remove("ref"))
            {
                Some(serde_json::Value::String(name)) if !name.is_empty() => Some(name),
                Some(_) => {
                    return Err(format!("operation {index}: ref must be a non-empty string"));
                }
                None => None,
            };
            substitute_refs(&mut value, &refs)
                .map_err(|error| format!("operation {index}: {error}"))?;
            names.push(name);
            crate::dto::parse_raw_command(value)
                .map_err(|error| format!("operation {index}: {error}"))
        })?;
        let ref_ids: std::collections::BTreeMap<String, String> = names
            .iter()
            .zip(&staged.created_ids)
            .filter_map(|(name, id)| Some((name.clone()?, id.clone()?)))
            .collect();
        let before = session.project();
        let (clips_before, markers_before, media_before, duration_before) = shape(before);
        let (clips_after, markers_after, media_after, duration_after) = shape(&staged.project);
        let validation = if validate {
            let report = query::validate(&staged.project, None)?;
            let baseline = query::validate(before, None)?;
            if report.errors > baseline.errors {
                return Err(format!(
                    "plan rejected: it introduces {} validation error(s); first: {}",
                    report.errors - baseline.errors,
                    report
                        .issues
                        .iter()
                        .find(|issue| issue.severity == "error")
                        .map(|issue| issue.message.as_str())
                        .unwrap_or("unknown")
                ));
            }
            Some(report)
        } else {
            None
        };
        let created_ids = staged.created_ids.clone();
        let changed = staged.applied;
        if !dry_run {
            session.commit(staged);
        }
        Ok(EditPlanResult {
            committed: !dry_run && changed,
            dry_run,
            old_revision,
            new_revision: if dry_run || !changed {
                old_revision
            } else {
                session.revision()
            },
            created_ids,
            refs: ref_ids,
            clips_delta: clips_after as i64 - clips_before as i64,
            markers_delta: markers_after as i64 - markers_before as i64,
            media_delta: media_after as i64 - media_before as i64,
            duration_before,
            duration_after,
            validation,
        })
    }

    /// Checks what an edit session depends on - writable app and scratch
    /// directories, the active project and its lock - and reports each as a
    /// coded result. There are no external encoders or probes to find: media
    /// is decoded and encoded in-process.
    pub fn health_check(&self) -> HealthReport {
        let mut checks = Vec::new();
        let mut dir_check = |name, ok_code, bad_code, dir: &Path, fix| {
            let result = writable(dir);
            let ok = result.is_ok();
            checks.push(HealthCheck {
                name,
                code: if ok { ok_code } else { bad_code },
                ok,
                message: match result {
                    Ok(()) => format!("{} is writable", dir.display()),
                    Err(error) => format!("{}: {error}", dir.display()),
                },
                remediation: Some(fix).filter(|_| !ok),
            });
        };
        dir_check(
            "config directory",
            "config_dir_ok",
            "config_dir_unwritable",
            &self.dirs.config,
            "Fix permissions on the config directory so recents can be saved.",
        );
        dir_check(
            "scratch directory",
            "scratch_dir_ok",
            "scratch_dir_unwritable",
            &std::env::temp_dir(),
            "Free space or fix permissions on the system temp directory.",
        );
        let guard = self.session_lock();
        checks.push(match guard.as_ref() {
            Some(open) => HealthCheck {
                name: "active project",
                code: "project_open",
                ok: true,
                message: format!("{} is open and locked by this agent", open.session.path()),
                remediation: None,
            },
            None => HealthCheck {
                name: "active project",
                code: "no_project",
                ok: true,
                message: "no project is open".to_owned(),
                remediation: None,
            },
        });
        HealthReport {
            healthy: checks.iter().all(|check| check.ok),
            checks,
        }
    }

    /// Undoes the last applied edit.
    pub fn undo(&self) -> Result<puppetcat_host::EditorView, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        Ok(session.undo())
    }

    /// Redoes the last undone edit.
    pub fn redo(&self) -> Result<puppetcat_host::EditorView, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;
        Ok(session.redo())
    }

    /// Probes a file and applies it into the bin as one step - the agent's
    /// two-step import (probe, then `AddMedia`) collapsed into one call.
    pub fn import_media(&self, path: &str) -> Result<puppetcat_host::EditorView, String> {
        let summary = puppetcat_host::media::probe(path)?;
        self.apply(Command::AddMedia {
            item: summary.to_new_media(),
        })
    }

    /// Adds standard black bookends without asking the caller to manufacture
    /// media or coordinate clips and markers. The generated one-pixel PNG is
    /// project-owned and reused on subsequent calls; the timeline mutation
    /// itself is one command and therefore one undo step.
    pub fn add_fade_to_black(
        &self,
        track_id: Option<String>,
        duration: f64,
        transition_duration: f64,
        shift_all_tracks: bool,
        shift_free_markers: bool,
    ) -> Result<puppetcat_host::EditorView, String> {
        let mut guard = self.session_lock();
        let session = guard.as_mut().ok_or_else(no_project)?;

        let generated = Path::new(session.path()).join(".puppetcat-generated");
        std::fs::create_dir_all(&generated)
            .map_err(|error| format!("could not create {}: {error}", generated.display()))?;
        let black = generated.join("solid-000000.png");
        if !black.exists() {
            let frame = Frame::from_rgba(1, 1, vec![0, 0, 0, 255])
                .ok_or_else(|| "could not create the black generator frame".to_owned())?;
            let bytes = puppetcat_media::png(&frame).map_err(|error| error.to_string())?;
            let partial = generated.join(".solid-000000.png.part");
            std::fs::write(&partial, bytes)
                .map_err(|error| format!("could not write {}: {error}", partial.display()))?;
            std::fs::rename(&partial, &black)
                .map_err(|error| format!("could not install {}: {error}", black.display()))?;
        }
        let path = black.to_string_lossy().into_owned();
        if !session.project().media.iter().any(|item| item.path == path) {
            let summary = puppetcat_host::media::probe(&path)?;
            session
                .apply(Command::AddMedia {
                    item: summary.to_new_media(),
                })
                .map_err(|error| error.to_string())?;
        }
        let media_id = session
            .project()
            .media
            .iter()
            .find(|item| item.path == path)
            .map(|item| item.id.clone())
            .ok_or_else(|| "the generated black media did not enter the bin".to_owned())?;
        let track_id = match track_id {
            Some(id) => id,
            None => {
                let timeline = session.project().active();
                timeline
                    .clips
                    .iter()
                    .filter(|clip| {
                        matches!(clip.kind, ClipKind::Video | ClipKind::Image)
                            && timeline
                                .track(&clip.track_id)
                                .is_some_and(|track| track.visible)
                    })
                    .min_by(|left, right| {
                        left.start
                            .total_cmp(&right.start)
                            .then_with(|| right.duration.total_cmp(&left.duration))
                    })
                    .map(|clip| clip.track_id.clone())
                    .ok_or_else(|| "the active timeline has no visible video clip".to_owned())?
            }
        };
        session
            .apply(Command::AddFadeToBlack {
                media_id,
                track_id,
                duration,
                transition_duration,
                shift_all_tracks,
                shift_free_markers,
            })
            .map_err(|error| error.to_string())?;
        Ok(session.view())
    }

    /// Analyses one bin item, one timeline clip, or the active timeline's
    /// audible mix. Times are source-local for a media item and timeline
    /// times for clip/timeline targets.
    #[allow(clippy::too_many_arguments)]
    pub fn analyze_audio(
        &self,
        media_id: Option<String>,
        clip_id: Option<String>,
        start: Option<f64>,
        end: Option<f64>,
        bucket_seconds: f64,
        silence_threshold_db: f64,
        min_silence: f64,
        include_buckets: bool,
    ) -> Result<AudioAnalysis, String> {
        if media_id.is_some() && clip_id.is_some() {
            return Err("choose mediaId or clipId, not both".to_owned());
        }

        #[derive(Clone)]
        struct Source {
            path: String,
            source_start: f64,
            source_duration: f64,
            timeline_start: f64,
            speed: f64,
            volume: f64,
            has_filters: bool,
            curved_speed: bool,
        }

        let (target, origin, target_end, sources) = {
            let guard = self.session_lock();
            let session = guard.as_ref().ok_or_else(no_project)?;
            let project = session.project();
            let timeline = project.active();
            if let Some(id) = media_id {
                let media = project
                    .media_by_id(&id)
                    .ok_or_else(|| format!("media {id:?} is not in the bin"))?;
                if !media.has_audio {
                    return Err(format!("media {id:?} has no audio stream"));
                }
                let origin = start.unwrap_or(0.0).max(0.0);
                let target_end = end.or(media.duration).unwrap_or(origin).max(origin);
                (
                    format!("media:{id}"),
                    origin,
                    target_end,
                    vec![Source {
                        path: media.path.clone(),
                        source_start: origin,
                        source_duration: target_end - origin,
                        timeline_start: origin,
                        speed: 1.0,
                        volume: 1.0,
                        has_filters: false,
                        curved_speed: false,
                    }],
                )
            } else if let Some(id) = clip_id {
                let clip = timeline
                    .clip(&id)
                    .ok_or_else(|| format!("clip {id:?} is not on the active timeline"))?;
                let media = project
                    .media_by_id(&clip.media_id)
                    .ok_or_else(|| format!("clip {id:?} has no media"))?;
                if !media.has_audio || clip.muted == Some(true) {
                    return Err(format!("clip {id:?} has no audible stream"));
                }
                let origin = start.unwrap_or(clip.start).max(clip.start);
                let target_end = end
                    .unwrap_or(clip.start + clip.duration)
                    .min(clip.start + clip.duration)
                    .max(origin);
                let speed = clip.speed.max(0.0625);
                (
                    format!("clip:{id}"),
                    origin,
                    target_end,
                    vec![Source {
                        path: media.path.clone(),
                        source_start: clip.source_start + (origin - clip.start) * speed,
                        source_duration: (target_end - origin) * speed,
                        timeline_start: origin,
                        speed,
                        volume: clip.volume,
                        has_filters: !clip.filters.is_empty(),
                        curved_speed: clip.speed_curve.is_some(),
                    }],
                )
            } else {
                let origin = start.unwrap_or(0.0).max(0.0);
                let target_end = end.unwrap_or_else(|| timeline.duration()).max(origin);
                let mut sources = Vec::new();
                for clip in &timeline.clips {
                    if !matches!(clip.kind, ClipKind::Video | ClipKind::Audio) {
                        continue;
                    }
                    let Some(track) = timeline.track(&clip.track_id) else {
                        continue;
                    };
                    let Some(media) = project.media_by_id(&clip.media_id) else {
                        continue;
                    };
                    if track.muted || clip.muted == Some(true) || !media.has_audio {
                        continue;
                    }
                    let from = clip.start.max(origin);
                    let to = (clip.start + clip.duration).min(target_end);
                    if to <= from {
                        continue;
                    }
                    let speed = clip.speed.max(0.0625);
                    sources.push(Source {
                        path: media.path.clone(),
                        source_start: clip.source_start + (from - clip.start) * speed,
                        source_duration: (to - from) * speed,
                        timeline_start: from,
                        speed,
                        volume: clip.volume,
                        has_filters: !clip.filters.is_empty(),
                        curved_speed: clip.speed_curve.is_some(),
                    });
                }
                ("timeline:active".to_owned(), origin, target_end, sources)
            }
        };

        let count = ((target_end - origin) / bucket_seconds.max(0.02)).ceil() as usize;
        let count = count.max(1);
        let mut power = vec![0.0f64; count];
        let mut peak = vec![0.0f64; count];
        let mut zcr_power = vec![0.0f64; count];
        let mut warnings = Vec::new();
        for source in sources {
            if source.has_filters {
                warnings.push(format!(
                    "{} has audio filters; analysis uses the decoded source before those filters",
                    source.path
                ));
            }
            if source.curved_speed {
                warnings.push(format!(
                    "{} has a speed curve; analysis maps it using the clip's mean speed",
                    source.path
                ));
            }
            let decoded = analysis::analyze_file(
                Path::new(&source.path),
                source.source_start,
                Some(source.source_duration),
                bucket_seconds * source.speed,
            )?;
            for bucket in decoded {
                let at = source.timeline_start + bucket.start / source.speed;
                if at < origin || at >= target_end {
                    continue;
                }
                let index = ((at - origin) / bucket_seconds.max(0.02)).floor() as usize;
                if index >= count {
                    continue;
                }
                let contribution = bucket.rms * source.volume.max(0.0);
                let squared = contribution * contribution;
                power[index] += squared;
                peak[index] = peak[index].max(bucket.peak * source.volume.max(0.0));
                zcr_power[index] += bucket.zcr * squared;
            }
        }
        let width = bucket_seconds.clamp(0.02, 2.0);
        let features: Vec<FeatureBucket> = (0..count)
            .map(|index| {
                let start = index as f64 * width;
                let end = ((index + 1) as f64 * width).min(target_end - origin);
                FeatureBucket {
                    start,
                    end,
                    rms: power[index].sqrt().min(1.0),
                    peak: peak[index].min(1.0),
                    zcr: if power[index] > 0.0 {
                        zcr_power[index] / power[index]
                    } else {
                        0.0
                    },
                }
            })
            .collect();
        warnings.sort();
        warnings.dedup();
        Ok(analysis::summarize(
            target,
            origin,
            features,
            width,
            silence_threshold_db,
            min_silence,
            include_buckets,
            warnings,
        ))
    }

    /// Returns a compact, optionally filtered summary of one timeline.
    #[allow(clippy::too_many_arguments)]
    pub fn timeline_summary(
        &self,
        timeline_id: Option<&str>,
        include_clips: bool,
        include_markers: bool,
        track_id: Option<&str>,
        start: Option<f64>,
        end: Option<f64>,
        limit: usize,
    ) -> Result<TimelineSummary, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        query::timeline_summary(
            session.project(),
            timeline_id,
            include_clips,
            include_markers,
            track_id,
            start,
            end,
            limit,
        )
    }

    /// Looks up one clip without serializing the whole project.
    pub fn get_clip(
        &self,
        clip_id: &str,
        timeline_id: Option<&str>,
    ) -> Result<puppetcat_project::model::Clip, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        query::clip(session.project(), clip_id, timeline_id)
    }

    /// Looks up one media-bin item without serializing the whole project.
    pub fn get_media(&self, media_id: &str) -> Result<puppetcat_project::model::MediaItem, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        query::media(session.project(), media_id)
    }

    /// Checks project references, times, files, transitions, and markers.
    pub fn validate_timeline(&self, timeline_id: Option<&str>) -> Result<ValidationReport, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        query::validate(session.project(), timeline_id)
    }

    /// Renders the active session at `time` to PNG bytes - the "see the
    /// timeline" capability the window has and an agent otherwise wouldn't.
    pub fn render_snapshot(
        &self,
        time: f64,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        crate::snapshot::render_still(session, &self.titles, &self.pool, time, width, height)
    }

    /// Renders several instants with the same preview path as exports and
    /// combines them into one image. Optional paths let callers persist the
    /// contact sheet and each full-resolution source frame without shelling
    /// out to image utilities.
    #[allow(clippy::too_many_arguments)]
    pub fn render_contact_sheet(
        &self,
        times: Vec<f64>,
        width: Option<u32>,
        height: Option<u32>,
        columns: Option<u32>,
        output: Option<String>,
        frames_output_dir: Option<String>,
    ) -> Result<crate::snapshot::ContactSheet, String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        crate::snapshot::render_contact_sheet(
            session,
            &self.titles,
            &self.pool,
            &times,
            width,
            height,
            columns,
            output.as_deref().map(Path::new),
            frames_output_dir.as_deref().map(Path::new),
        )
    }

    /// Starts exporting the active session. Errs immediately if one is
    /// already running; use `export_status` to poll the render itself.
    ///
    /// `range` limits the export to part of the timeline; see
    /// [`ExportRangeArgs`].
    pub fn start_export(
        &self,
        output: &str,
        crf: u8,
        preset: &str,
        range: &ExportRangeArgs,
    ) -> Result<(), String> {
        let guard = self.session_lock();
        let session = guard.as_ref().ok_or_else(no_project)?;
        let range = range.resolve(session.project().active())?;
        let titles = self
            .titles
            .clips(
                session.project(),
                session.settings().width,
                session.settings().height,
            )
            .into_iter()
            .map(|title| title.clip)
            .collect();
        let spec = puppetcat_host::export::ExportSpec {
            output: output.to_owned(),
            scratch_dir: puppetcat_host::export::temp_folder_preference(&self.dirs),
            crf,
            preset: preset.to_owned(),
            range,
        };
        self.export.start(session, spec, titles)
    }

    /// The running (or most recently finished) export's last known progress.
    pub fn export_status(&self) -> ExportStatus {
        self.export.status()
    }
}

fn no_project() -> String {
    "no project is open - call create_project or open_project first".to_owned()
}

/// Which part of the timeline an export covers, as an agent states it: by
/// seconds, by marker, or a mix (a marker for one end, seconds for the
/// other). Nothing at all is the whole timeline.
#[derive(Clone, Debug, Default)]
pub struct ExportRangeArgs {
    /// Seconds where the extract begins.
    pub start: Option<f64>,
    /// Seconds where the extract ends.
    pub end: Option<f64>,
    /// A marker (id or name) on the active timeline to begin at.
    pub from_marker: Option<String>,
    /// A marker (id or name) on the active timeline to end at.
    pub to_marker: Option<String>,
}

impl ExportRangeArgs {
    /// The `(start, end)` seconds this names on `timeline`, or `None` for
    /// the whole thing. An end left open runs to the end of the edit, a
    /// start left open begins at zero - but naming a marker or a time for
    /// either end makes it a range.
    fn resolve(
        &self,
        timeline: &puppetcat_project::model::Timeline,
    ) -> Result<Option<(f64, f64)>, String> {
        let marker_time = |key: &str| -> Result<f64, String> {
            let matches =
                |marker: &&puppetcat_project::model::Marker| marker.id == key || marker.name == key;
            timeline
                .markers
                .iter()
                .find(matches)
                .map(|marker| marker.time)
                .ok_or_else(|| format!("no marker \"{key}\" on the active timeline"))
        };
        if self.start.is_some() && self.from_marker.is_some() {
            return Err("give either start or fromMarker, not both".to_owned());
        }
        if self.end.is_some() && self.to_marker.is_some() {
            return Err("give either end or toMarker, not both".to_owned());
        }
        let start = match (&self.from_marker, self.start) {
            (Some(key), _) => Some(marker_time(key)?),
            (None, start) => start,
        };
        let end = match (&self.to_marker, self.end) {
            (Some(key), _) => Some(marker_time(key)?),
            (None, end) => end,
        };
        if start.is_none() && end.is_none() {
            return Ok(None);
        }
        let start = start.unwrap_or(0.0);
        let end = end.unwrap_or(f64::MAX);
        if end <= start {
            return Err(format!(
                "the export range is empty: it ends at {end:.2}s, before it starts at {start:.2}s"
            ));
        }
        Ok(Some((start, end)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_export_range_resolves_markers_and_seconds() {
        use puppetcat_project::model::{Marker, Project};
        let mut project = Project::new();
        for (id, name, time) in [("mk1", "intro", 2.0), ("mk2", "outro", 8.0)] {
            project.active_mut().markers.push(Marker {
                id: id.to_owned(),
                name: name.to_owned(),
                time,
                clip_id: None,
                offset: 0.0,
            });
        }
        let timeline = project.active();
        let args = |start, end, from: Option<&str>, to: Option<&str>| ExportRangeArgs {
            start,
            end,
            from_marker: from.map(str::to_owned),
            to_marker: to.map(str::to_owned),
        };
        assert_eq!(args(None, None, None, None).resolve(timeline), Ok(None));
        assert_eq!(
            args(None, None, Some("mk1"), Some("outro")).resolve(timeline),
            Ok(Some((2.0, 8.0)))
        );
        assert_eq!(
            args(Some(1.0), None, None, Some("mk2")).resolve(timeline),
            Ok(Some((1.0, 8.0)))
        );
        assert!(
            args(None, None, Some("nope"), None)
                .resolve(timeline)
                .is_err()
        );
        assert!(
            args(Some(1.0), None, Some("mk1"), None)
                .resolve(timeline)
                .is_err()
        );
        assert!(
            args(Some(5.0), Some(5.0), None, None)
                .resolve(timeline)
                .is_err()
        );
    }

    /// A scratch root for one test: a project location and, critically, its
    /// own `AppDirs` (see `engine` below) - the tests here must never touch
    /// this machine's real recents list.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "puppetcat-agent-engine-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// An `AgentEngine` rooted entirely under `root`, so `create_project`'s
    /// call to `projects::remember` writes a scratch recents list instead of
    /// this machine's real one.
    fn engine(root: &std::path::Path) -> AgentEngine {
        AgentEngine::with_dirs(puppetcat_host::AppDirs::under(&root.join("dirs")))
            .expect("builds against a scratch AppDirs")
    }

    /// Two `AgentEngine`s stand in for two separate `puppetcat-agent-mcp`
    /// processes: each opens its own file handle on the same lock file, so
    /// this exercises the same OS-level exclusion two real processes would
    /// hit, without actually spawning any.
    #[test]
    fn a_second_agent_cannot_open_a_project_the_first_already_has() {
        let root = scratch("two-agents");
        let first = engine(&root);
        let info = first
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Shared",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates and opens");

        let second = engine(&root);
        let error = second
            .open_project(&info.path)
            .expect_err("the first agent still has it open");
        assert!(error.contains("already"), "got: {error}");

        first.close_project().expect("releases the lock");
        second
            .open_project(&info.path)
            .expect("free once the first agent closes it");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The window's slot is independent of the agent's: a window watching
    /// this project must be able to see the agent's slot as held without
    /// that check ever depending on, or contending with, the window's own.
    #[test]
    fn the_agent_slot_never_blocks_the_window_slot() {
        let root = scratch("agent-vs-window");
        let agent = engine(&root);
        let info = agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Shared",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates and opens");

        assert!(puppetcat_host::SlotLock::is_held(
            Path::new(&info.path),
            Slot::Agent
        ));
        let window = puppetcat_host::SlotLock::try_acquire(Path::new(&info.path), Slot::Window);
        assert!(window.is_some(), "the window slot must still be free");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Reopening the very project already open here must not read as some
    /// other agent holding it - a client re-issuing `open_project` on its
    /// own project should just get a fresh session, not an error.
    #[test]
    fn reopening_the_same_project_here_succeeds() {
        let root = scratch("reopen-self");
        let agent = engine(&root);
        let info = agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Shared",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates and opens");
        agent
            .open_project(&info.path)
            .expect("reopening its own project is not a conflict");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_edit_plan_is_atomic_resolves_refs_and_checks_the_revision() {
        use serde_json::json;
        let root = scratch("edit-plan");
        let agent = engine(&root);
        agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Plan",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates and opens");
        let revision = agent.get_state().expect("state").revision;
        let tracks = agent
            .get_state()
            .expect("state")
            .project
            .active()
            .tracks
            .len();
        let plan = || {
            vec![
                json!({"op": "addTrack", "ref": "lane"}),
                json!({"op": "renameTrack", "trackId": "$lane", "name": "Overlay"}),
            ]
        };

        // A dry run reports but changes nothing.
        let dry = agent
            .apply_edit_plan(revision, plan(), true, true)
            .expect("dry run");
        assert!(!dry.committed);
        assert_eq!(dry.new_revision, revision);
        assert_eq!(agent.get_state().unwrap().revision, revision);

        // A bad operation commits nothing, even after a good one.
        let bad = vec![
            json!({"op": "addTrack"}),
            json!({"op": "renameTrack", "trackId": "$nope", "name": "x"}),
        ];
        assert!(agent.apply_edit_plan(revision, bad, false, true).is_err());
        assert_eq!(
            agent.get_state().unwrap().project.active().tracks.len(),
            tracks
        );

        // The real run resolves the ref and is one undo step.
        let done = agent
            .apply_edit_plan(revision, plan(), false, true)
            .expect("commits");
        assert!(done.committed);
        assert_ne!(done.new_revision, revision);
        let lane = done.refs.get("lane").expect("ref resolved").clone();
        let state = agent.get_state().unwrap();
        let track = state.project.active().track(&lane).expect("track exists");
        assert_eq!(track.name, "Overlay");

        // The old revision is now stale.
        let stale = agent
            .apply_edit_plan(revision, plan(), false, true)
            .expect_err("stale");
        assert!(stale.contains("stale"), "got: {stale}");

        agent.undo().expect("undo");
        assert_eq!(
            agent.get_state().unwrap().project.active().tracks.len(),
            tracks
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn health_check_reports_coded_results() {
        let root = scratch("health");
        let agent = engine(&root);
        let report = agent.health_check();
        assert!(report.healthy);
        assert!(report.checks.iter().any(|check| check.code == "no_project"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lifecycle_moves_and_renames_without_overwriting() {
        let root = scratch("lifecycle");
        let agent = engine(&root);
        let info = agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Life",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates");

        let exports = root.join("exports");
        assert!(
            agent
                .set_export_directory(&exports.to_string_lossy(), false)
                .is_err()
        );
        agent
            .set_export_directory(&exports.to_string_lossy(), true)
            .expect("creates");
        assert_eq!(agent.rename_project("Renamed").unwrap(), "Renamed");
        assert!(agent.rename_project("  ").is_err());

        let target = root.join("projects").join("Moved");
        std::fs::create_dir_all(&target).unwrap();
        assert!(
            agent.move_project(&target.to_string_lossy()).is_err(),
            "existing destination"
        );
        std::fs::remove_dir(&target).unwrap();
        let moved = agent
            .move_project(&target.to_string_lossy())
            .expect("moves");
        assert!(moved.path.ends_with("Moved"));
        assert!(!Path::new(&info.path).exists());
        assert!(agent.get_state().is_ok());

        let (from, to) = (root.join("a.mp4"), root.join("out").join("b.mp4"));
        std::fs::write(&from, b"video").unwrap();
        let report = agent
            .move_output(&from.to_string_lossy(), &to.to_string_lossy(), false, true)
            .expect("moves");
        assert_eq!(report.bytes, 5);
        assert!(!from.exists() && to.exists());
        std::fs::write(&from, b"other").unwrap();
        assert!(
            agent
                .move_output(&from.to_string_lossy(), &to.to_string_lossy(), false, true)
                .is_err()
        );
        assert!(from.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relink_reports_missing_files_and_changes_nothing() {
        let root = scratch("relink");
        let agent = engine(&root);
        agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                "Relink",
                1920,
                1080,
                30,
                1,
            )
            .expect("creates");
        agent
            .apply(Command::AddMedia {
                item: puppetcat_project::commands::NewMedia {
                    path: "/old/clip.mp4".to_owned(),
                    name: "clip.mp4".to_owned(),
                    duration: Some(2.0),
                    kind: puppetcat_project::model::MediaKind::Video,
                    width: None,
                    height: None,
                    frame_rate: None,
                    frame_rate_fraction: None,
                    video_codec: None,
                    audio_codec: None,
                    has_audio: false,
                },
            })
            .expect("adds");
        let revision = agent.get_state().unwrap().revision;
        let report = agent
            .relink_media(
                vec![],
                Some(("/old".to_owned(), "/nowhere".to_owned())),
                false,
            )
            .expect("reports");
        assert!(!report.committed);
        assert_eq!(report.items[0].status, "missing");
        assert_eq!(agent.get_state().unwrap().revision, revision);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A scratch project holding one 10s video clip on track T1.
    fn project_with_clip(name: &str) -> (std::path::PathBuf, AgentEngine, String) {
        let root = scratch(name);
        let agent = engine(&root);
        agent
            .create_project(
                &root.join("projects").to_string_lossy(),
                name,
                1920,
                1080,
                30,
                1,
            )
            .expect("creates");
        let view = agent
            .apply(Command::AddMedia {
                item: puppetcat_project::commands::NewMedia {
                    path: "/nowhere/clip.mp4".to_owned(),
                    name: "clip.mp4".to_owned(),
                    duration: Some(10.0),
                    kind: puppetcat_project::model::MediaKind::Video,
                    width: None,
                    height: None,
                    frame_rate: None,
                    frame_rate_fraction: None,
                    video_codec: None,
                    audio_codec: None,
                    has_audio: false,
                },
            })
            .expect("adds media");
        let media = view.created_id.expect("media id");
        let view = agent
            .apply(Command::AddClip {
                media_id: media,
                track_id: "T1".to_owned(),
                start: 0.0,
            })
            .expect("adds clip");
        let clip = view.created_id.expect("clip id");
        (root, agent, clip)
    }

    #[test]
    fn layouts_save_apply_roundtrip_and_undo() {
        let (root, agent, clip) = project_with_clip("layouts");
        let corner = LayoutValues {
            scale: Some(0.3),
            offset_x: Some(0.35),
            offset_y: Some(0.35),
            ..Default::default()
        };
        agent
            .save_layout_template("corner", None, corner)
            .expect("saves");
        let selector = || ClipSelector {
            track_id: Some("T1".to_owned()),
            ..Default::default()
        };

        let dry = agent
            .apply_layout_template("corner", selector(), true)
            .expect("dry");
        assert_eq!((dry.committed, dry.changed), (false, 1));
        let done = agent
            .apply_layout_template("corner", selector(), false)
            .expect("applies");
        assert!(done.committed);
        let state = agent.get_state().unwrap();
        let styled = state
            .project
            .active()
            .clips
            .iter()
            .find(|c| c.id == clip)
            .unwrap();
        assert_eq!(styled.scale, 0.3);
        agent.undo().unwrap();
        let state = agent.get_state().unwrap();
        assert_eq!(state.project.active().clips[0].scale, 1.0);

        assert!(
            agent
                .apply_layout_template("nope", selector(), false)
                .is_err()
        );

        let file = root.join("layouts.json");
        assert_eq!(
            agent
                .export_layout_templates(&file.to_string_lossy(), false)
                .unwrap(),
            1
        );
        assert!(
            agent
                .export_layout_templates(&file.to_string_lossy(), false)
                .is_err()
        );
        agent.remove_layout_template("corner").unwrap();
        let names = agent
            .import_layout_templates(&file.to_string_lossy(), false)
            .unwrap();
        assert_eq!(names, vec!["corner".to_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn batch_export_preflight_rejects_bad_plans_before_rendering() {
        let (root, agent, _clip) = project_with_clip("batch");
        for (name, time) in [
            ("Hook start", 1.0),
            ("Hook end", 3.0),
            ("Tail start", 5.0),
            ("Tail end", 8.0),
        ] {
            agent
                .apply(Command::AddMarker {
                    time,
                    name: Some(name.to_owned()),
                    clip_id: None,
                })
                .unwrap();
        }
        let out = root.join("out");
        let request = |ranges: Vec<BatchRangeArg>, pair: bool| BatchRequest {
            ranges,
            pair_by_name: pair,
            template: "{index}-{name}".to_owned(),
            output_dir: out.to_string_lossy().into_owned(),
            crf: 18,
            preset: "medium".to_owned(),
            collision: "fail".to_owned(),
            head: 0.5,
            tail: 0.5,
            dry_run: true,
        };

        let plan = agent
            .start_batch_export(request(vec![], true))
            .expect("pairs by name");
        assert!(!plan.started);
        assert_eq!(plan.items.len(), 2);
        assert!(plan.items[0].output.ends_with("01-hook.mp4"));
        assert_eq!(plan.items[0].range, (0.5, 3.5));
        assert!(!out.exists(), "a dry run writes nothing");

        let range = |start, end| BatchRangeArg {
            start: Some(start),
            end: Some(end),
            ..Default::default()
        };
        assert!(
            agent
                .start_batch_export(request(vec![range(1.0, 5.0), range(4.0, 6.0)], false))
                .is_err(),
            "overlap"
        );
        assert!(
            agent
                .start_batch_export(request(vec![range(5.0, 2.0)], false))
                .is_err(),
            "reversed"
        );
        assert!(
            agent.start_batch_export(request(vec![], false)).is_err(),
            "nothing to export"
        );

        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("01-hook.mp4"), b"x").unwrap();
        assert!(
            agent.start_batch_export(request(vec![], true)).is_err(),
            "existing file"
        );
        let mut skipping = request(vec![], true);
        skipping.collision = "skip".to_owned();
        let plan = agent.start_batch_export(skipping).unwrap();
        assert!(plan.items[0].skip && !plan.items[1].skip);
        let _ = std::fs::remove_dir_all(&root);
    }
}
