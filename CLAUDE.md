# Graphit

Pen-first note-taking app in Rust with GTK4, libadwaita, poppler and cairo. It replaces Xournal++ and keeps its `.xopp` files as they are. An optional study companion runs Claude Code as a subprocess. Public repo: LaserFocus007/graphit, license GPL-3.0.

## Working with the maintainer

- Talk to the maintainer in German. Everything in the repo (README, commits, release notes, docs) is in English. Code comments and German UI source strings stay German (see `src/i18n.rs`).
- Ask before building something that could go in the wrong direction. Show options (mockups help) when the design is open.
- "Clean and polished" is the bar: native GNOME look, smooth, no half-finished UI.
- The maintainer runs `sudo` commands themselves. Never ask for or probe credentials.
- Change only what was asked. Example: pen latency work must not touch the CPU power profile.

## Privacy rules (strict)

- Never put an email address, real name, university or other personal data into the repo, commits, releases, screenshots or test files.
- Commit with the repo-local identity `Graphit <graphit@users.noreply.github.com>` and end every commit message with `Co-Authored-By: Claude <noreply@anthropic.com>` (or the attribution line the session asks for).
- Before every commit and release, scan the diff and the built package for home paths and personal strings. Build scripts use `--remap-path-prefix` so no local paths end up in binaries.
- Screenshots for docs use neutral demo content only.

## Layout

| Path | What it does |
|---|---|
| `src/main.rs` | Startup, crash log, test hooks (`GRAPHIT_*` env vars) |
| `src/window.rs` | Main window: tabs, dock, sidebar, split view, library, text bar |
| `src/canvas.rs`, `src/canvas/` | Drawing widget. GPU tiles (`tiles.rs`), editing (`edit.rs`), radial menu, laser, PDF text selection (`search.rs`), Claude's marks (`guide.rs`) |
| `src/model.rs`, `src/xopp.rs`, `src/xml.rs` | Document model (immutable items in `Arc`) and the Xournal++ format |
| `src/render.rs`, `src/tiles.rs` | Cairo rasterizing on worker threads, tile cache keyed by page, kind, level, tx, ty |
| `src/claude.rs` | Study companion: chat, walkthrough cards, quiz, card placement |
| `src/place.rs` | Claude writing into notes, placement on free space |
| `src/latex.rs` | Formulas via `pdflatex` and `pandoc` |
| `src/speech.rs` | Reading aloud with Piper voices, system voice as fallback |
| `src/platform.rs` | Linux/Windows differences: starting and killing helper processes |
| `src/settings.rs`, `src/prefs.rs` | Settings file and dialog |
| `src/i18n.rs` | UI language: German source strings, English table |
| `src/update.rs` | Once-a-day check for a newer GitHub release |
| `packaging/build-deb.sh` | Linux `.deb` |
| `packaging/windows/bundle.sh` | Portable Windows zip (MSYS2 UCRT64) |
| `.github/workflows/build.yml` | CI: Linux tests and `.deb`, Windows tests and zip |

## Conventions

- Rust 2024, gtk4-rs 0.11 with `v4_14`, libadwaita 0.9 with `v1_5`. Keep to those API levels: Ubuntu 24.04 / Linux Mint 22 ship GTK 4.14.
- Platform-specific code lives behind `cfg(windows)` / `cfg(not(windows))`, preferably in `src/platform.rs`. The Linux path must never change because of Windows work. Start helper programs with `platform::command`, never bare `Command::new`, so Windows shows no console window.
- New UI strings: write them in German in the code and add the English pair to `src/i18n.rs`.
- One undo step per user action (`push_items_change`). Shape snapping is two steps: clean shape, then the raw stroke.

## Testing

- `cargo test --release` must pass (72+ tests).
- GUI tests on Linux run in Xephyr: `Xephyr :77 -screen 1400x850 &`, then `DISPLAY=:77 GDK_BACKEND=x11` with isolated `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME`. Drive with `xdotool`, capture with `import -window root`.
- Any `GRAPHIT_*` test env var makes the instance non-unique, so it never talks to the maintainer's running Graphit. Useful ones: `GRAPHIT_PROFILE`, `GRAPHIT_TOUR=<file.json>`, `GRAPHIT_CLAUDE`, `GRAPHIT_KEYLOG`, `GRAPHIT_PENLOG`.
- Never `pkill graphit`. Kill only the PIDs you started.
- The mouse starts with the select-text-and-pan tool. To draw with the mouse in a test, pick the pen in the dock first.

## Release flow

1. Commit the work (after the privacy scan).
2. Bump `version` in `Cargo.toml` and the `graphit` entry in `Cargo.lock`.
3. `cargo test --release`, `sh packaging/build-deb.sh`, scan the `.deb`.
4. Commit "Release X.Y.Z", tag `vX.Y.Z`, `git push origin main vX.Y.Z`.
5. `gh release create vX.Y.Z graphit_X.Y.Z_amd64.deb` with English notes (New / Improved / Fixed).
6. Install locally: `PATH=$HOME/.cargo/bin:$PATH ./install.sh`.

The update check compares versions numerically (0.1.10 is newer than 0.1.9). Windows builds are in progress on the `windows` branch, see `PORTING.md`.
