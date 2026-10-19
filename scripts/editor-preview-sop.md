# SOP: updating the README screenshot

The image at the top of `README.md` is `assets/editor-screenshot.png`, a single
screenshot of the editor window.

## Steps

1. **Run the app** — `cd engine && cargo run --release -p puppetcat`. Stage something presentable:
   media in the bin, a couple of tracks on the timeline, the playhead
   somewhere interesting. Use media you have the right to publish, and avoid
   personal file names in the media panel.

2. **Take the screenshot.** On macOS press `⌘⇧4`, then `Space`, then click the
   PuppetCat window (this captures just the window).

3. **Replace `assets/editor-screenshot.png`** with it. The README references the
   file by path, so no README edit is needed.

4. **Check the result** — open the README preview and confirm the image is
   sharp and shows nothing private.

5. **Commit** the changed file.
