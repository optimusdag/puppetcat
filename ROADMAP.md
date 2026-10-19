# Roadmap

> PuppetCat is in early development. This roadmap is the direction of the
> project, not a promise of features or dates.
>
> 🌟 = high priority / especially wanted

## Built

Since the fork from [Concat](https://github.com/jub0t/Concat):

### Agent & MCP

* [x] 🌟 **One engine, two front ends** — Every editing command is reachable from the window, the CLI and MCP, through the same shared functions; the window can host the agent's tools.
* [x] **Transactional edit plans** — Revision tokens and `apply_edit_plan`, with operations that can reference ids minted by earlier ones.
* [x] **Agent inspection tools** — Timeline queries, contact sheets, `get_capabilities`, `health_check` and `list_catalogue`.
* [x] **Safe project and media lifecycle** — Open, release, relink and move projects through the agent, with a cross-process project lock (an agent always wins; two windows take turns).
* [x] **Speech tools** — Transcription, captions and voice edits available to the agent and shared with the window.

### Editing

* [x] **Ripple editing** — Ripple trim, delete and insert, and insert gap, with link and marker policies.
* [x] **Delete range and Remove silence** — Cut a time range or every silent region as one undoable edit.
* [x] **Beat detection, beat markers and Cut to beats** — Find beats and tempo, mark them, and split clips at them.
* [x] **Scene detection** — `detect_scenes` and Clip > Detect scenes mark the cuts.
* [x] **Markers attached to clips** — With clip and timeline context menus.
* [x] **Relink media, rename and move project** — In the window and for agents.
* [x] **Follow playhead, track reordering, live inspector details**

### Transitions

* [x] **Motion transitions** — Wipe, push and zoom, showing both pictures between different clips.
* [x] **Cross-fades that never reveal cut footage or dip through black** — Preview and export match; chained transitions get separate lanes.
* [x] **Fade bookends** — Fade in/out and fade to black, in the window and for agents.
* [x] **Transitions in the clip context menu**

### Export & Templates

* [x] **Export at the project's own shape** — Resolution and frame-rate templates, and export of a selected range.
* [x] **Batch export** — By marker pair, with project layout templates and template slots.
* [x] **Scratch-folder export setting** and a scrub-friendly keyframe interval.

### Platform

* [x] **Signed and notarized macOS builds** — A local packaging script and DMG.
* [x] **Open project** on the start screen and in the File menu.
* [x] **Fixes** — Green/Blue Screen FFmpeg chains, crash-on-quit, and an audio mix that hung on a clip trimmed to a track's end.

## Next

* [ ] 🌟 **Windows and Linux testing** — CI builds every platform, but only macOS is tested and signed; test, package and support the others.
* [ ] 🌟 **Rough cuts from analysis** — Higher-level agent tools that go from analysis to an edit: split at detected scenes (detection exists, splitting doesn't), drop filler words, and assemble a sequence from tagged clips.
* [ ] 🌟 **Transcript-based editing** — Transcription and captions exist; edit spoken content as text with the timeline following, plus word-level highlighting and filler-word removal.
* [ ] **Agent verification loop** — Agents can already render snapshots and contact sheets; add checks that an edit did what it meant, such as comparing frames before and after.
* [ ] **Plans you can review** — Agents get a dry-run plan diff today; show it in the window before an edit applies, with per-step undo.
* [ ] **Keyframe graph editor** — Curves, easing and interpolation for animation and effect parameters, plus speed-curve presets.
* [ ] **Tracked masks and face blur** — Bézier masks, mask tracking, and automatic face detection and blurring.
* [ ] **Chroma key spill suppression** — Finish the Green/Blue Screen effects.
* [ ] **Adjustment layers** — Apply effects and colour across multiple clips.
* [ ] **Hardware analysis and proxies** — Detect device capability and pick playback and render settings automatically.
* [ ] **Plugin system** — Extend PuppetCat through its API without modifying the core, with a permission model for third-party extensions.

## Longer-term ideas

* [ ] A community registry for templates, effects and transitions
* [ ] Custom effect and transition mixers on WGSL shader nodes
* [ ] Background removal and auto reframe
* [ ] Video stabilization, frame interpolation and upscaling
* [ ] Advanced colour grading and LUT workflows
* [ ] Local semantic search over project media
