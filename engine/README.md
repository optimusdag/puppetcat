# PuppetCat Engine

The video engine behind PuppetCat. Rust, no GC, no hidden control flow.

## Layout

| Crate | What it is | Dependencies |
|---|---|---|
| `puppetcat-core` | Time, arena, frames, timeline model. The vocabulary every other crate speaks. | **none** (std only) |
| `puppetcat-media` | Getting pixels and samples in and out of files. Links FFmpeg (libav*), and is the only crate that knows it exists. | `puppetcat-core`, ffmpeg-the-third |
| `puppetcat-project` | The edit itself: document model, operations as commands, undo, puppetcat.json IO. | serde, serde_json |
| `puppetcat-render` | Turning a timeline plus a timestamp into one finished frame. | `puppetcat-core`, wgpu (optional) |
| `puppetcat-effects` | Effect packages: manifests, chain templates, the catalogue. Each built-in effect is a folder under `packages/`. | `puppetcat-project`, serde, toml |
| `puppetcat-export` | Timeline to file: flatten, the frame-by-frame render loop, the paused monitor's true frame. | `puppetcat-core`, `puppetcat-media`, `puppetcat-render`, `puppetcat-project`, `puppetcat-effects` |
| `puppetcat-cli` | A binary to drive the above. The vertical slice. | the engine crates |
| `puppetcat-host` | What the window needs that is not the edit: sessions, project folders, previews, playback, templates, job slots. | `puppetcat-media`, `puppetcat-project`, `puppetcat-export`, cpal |
| `puppetcat-speech` | Transcription (whisper.cpp, in-process) and text to speech (Kokoro via sherpa-onnx). | `puppetcat-host`, `puppetcat-media`, whisper-rs, sherpa-onnx |
| `puppetcat-text` | Titles as pixels: shapes text and paints plate, shadow, outline and fill onto a frame-sized canvas. Used by `puppetcat-host`. | none of the engine crates |
| `puppetcat-agent` | The agent surface: an MCP server (`puppetcat-agent-mcp`) and an in-process engine for driving a project with no window, plus the bridge that lets a running window host the tools. | `puppetcat-host`, `puppetcat-export`, `puppetcat-effects`, `puppetcat-speech`, rmcp |
| `puppetcat` | The editor window: every pane, dialog and primitive, in Slint. The app a user launches. | slint, `puppetcat-host`, `puppetcat-speech`, `puppetcat-agent` |

The dependency arrows point one way:

```
puppetcat ------> puppetcat-speech -> puppetcat-host -> {export, project, media} -> core
    |                                       |             -> render -> core
    +-> puppetcat-agent -> host, speech     +-> text      export -> effects -> project
puppetcat-agent-mcp (binary) -> puppetcat-agent
```

If you ever find yourself wanting `core` to depend on `media`, something has
been put in the wrong crate.

`puppetcat` is the app. It opens project folders through `puppetcat-host`, reads
the engine's project to draw the bin and the lanes, and writes every edit as
a `puppetcat-project` command.

## Build and run

```sh
cargo build
cargo test
cargo run -p puppetcat-cli -- probe some-video.mp4
cargo run -p puppetcat-cli -- render some-video.mp4 out.mp4 --frames 120
cargo run -p puppetcat                    # the editor window, debug
cargo run --profile quick -p puppetcat    # optimised, rebuilds in seconds: for trying changes
cargo build --profile app -p puppetcat    # the shipping binary: fat LTO, panic=abort, stripped
```

Nothing is spawned at run time: FFmpeg is linked, whisper.cpp and sherpa-onnx
are compiled in. A build needs the FFmpeg 7+ development libraries (headers
and import libraries) - `brew install ffmpeg` on macOS, a BtbN `shared` build
unpacked and pointed at with `FFMPEG_DIR` on Windows and on Linux
distributions whose packaged FFmpeg is older than 7 - plus cmake and a C++
toolchain for whisper.cpp. The window builds with Slint's
Skia renderer by default; `--no-default-features --features wgpu` swaps in
FemtoVG over wgpu, and the two are meant to be compared, not chosen once.
On Linux, Skia needs the fontconfig and freetype headers at build time (see
`.github/workflows/build-app.yml` for the package list).

On Nix, the flake at the repository root builds the window with every native
dependency pinned: `nix build` (then `./result/bin/puppetcat`), `nix run`, or
`nix develop` for a shell with the toolchain and libraries in it. Linux
x86_64 and aarch64.

## Reading this codebase cold

1. [`../ARCHITECTURE.md`](../ARCHITECTURE.md) - the map of the whole system. Read it first.
2. `crates/*/src/lib.rs` - every crate opens with a `//!` block saying what it is for.
3. `cargo doc --open` - the generated API map of the whole engine.

## Conventions

- **Time is exact.** All timestamps are `puppetcat_core::time::Rational` seconds, never `f64`.
  Frame-accurate editing and floating point do not mix.
- **Graphs use handles, not pointers.** `puppetcat_core::arena` explains why.
- **Shallow generics.** Concrete types until three call sites demand otherwise.
- **Threads, not async.** The render path is CPU-bound; `async` buys nothing here.
