# wpick

Native Wayland live wallpaper manager for Wallpaper Engine (Steam)

[![Build](https://github.com/ederadar/wpick/actions/workflows/ci.yml/badge.svg)](https://github.com/ederadar/wpick/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![AUR version](https://img.shields.io/aur/version/wpick)](https://aur.archlinux.org/packages/wpick)

---

## What it does

Plays Wallpaper Engine video wallpapers natively on Wayland — no Wine, no linux-wallpaperengine. Parses your Steam library, decodes video via ffmpeg, renders on a wlr-layer-shell background surface, and plays audio via PipeWire or ALSA.

## Features

- Auto-discovers Steam library (native, Flatpak, and Snap installs)
- Video wallpapers with audio (AAC/MP3)
- Audio ducking — fades out when other apps play sound
- Global mute and volume control
- SQLite metadata cache (instant startup after first scan)
- Terminal UI (ratatui) and CLI
- Wayland only: Hyprland, Sway, river, niri

## Requirements

- Rust 1.78+
- Wayland compositor with wlr-layer-shell support
- ffmpeg (system library)

```bash
sudo pacman -S ffmpeg
```

- Steam with Wallpaper Engine (App ID 431960) installed
- PipeWire or PulseAudio

## Installation

**Option 1 — AUR**

```bash
yay -S wpick
```

**Option 2 — cargo install**

```bash
cargo install --git https://github.com/ederadar/wpick wpick-tui
cargo install --git https://github.com/ederadar/wpick wpick-daemon
```

**Option 3 — build from source**

```bash
git clone https://github.com/ederadar/wpick
cd wpick
cargo build --workspace --release
sudo cp target/release/wpick /usr/local/bin/
sudo cp target/release/wpick-daemon /usr/local/bin/
```

## Usage

```bash
wpick              # start daemon + TUI
wpick tui          # TUI only (daemon must be running)
wpick list         # list wallpapers
wpick set <id>     # set wallpaper by ID
wpick volume 60    # set volume 0-100
wpick mute         # toggle mute
wpick kill         # stop daemon
```

### TUI keybindings

| Key | Action |
|-----|--------|
| `↑↓` / `j` `k` | Navigate |
| `Enter` | Apply wallpaper |
| `+` / `-` | Volume |
| `m` | Toggle mute |
| `r` | Refresh list |
| `i` | Toggle detail view |
| `q` | Quit TUI (daemon keeps running) |
| `Q` | Kill daemon |

## Architecture

| Crate | Role |
|-------|------|
| `wpick-core` | Library: Steam discovery, PKG parsing, SQLite cache, IPC types |
| `wpick-daemon` | Background process: Wayland renderer, audio, IPC server |
| `wpick-tui` | Terminal UI and CLI (binary: `wpick`) |

## Limitations (v0.1)

- Video wallpapers only — Scene and Web types not yet supported
- Single monitor — multi-monitor planned for v0.2
- Preview images in TUI not yet rendered — planned for v0.2
- Tested on: Arch Linux, Hyprland, NVIDIA RTX 3050 Ti, PipeWire 1.6.3

## License

MIT
