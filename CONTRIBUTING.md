# Contributing to PuppetCat

Thanks for being here. PuppetCat is a video editor that runs entirely on the
user's machine, and it gets better mostly through people using it hard and
reporting what broke.

## The most valuable thing you can do

Grab a build from the [Releases](https://github.com/optimusdag/puppetcat/releases) page
and edit a real video with it. Real footage finds what no reading of the
source does. A good bug report — what you did, what happened, your
OS, the media you used — is worth more than most patches.

## Before you write code

Open an issue first for anything beyond a small fix. Items on the
[`ROADMAP.md`](ROADMAP.md) may be in progress or deferred, and it is
genuinely no fun to review a big PR that has to be turned down for reasons
that were invisible from outside.

## Setting up

You will need Rust (see `rust-version` in `engine/Cargo.toml`), the FFmpeg
7+ development libraries (`brew install ffmpeg`; see `engine/README.md` for
Windows and Linux), cmake and a C++ compiler. Then:

```sh
cd engine && cargo run -p puppetcat
```

That is the editor window. Everything - the engine, the host layer and the
Slint UI - is one Cargo workspace under `engine/`.

## Layout

| Path | What lives there |
|---|---|
| `engine/crates/` | The engine (core, media, render, export, project), the host layer (`puppetcat-host`, `puppetcat-speech`), the CLI, and `puppetcat`, the Slint editor window |
| `test/` | Media and analysis fixtures |

[`ARCHITECTURE.md`](ARCHITECTURE.md) explains how these fit together and where
the sharp edges are. Read it before touching the engine.

## Checks

Run these before opening a PR:

```sh
cd engine && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

New source files need a licence header — see below. Match the style of the code
around you; the engine avoids cleverness on purpose.

## Licensing your contribution

PuppetCat is **AGPL-3.0-or-later** ([`LICENSE`](LICENSE)), with a plugin exception
so that PuppetCat API plugins can carry their own licence
([`LICENSE-EXCEPTIONS.md`](LICENSE-EXCEPTIONS.md)).

Contributions are accepted under the CLA in [`CLA.md`](CLA.md). **You keep the
copyright in your work** — the CLA is a licence, not an assignment. It exists so
that copyright in PuppetCat stays centralised, which is what makes the AGPL
enforceable against companies that take the code without honouring it.

Sign off your commits to indicate agreement:

```sh
git commit -s -m "your message"
```

Typo and documentation fixes do not need a sign-off.

### File headers

New source files start with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors
```

Files inherited from Concat keep their original copyright line
(`Jareer and Concat contributors`) — that is a historical record, not a
placeholder, and it is not replaced by editing the file. Add your own line
alongside it if you want one; don't remove what's there.

### Third-party code

If your patch brings in code you did not write, say so in the PR: what it is,
where it came from, and its licence. Anything incompatible with
AGPL-3.0-or-later cannot be merged, and licence problems are much cheaper to
catch before a merge than after a release.

## Trademarks

The code is free to fork. The PuppetCat name and logo are not covered by the
AGPL grant — see [`TRADEMARK.md`](TRADEMARK.md). Forks are welcome; please
ship them under your own name, the same courtesy this project extends to
Concat, the project it was forked from.

## Conduct

[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) applies to the repo and its
Discussions.
