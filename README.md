# wpick

> Native Wayland video wallpaper daemon — plays any video file.  
> Steam Workshop library auto-detected when available. No Wine, no DRM hacks — pure Rust.

> **Disclaimer:** wpick is an independent open-source project and is not affiliated with,
> endorsed by, or sponsored by Valve Corporation or the Wallpaper Engine developers.
> "Wallpaper Engine" and "Steam" are trademarks of Valve Corporation.

[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.5.1-blue)](CHANGELOG.md)

wpick plays video files directly on `wlr-layer-shell` background surfaces,
with streaming audio, PulseAudio ducking, VA-API hardware decode,
and a ratatui TUI with image preview for browsing your library.
Steam Workshop is auto-detected — no configuration needed if you have it.

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
| Per-monitor wallpaper & fit mode | ✅ |
| Fit modes: Fit / Fill / Stretch / Center | ✅ |
| Fullscreen auto-pause (Hyprland) | ✅ |
| Competitor tool handling (SIGSTOP / SIGKILL) | ✅ |
| Wallpaper persist on restart | ✅ |
| TUI — browse, search, filter, image preview | ✅ |
| TUI source filter (Workshop / Local folders) | ✅ |
| Custom video folders (`extra_dirs`) | ✅ |
| CLI one-shot commands | ✅ |
| SQLite metadata cache | ✅ |
| Systemd user service | ✅ |
| Global hotkey (`Super+W` → TUI popup) | ✅ |
| Wallpaper timer with shuffle | ✅ |
| Favorites & Most Played | ✅ |
| Packs (named collections) | ✅ |
| Per-workspace wallpapers (Hyprland / Sway) | ✅ |
| FPS cap (`max_fps` — reduces cursor jitter) | ✅ |
| Theme presets (nord, dracula, tokyo…) | ✅ |
| Shell completions / man pages | ✅ |
| Scene wallpapers | ❌ not planned |
| Web wallpapers | ❌ not planned |

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
dnf install ffmpeg-devel pulseaudio-libs-devel wayland-devel wayland-protocols-devel
```

**Build & install:**

```bash
git clone https://github.com/ShevelievS/wpick
cd wpick
cargo build --workspace --release
install -Dm755 target/release/wpick        ~/.local/bin/wpick
install -Dm755 target/release/wpick-daemon ~/.local/bin/wpick-daemon
```

Make sure `~/.local/bin` is in your `PATH`.

`wpick-daemon` must be in `PATH` — `wpick` auto-starts it on first run.

### Autostart with systemd (recommended)

```bash
mkdir -p ~/.config/systemd/user
cp dist/systemd/wpick-daemon.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now wpick-daemon
```

Check status: `systemctl --user status wpick-daemon`  
View logs:    `journalctl --user -u wpick-daemon -f`

---

## Quick start

```bash
# Launch TUI — auto-starts daemon in background
wpick

# Wallpaper control
wpick list              # list all cached wallpapers
wpick set 1234567890    # set wallpaper by Steam Workshop ID
wpick scan              # rescan Workshop dirs and extra_dirs
wpick info 1234567890   # show wallpaper details
wpick status            # show current wallpaper, volume, mute state

# Audio
wpick volume 60         # set volume to 60 %
wpick mute              # toggle mute

# Timer (playlist rotation)
wpick timer set --ids 111 222 333 --interval 300 --shuffle
wpick timer stop
wpick timer status

# Daemon control
wpick kill              # stop daemon (requires same UID as daemon)
```

On first run `wpick` scans your Steam Workshop library automatically.
If the scan finds nothing, check that Steam is installed and your Workshop
content is under `~/.steam/steam/steamapps/workshop/content/431960/`.

---

## TUI

```
wpick
```

**Navigation**

| Key | Action |
|-----|--------|
| `↑ ↓` / `j k` | Move through wallpaper list |
| `← →` / `h l` | Switch between Nav panel and List |
| `[` | Toggle left navigation panel |
| `]` | Toggle right preview panel |
| `Tab` | Cycle source filter (All → Workshop → Local folders) |
| `/` | Live search |
| `o` | Sort dialog (Default / Name / Size / Resolution) |

**Wallpaper**

| Key | Action |
|-----|--------|
| `Enter` | Apply selected wallpaper (monitor picker if multi-monitor) |
| `f` | Cycle fit mode (Fit → Fill → Stretch → Center) |
| `*` | Toggle favorite |
| `p` | Add to pack |
| `i` | Toggle detail / full-screen view |

**Timer**

| Key | Action |
|-----|--------|
| `T` | Open timer dialog |

**Audio**

| Key | Action |
|-----|--------|
| `+ -` | Volume up / down |
| `m` | Toggle mute |

**Library & UI**

| Key | Action |
|-----|--------|
| `r` | Rescan library |
| `s` | Open folder picker — add/remove custom video folders |
| `S` | Settings dialog (theme, colors, layout) |
| `?` | Help overlay |
| `q` | Quit TUI (daemon keeps running) |
| `Q` / `Ctrl-C` | Quit TUI **and** kill daemon |
| `Esc` | Cancel active scan / close overlays |

The right panel shows an image preview of the selected wallpaper.
Protocol is auto-detected: Kitty graphics → Sixel → halfblocks fallback.

The left navigation panel shows: **Favorites**, **Frequent** (most played),
**Packs** (named collections), and source filters (Workshop / local folders).

Scans run in the background — the TUI stays responsive. Press `Esc` at any
time to cancel a running scan.

---

## Global Hotkey

wpick can open its TUI in a floating terminal popup via a global hotkey,
even when the TUI is not running.

**Setup (one-time):**

```bash
# Add yourself to the input group (re-login required)
sudo usermod -aG input $USER
```

**Config:**

```toml
[hotkey]
enabled  = true
keys     = "super+w"   # modifiers: super ctrl shift alt
terminal = ""          # auto-detected: foot → kitty → alacritty → wezterm → xterm
width    = 960
height   = 640
```

On Hyprland the popup appears as `[float;center;size 960 640]` and is
refocused if already open. On other compositors the terminal is spawned
without size hints.

---

## Shell completions

wpick can generate completion scripts for bash, zsh, and fish:

```bash
# Bash
wpick completions bash > ~/.local/share/bash-completion/completions/wpick

# Zsh (add to a directory in your $fpath)
wpick completions zsh > ~/.zfunc/_wpick
# then in ~/.zshrc, before compinit:  fpath=(~/.zfunc $fpath)

# Fish
wpick completions fish > ~/.config/fish/completions/wpick.fish
```

Restart your shell or source the file for completions to take effect.

---

## Man page

```bash
wpick man > /tmp/wpick.1 && man /tmp/wpick.1
```

---

## Configuration

Config file: `~/.config/wpick/config.toml`  
Created automatically with defaults on first run.

```toml
[general]
volume             = 0.8
muted              = false
max_fps            = 30      # frame cap — lower values reduce compositor CPU and cursor jitter
# pause_competitors = false  # true = SIGSTOP competing daemons; false (default) = SIGKILL

[audio]
ducking_enabled = true    # fade wallpaper audio when another app plays
chunk_frames    = 2048    # streaming decode chunk size (frames, ~42 ms at 48 kHz)

[pause]
on_fullscreen = true      # pause rendering when a fullscreen window is active
on_battery    = false     # pause on battery power
on_lid_close  = false     # pause when laptop lid is closed

[paths]
# Extra directories scanned for local video files (mp4, webm, mkv, avi, mov, …)
# Symlinks are NOT followed — use real paths.
extra_dirs = [
    "/home/user/Videos/wallpapers",
    "/mnt/nas/wallpapers",
]

[hotkey]
enabled  = true           # requires: sudo usermod -aG input $USER + re-login
keys     = "super+w"      # modifiers: super ctrl shift alt + key
terminal = ""             # auto-detected if empty
width    = 960
height   = 640

[tui]
theme    = "dark"         # dark | nord | dracula | tokyo | forrest | deep
windowed = false          # render TUI in a centered 82×82% sub-area
surface_reassert_secs = 0 # delay before Wayland surface reinit on startup
                          # set >0 (e.g. 12) only if QuickShell starts after wpick

# Per-monitor wallpaper (key = wl_output name, e.g. "eDP-1", "HDMI-A-1")
[monitors."eDP-1"]
wallpaper_id = 1234567890
fit          = "fill"     # fit | fill | stretch | center
mute         = false      # mute audio on this monitor

# Per-workspace wallpapers — Hyprland workspace name → wallpaper ID.
# When you switch workspace the assigned wallpaper is applied on the focused monitor.
# Set id = 0 to clear a mapping. Works with both Hyprland and Sway.
[workspace_wallpapers]
"1" = 1234567890
"2" = 9876543210
```

### Fit modes

| Mode | Description |
|------|-------------|
| `fit` | Scale to fit inside screen — letterbox/pillarbox borders |
| `fill` | Scale to fill screen — center-crops overflow |
| `stretch` | Stretch to fill — ignores aspect ratio |
| `center` | No scaling — 1:1 pixels, centered, black borders if smaller |

### Custom video folders

Add any directory to `[paths] extra_dirs` in the config, or use the TUI folder
picker (`s` key) to browse the filesystem and add/remove directories interactively.
Local files are assigned stable IDs based on their path and appear in the TUI
under their folder name in the source filter (`Tab`).

The folder picker shows a colour-coded badge for each directory:

| Badge | Colour | Meaning |
|-------|--------|---------|
| `[V]` | green  | Contains video files directly — good candidate |
| `[·]` | yellow | No direct videos, but has sub-directories |
| `[-]` | gray   | Empty |
| `[?]` | gray   | Permission denied |
| `[!]` | red    | System path (`/proc`, `/sys`, `/dev`, `/run`) — blocked |

Scan depth is limited to 6 levels, so adding a large directory (e.g. Downloads)
will not cause a runaway scan.

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
  └── Unix socket ($XDG_RUNTIME_DIR/wpick.sock) ──► wpick-daemon
                                                         ├── renderer      (wlr-layer-shell + wl_shm)
                                                         │     ├── HwDecoder    (VA-API → NV12 → BGRA)
                                                         │     └── VideoDecoder (swscale → BGRA)
                                                         ├── audio task    (rodio + streaming ffmpeg)
                                                         │     └── DuckHandle   (PulseAudio ducking)
                                                         ├── hotkey task   (evdev /dev/input/*)
                                                         └── IPC server    (JSON-newline, UID-auth Kill)
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
`workspace>>` and `workspacev2>>` (Hyprland v0.40+) events trigger
per-workspace wallpaper switches and fullscreen state queries.

**Frame pacing:**  
`max_fps` (default 30) caps the committed-frame rate regardless of video FPS.
A drift-clamp anchors `next_frame` to wall clock after each commit to prevent
render-loop spin accumulation over long runtimes. Together these eliminate
cursor jitter on high-refresh-rate displays after extended wallpaper playback.

---

## Development

```bash
cargo test   --workspace          # 78 tests, 0 failures expected
cargo clippy --workspace -- -D warnings
cargo build  --workspace --release
```

| Crate | Purpose |
|-------|---------|
| `wpick-core` | Shared types: config, model, IPC protocol, cache, discovery |
| `wpick-daemon` | Renderer + audio + IPC server |
| `wpick-tui` | TUI browser + CLI (`wpick` binary) |

---

## Roadmap

### v0.5.1 — Polish (planned)

| Item | Description |
|------|-------------|
| PulseAudio reconnect | Exponential backoff reconnect loop after ducking socket disconnects |
| Fill mode center-crop | True center-of-frame crop for Fill mode (currently crops from top-left) |
| Workspace crossfade | Crossfade transition on workspace wallpaper switches (currently hard-cut) |

### v0.6 — GPU render path (planned)

| Item | Description |
|------|-------------|
| DMA-BUF / wl_drm | Zero-copy GPU frame delivery — eliminates wl_shm CPU upload entirely |
| EGL surface | Optional `wl_egl_surface` path for hardware-accelerated frame delivery |

---

## License

wpick source code is licensed under the [MIT License](LICENSE) © 2026 ShevelievS.

This binary links dynamically against:
- **FFmpeg** (`ffmpeg-next` / `ffmpeg-sys-next`) — [LGPL 2.1+](https://ffmpeg.org/legal.html)
- **PulseAudio** (`libpulse-binding`) — [LGPL 2.1](https://www.freedesktop.org/wiki/Software/PulseAudio/)

These libraries are not modified. Their licenses apply to their respective source code.
