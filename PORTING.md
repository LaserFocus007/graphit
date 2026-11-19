# Windows port (target: 0.2.0)

Goal: one code base and one release with two downloads, the Linux `.deb` and a Windows build. Linux must never break because of Windows work. Everything here happens on the `windows` branch until Windows runs well, then it is merged into `main` for 0.2.0.

## Done on Linux

- `src/platform.rs`: helper processes start without a console window on Windows (`platform::command`), and processes are killed with `taskkill /T` instead of `kill`.
- Claude Code: on Windows only the real `claude.exe` is used (native installer in `%USERPROFILE%\.local\bin`, or on `PATH`). An npm `claude.cmd` wrapper cannot take the long prompt arguments safely, so it is ignored.
- `pdflatex` and `pandoc` start through `platform::command`.
- Reading aloud on Windows: Piper writes a WAV file and PowerShell plays it (no `aplay`). Without Piper the built-in Windows voice (System.Speech) speaks, in the text's language if such a voice is installed. Voices install with `tools/install-voice.ps1` into Graphit's data folder.
- Release builds on Windows have no console window (`windows_subsystem = "windows"`), debug builds keep it for log output.
- `packaging/windows/bundle.sh` builds a portable folder with the GTK runtime and zips it.
- CI (`.github/workflows/build.yml`) builds and tests Linux and Windows on every push. The Windows zip is attached to the workflow run as the artifact `graphit-windows`.

Data and settings use glib's folders, which map to `%LOCALAPPDATA%` and friends on Windows without code changes.

## Build on Windows

1. Install MSYS2 from msys2.org and open the **MSYS2 UCRT64** shell.
2. Install the toolchain and libraries:
   ```sh
   pacman -S --needed git zip mingw-w64-ucrt-x86_64-{gtk4,libadwaita,poppler,rust,pkgconf,gcc,adwaita-icon-theme}
   ```
3. `git clone https://github.com/LaserFocus007/graphit && cd graphit && git switch windows`
4. Quick run during development: `cargo run --release -- <file.xopp>`
5. Portable build: `sh packaging/windows/bundle.sh`, then start `dist/Graphit/bin/graphit.exe`.

Optional helpers on Windows: Claude Code (native installer, then `claude` once to sign in), MiKTeX for `pdflatex`, pandoc from pandoc.org.

## To test on the Windows device

Most important first. The pen is the biggest risk because GTK talks to a different input API on Windows.

- [ ] Pen: pressure, tilt, eraser end, barrel buttons, hover cursor, low latency
- [ ] Palm rejection and two-finger pan and zoom on the touchscreen
- [ ] Mouse: text selection on PDFs, text bar, pan, right click opens the radial menu
- [ ] Open and save `.xopp` files made on Linux, and back (paths to background PDFs)
- [ ] PDF rendering and fonts, PDF export
- [ ] Pasting screenshots from the clipboard (transparent background)
- [ ] Study companion: Claude Code found, answers stream, walkthrough cards, Esc
- [ ] Formulas with MiKTeX and pandoc
- [ ] Reading aloud: system voice, then install natural voices from the settings
- [ ] Opening a file while Graphit runs (single instance may not apply on Windows, see below)
- [ ] Dark paper, library thumbnails, search, tabs, reading panes

## Still open

- **Installer.** The zip is portable. For 0.2.0 an installer (Inno Setup or MSIX) should add a Start menu entry, the app icon and the `.xopp` file association.
- **Release asset.** Attach the Windows zip or installer to the GitHub release next to the `.deb`, and point the in-app update hint at the right download.
- **Single instance.** On Linux a second launch hands the file to the running window over D-Bus. Check what GApplication does on Windows and fix if a second window opens.
- **Online accounts.** GNOME Online Accounts (`src/drive.rs`) do not exist on Windows. The code only recognizes gvfs paths, so it simply stays inactive. A OneDrive or Google Drive desktop folder works as a normal folder.
- **On-screen keyboard.** `claude.rs` toggles the GNOME screen keyboard through a GSettings schema that is missing on Windows. The lookup is guarded, so nothing happens there. Windows shows its touch keyboard by itself.
- **Pen latency tools** in `tools/` are Linux-only and not part of the Windows build.
- **README.** Add Windows to the install section once the build is ready.
