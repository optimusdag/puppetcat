# Roadmap

> PuppetCat is still in early development. This roadmap represents the direction of the project rather than guaranteed features or release dates.
>
> Inherited from [Concat](https://github.com/jub0t/Concat) at fork time and
> renamed in place; it has not yet been reviewed against PuppetCat's own
> priorities. Treat it as a starting point, not a commitment.
>
> 🌟 = high priority / especially wanted

### 🧱 Editor & Core Architecture

* [x] 🌟 **PuppetCat API** — A stable API shared by the PuppetCat UI, plugins, scripts, automation, and MCP integrations. *(Built: an MCP server and a CLI over one engine; every editing command is reachable, and the window calls the same shared functions.)*
* [ ] 🌟 **Extensible Effects Runtime** — A scalable, data-driven system capable of supporting hundreds of effects, filters, transitions, and generators, including custom WGSL shaders.
* [ ] **Hardware Analysis** — Detect device capabilities and performance tier so PuppetCat can automatically choose appropriate playback, rendering, proxy, and processing settings.
* [ ] **Customizable Workspace** — Allow panels to be moved, resized, docked, and rearranged similar to Blender and other professional creative tools.

### 🎨 Effects, Transitions & Mixers

* [ ] 🌟 **Effect Mixer** — Create custom effects by combining reusable processing nodes, with support for custom WGSL shader nodes.
* [ ] 🌟 **Transition Mixer** — Build and share custom transitions from reusable nodes or custom shaders.
* [ ] 🌟 **Voice Mixer** — Create custom voice effects by combining audio processing stages such as pitch shift, formant shift, EQ, compression, reverb, delay, chorus, distortion, noise gate, limiter, stereo width, filters, de-essing, excitation, and more.
* [ ] **Generators** — Procedural visual sources such as gradients, noise, shapes, animated backgrounds, particles, grids, audio visualizers, and other generated media.
* [ ] **Reusable Creative Assets** — Expand support for color presets, caption styles, title animations, motion presets, effect presets, and other reusable creative components.

### 📦 Templates & Community Ecosystem

* [ ] 🌟 **PuppetCat Hub / Registry** — A centralized community registry for installing and sharing templates, effects, transitions, caption styles, presets, generators, Mixer creations, and other extensions — somewhat like npm for creative assets.
* [ ] **Improved Templates** — Expand the template system and make templates easier to create, customize, package, and share.
* [ ] **Stock Media Library** — Integrate accessible stock video, images, music, and sound effects.

### 🎬 Timeline & Animation

* [ ] 🌟 **Keyframe Graph Editor** — Fine-grained control over animation curves, easing, interpolation, and effect parameters.
* [ ] **Speed Curves / Time Remapping** — Custom speed ramps, reusable velocity presets, and improved slow/fast-motion workflows. *(Partly built: the engine supports speed curves, reverse and constant speed; curve presets and a curve editor are not verified.)*
* [ ] **Adjustment Layers** — Apply effects, filters, and color adjustments across multiple clips.
* [ ] **Scene Detection** — Detect cuts in imported footage and optionally split clips automatically. *(Partly built: `detect_scenes` and Clip > Detect scenes find the cuts and mark them; splitting at them automatically is not.)*
* [x] **Beat Detection & Beat Markers** — Analyze music and create timeline markers for rhythm-based editing. *(Built: `detect_beats` and the window's Mark beats actions find beats and a tempo and drop markers; no rhythm-based auto-editing yet.)*
* [x] **Auto Cut / Beat Sync** — Automatically create edits synchronized to music and detected beats. *(Built: `cut_to_beats` and the window's Cut video to beat markers actions split visual clips at detected beat markers as one undoable edit, with clip and range scopes.)*

### 📝 Captions & Transcript Editing

* [ ] 🌟 **Advanced Caption Styling** — Reusable caption styles, animations, per-word effects, and other creator-focused caption tools.
* [ ] **Word Highlighting** — Use word-level transcription timestamps to highlight spoken words dynamically.
* [ ] **Transcript-Based Editing** — Edit spoken content through its transcript, with timeline edits automatically following text changes.
* [ ] **Filler-Word Removal** — Detect and remove filler words such as “um”, “uh”, and similar speech disfluencies.
* [ ] **Profanity Detection** — Detect profanity from transcribed audio and provide options to censor, mute, or modify affected sections.

### 🎙️ Audio

* [x] 🌟 **Noise Removal / Voice Cleanup** — Local audio denoising and speech enhancement. *(Built: the Noise Reduction and Enhance Voice audio effects (FFmpeg `afftdn`).)*
* [x] **Silence Detection & Removal** — Automatically detect silent regions and optionally remove them from the timeline. *(Built: `analyze_audio` finds it, `remove_silence` and the window's Remove silence sheet cut it.)*
* [x] **Audio Normalization** — Quickly normalize dialogue and other audio to useful loudness targets. *(Built: the Normalize Loudness audio effect (EBU R128).)*
* [x] **Beat Analysis** — Reusable audio analysis infrastructure for beat detection, synchronization, and music-driven effects. *(Built: the beat detector is reusable analysis in `puppetcat-host`.)*

### 👁️ Tracking, Masks & Computer Vision

* [ ] 🌟 **Object & Face Tracking** — Track people, faces, and selected objects through footage and attach effects, text, masks, or other elements to tracked targets.
* [ ] 🌟 **Masks & Tracked Masks** — Rectangle, ellipse, polygon/Bézier, feathering, expansion, inversion, keyframing, and tracking.
* [ ] **Automatic Face Detection & Blurring** — Detect and optionally track faces for automatic privacy blurring.
* [ ] **Background Removal** — Local subject segmentation and background removal without requiring a green screen.
* [ ] **Auto Reframe** — Automatically reframe footage for different aspect ratios while keeping important subjects in view.
* [ ] **Chroma Key** — Green/blue-screen removal with adjustable tolerance, spill suppression, feathering, and masking. *(Partly built: the Green Screen and Blue Screen effects, with similarity and softness; spill suppression is not.)*

### 🔎 Media Intelligence & Automation

* [ ] **Smart Media Search** — Search local project media using speech, filenames, metadata, and eventually visual content.
* [ ] **Long Video → Clips** — Help identify and extract useful short-form segments from longer recordings.
* [ ] **Local Media Analysis** — Build reusable background analysis for transcripts, scenes, faces, audio, and other metadata that multiple PuppetCat features can share.

### 🔌 Plugins & Extensibility

* [ ] 🌟 **Plugin System** — Allow developers to extend PuppetCat through the PuppetCat API without modifying the core application.
* [ ] **Custom Mixer Nodes** — Let developers create reusable nodes for Mixers, including GPU shader nodes and sandboxed processing nodes.
* [ ] **Extension Permissions & Sandboxing** — Provide a safe capability/permission model for third-party extensions.
* [ ] **Extension Versioning & Dependencies** — Stable API versions, dependency resolution, compatibility checks, and updates for community extensions.

---

### Longer-Term Ideas

These are areas worth exploring after the core editing experience is mature:

* [ ] Optical-flow frame interpolation
* [ ] Video stabilization
* [ ] Video denoising and restoration
* [ ] Local video upscaling
* [ ] Motion Mixer / reusable animation graphs
* [ ] Caption Mixer
* [ ] Advanced color grading and LUT workflows
* [ ] Local semantic video search
* [ ] Community-created automation tools
* [ ] Plugin SDK and developer tooling
