<div align="center">

# PuppetCat

**A free, open-source CapCut-style video editor.**

<p align="center">
  <a href="https://github.com/optimusdag/puppetcat/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/optimusdag/puppetcat/ci.yml?style=flat&logo=githubactions&logoColor=F8F8F8&label=Build&labelColor=000000&color=c6f432" alt="Build Status" /></a>
  <a href="https://github.com/optimusdag/puppetcat/releases/latest"><img src="https://img.shields.io/badge/Download-macOS%20%7C%20Windows%20%7C%20Linux-c6f432?style=flat&logo=desktop-download&logoColor=F8F8F8&labelColor=000000" alt="Download PuppetCat" /></a>
  <a href="https://github.com/optimusdag/puppetcat/releases"><img src="https://img.shields.io/badge/Version-0.2.0-c6f432?style=flat&logo=semver&logoColor=F8F8F8&labelColor=000000" alt="PuppetCat Version 0.2.0" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-AGPL%20v3-c6f432?style=flat&logo=gnu&logoColor=F8F8F8&labelColor=000000" alt="License: AGPL-3.0-or-later" /></a>
</p>

<img src="assets/preview-dark.png" alt="PuppetCat editor" width="100%" />

</div>

---

PuppetCat is a fork of [Concat](https://github.com/jub0t/Concat), an
open-source CapCut replacement. It does everything you use CapCut for —
without the watermarks, paywalls, or subscriptions. A native Rust engine does
the heavy lifting, a native interface does the editing, and it all runs on
your machine: install it and start cutting, no account, no extra downloads,
no setup.

> [!NOTE]
> PuppetCat is one native Rust binary: the engine and the Slint editor window
> live together in [`engine/`](./engine). See
> [`engine/README.md`](./engine/README.md) to build it from source.

## Highlights

- Free and local Text-to-Speech features.
- 🎬 Multi-track editing, with several timelines per project when one isn't enough
- ✂️ The cutting toolkit you'd expect: split, trim, merge, transitions, speed control
- 💬 Auto-captions that run entirely on your machine — your audio never leaves it
- 🎙️ Voice filters for cleaning up or playing with your sound
- 📝 Titles and styled text
- 📦 Templates — build an edit once, reuse it for the next video
- 🚫 No watermarks, no account, nothing behind a paywall
- 🖥️ Works the same on macOS, Windows and Linux

## Get started

PuppetCat is currently in **Alpha (pre-release)**. Download the latest build from [Releases](https://github.com/optimusdag/puppetcat/releases).

**Platform support:**

- ✅ **Windows** — tested
- ✅ **macOS** — unsigned binaries; run:
  `xattr -dr com.apple.quarantine /Applications/PuppetCat.app`
- ✅ **Linux**
  - 🧪 ARM
  - 🧪 x86_64
- 🚧 **Android**
  - Phones
  - Tablets
- 🚧 **iOS / iPadOS**
  - iPhone
  - iPad
- 🧪 **Raspberry Pi 5**

**Status:** ✅ Supported · 🚧 Work in progress · 🧪 To be tested

## Contribution

> [!IMPORTANT]
> The best way to contribute is to grab a build from the [Release](https://github.com/optimusdag/puppetcat/releases) page and test the application to see where it breaks or how it can be improved.

Ready to write code? [CONTRIBUTING.md](./CONTRIBUTING.md) covers setup, layout, the checks to run, and how contributions are licensed.

## Roadmap

Read [ROADMAP.MD](./ROADMAP.MD).

## License

PuppetCat is free software under the [GNU Affero General Public License v3.0 or
later](./LICENSE), inherited from Concat.

**If you just want to edit videos, none of this affects you.** Download a build
and use it for anything, including commercial work. There is nothing to accept,
register or pay, and no obligation attaches to you as a user.

**If you fork it, you are welcome here.** Modify it, ship it, sell it, host it.
The one condition is reciprocity: those who receive your version get the source
too, under the same license. That includes users who reach it over a network —
that is the "Affero" part, and it is why a hosted PuppetCat cannot be closed.

Two things sit alongside the AGPL:

- **Plugins keep their own license.** The [plugin
  exception](./LICENSE-EXCEPTIONS.md) means anything built on the PuppetCat API is
  yours to license as you like. The copyleft covers PuppetCat, not what talks to it.
- **The name is not part of the grant.** The code is free; *PuppetCat* and the logo
  identify builds from this project. Forks are encouraged — please ship them
  under your own name. See [TRADEMARK.md](./TRADEMARK.md).

Contributions are made under the [CLA](./CLA.md); you keep copyright in your
work. Distributed builds also include GPL-licensed FFmpeg and espeak-ng — see
[THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md).
