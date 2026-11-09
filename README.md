# Graphit

Notiz-App für Stift und Touchscreen unter Linux, geschrieben in Rust und GTK4.
Öffnet und speichert Xournal++-Dateien (`.xopp`) und annotiert PDFs.

A pen-first note-taking app for Linux, compatible with Xournal++ files. The interface is in German.

## Funktionen

- Stift, Marker, Radierer, Formen, Text, Bilder, Radialmenü
- Notizen und PDFs in Tabs, zwei Lese-Viewer daneben
- Touch-Gesten mit Handballen-Erkennung
- Optional: Claude als Lernhilfe, die auf der Seite zeigt, abfragt und einträgt

## Installation

Für Linux Mint 22, Ubuntu 24.04 oder neuer: das `.deb` aus den Releases laden, dann

```sh
sudo apt install ./graphit_*_amd64.deb
```

Beim ersten Start wählst du deinen Notizordner.

## Optionale Helfer

| Funktion | Nötig |
|---|---|
| KI-Funktionen | [Claude Code](https://claude.com/claude-code), installiert und angemeldet, mit eigenem Konto |
| Formeln in Antworten | `pdflatex` und `pandoc`, werden als empfohlene Pakete mitinstalliert |
| Natürliche Vorlesestimme | `/usr/share/graphit/install-stimme.sh` lädt Piper und eine deutsche Stimme, etwa 90 MB |

## Selbst bauen

```sh
sudo apt install build-essential dpkg-dev libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev
cargo build --release
./packaging/build-deb.sh
```

## Lizenz

GPL-3.0, siehe `LICENSE`.
