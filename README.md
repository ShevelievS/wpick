# wpick

> Native Wayland live wallpaper daemon for Wallpaper Engine (Steam) content.  
> No Wine, no `linux-wallpaperengine`, no DRM hacks — pure Rust.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.3.0-blue)](CHANGELOG.md)

wpick plays Wallpaper Engine video wallpapers directly on `wlr-layer-shell`
background surfaces, with streaming audio, PulseAudio ducking,
VA-API hardware decode, and a ratatui TUI with image preview for browsing
your Steam Workshop library.

---

## Features

| Feature | State |
|---------|-------|
| Video wallpapers (H.264 / VP9 / AV1 / …) | ✅ |
| Hardware decode — VA-API | ✅ |
| Software decode fallback (swscale) | ✅ |
| Streaming audio | ✅ |
| PulseAudio ducking (fade on foreign audio) | ✅ |
| Volume / mute control | ✅ |
| Multi-monitor support | ✅ |
| Fullscreen auto-pause (Hyprland) | ✅ |
| Competitor tool handling (SIGSTOP / SIGKILL) | ✅ |
| TUI — browse, search, filter, image preview | ✅ |
| CLI one-shot commands | ✅ |
| SQLite metadata cache | ✅ |
| Per-monitor config | ✅ |
| Scene wallpapers | ❌ planned |
| Web wallpapers | ❌ planned |
| Wallpaper persist on restart | ❌ planned |
| Systemd user service | ❌ planned |
| Shell completions / man pages | ❌ planned |

**Compositor requirements:** `wlr-layer-shell` (Hyprland, Sway, river, niri).  
GNOME and KDE are **not** supported.

Tested on:

- Arch Linux + Hyprland + Intel UHD (ADL GT2, VA-API confirmed)
- Arch Linux + Hyprland + AMD RDNA3
- Arch Linux + Sway + AMD RDNA3

---

## Install

### From source

**Dependencies:**

```bash
# Arch
pacman -S ffmpeg libpulse wayland wayland-protocols

# Fedora / RHEL
dnf install ffmpeg-devel libpulse-devel wayland-devel
```

**Build & install:**

```bash
git clone https://github.com/ShevelievS/wpick
cd wpick
cargo build --workspace --release
sudo install -Dm755 target/release/wpick        /usr/local/bin/wpick
sudo install -Dm755 target/release/wpick-daemon /usr/local/bin/wpick-daemon
```

`wpick-daemon` must be in `PATH` — `wpick` auto-starts it on first run.

---

## Quick start

```bash
# Launch TUI — auto-starts daemon in background
wpick

# CLI one-shot commands
wpick list              # list all cached wallpapers
wpick set 1234567890    # set wallpaper by Steam Workshop ID
wpick volume 60         # set volume to 60 %
wpick mute              # toggle mute
wpick info 1234567890   # show wallpaper details
wpick status            # show current wallpaper, volume, mute state
wpick scan              # rescan Steam Workshop dirs
wpick kill              # stop daemon
```

On first run `wpick` scans your Steam Workshop library automatically.
If the scan finds nothing, check that Steam is installed and your Workshop
content is under `~/.steam/steam/steamapps/workshop/content/431960/`.

---

## TUI

```
wpick
```

| Key | Action |
|-----|--------|
| `↑ ↓` / `j k` | Navigate list |
| `Enter` | Apply selected wallpaper |
| `+ -` | Volume up / down |
| `m` | Toggle mute |
| `r` | Rescan library |
| `/` | Live search |
| `Tab` | Cycle type filter (All → Video → Scene → Web) |
| `i` | Toggle detail / full-screen view |
| `q` | Quit TUI (daemon keeps running) |
| `Q` / `Ctrl-C` | Quit TUI **and** kill daemon |

The right panel shows an image preview of the selected wallpaper.
Protocol is auto-detected: Kitty graphics → Sixel → halfblocks fallback.

---

## Configuration

Config file: `~/.config/wpick/config.toml`  
Created automatically with defaults on first run.

```toml
[general]
volume             = 0.8
muted              = false
# pause_competitors = false  # true = SIGSTOP competing wallpaper tools instead of SIGKILL

[audio]
ducking_enabled = true    # fade out wallpaper audio when another app plays sound
chunk_frames    = 2048    # streaming decode buffer (frames)

[pause]
on_fullscreen = true      # pause when a fullscreen window is detected (Hyprland only)
on_battery    = false     # not yet implemented
on_lid_close  = false     # not yet implemented

# Per-monitor wallpaper (key = wl_output name, e.g. "eDP-1", "HDMI-A-1")
[monitors."eDP-1"]
wallpaper_id = 1234567890
fit          = "stretch"  # stretch | fit | fill | center
```

### Competing wallpaper tools

If you use a desktop rice with a built-in wallpaper daemon (e.g. mpvpaper
via QuickShell), set:

```toml
[general]
pause_competitors = true
```

This suspends (SIGSTOP) rather than kills the competing process, so your
shell does not restart it in a loop. wpick will resume it on exit.

---

## Architecture

```
wpick (TUI + CLI binary)
  └── Unix socket (~/.wpick.sock) ──► wpick-daemon
                                          ├── renderer     (wlr-layer-shell + wl_shm)
                                          │     ├── HwDecoder    (VA-API → NV12 → BGRA)
                                          │     └── VideoDecoder (swscale → BGRA)
                                          ├── audio task   (rodio + streaming ffmpeg)
                                          │     └── DuckHandle   (PulseAudio ducking)
                                          └── IPC server   (JSON-newline / Unix socket)
```

**Rendering pipeline:**

- **HW path (VA-API):** ffmpeg VA-API decode → CPU-side NV12 copy →
  YUV-to-BGRA conversion → `wl_shm` buffer upload per frame.
- **SW path (fallback):** ffmpeg + swscale → BGRA → `wl_shm` upload.
  Used when VA-API is unavailable or fails at runtime.

**Fullscreen handling (Hyprland):**  
A background thread connects to Hyprland's `socket2` event stream.
On `fullscreen>>1` the compositor stops delivering frame callbacks; the
daemon detects this via a 300 ms timeout and keeps the render loop alive.
On `fullscreen>>0` the surface is recreated to restore correct z-order.
Workspace switches query `j/activeworkspace` so moving to/from a fullscreen
workspace also triggers the correct pause/resume.

---

## Development

```bash
cargo test   --workspace          # 64 tests, 0 failures expected
cargo clippy --workspace -- -D warnings
cargo build  --workspace --release
```

| Crate | Purpose |
|-------|---------|
| `wpick-core` | Shared types: config, model, IPC protocol, cache, discovery |
| `wpick-daemon` | Renderer + audio + IPC server |
| `wpick-tui` | TUI browser + CLI (`wpick` binary) |

---

## License

Dual-licensed under MIT or Apache-2.0, at your option. See [LICENSE](LICENSE).
