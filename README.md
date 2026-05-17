# wpick

> Native Wayland live wallpaper daemon for Wallpaper Engine (Steam) content.  
> No Wine, no `linux-wallpaperengine`, no DRM hacks — pure Rust.

> **Disclaimer:** wpick is an independent open-source project and is not affiliated with,
> endorsed by, or sponsored by Valve Corporation or the Wallpaper Engine developers.
> "Wallpaper Engine" and "Steam" are trademarks of Valve Corporation.

[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.4.2-blue)](CHANGELOG.md)

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
Workspace switches query `j/activeworkspace` so moving to/from a fullscreen
workspace also triggers the correct pause/resume.

---

## Development

```bash
cargo test   --workspace          # ~70 tests, 0 failures expected
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

### v0.4.3 — Stability & performance (planned)

| Item | Description |
|------|-------------|
| Config write safety | Replace per-command `load → save` pattern with a single `Arc<Mutex<WpickConfig>>` + debounced writer — eliminates silent data loss when two IPC commands fire concurrently |
| IPC timeout resilience | Structured backoff on Hyprland socket reconnect (currently flat 2 s sleep) |
| Batch SQLite scan | Wrap all `upsert` calls in a single transaction — 10-20× faster scan on large libraries |
| `prune()` chunking | Split `WHERE id NOT IN (…)` into chunks of 999 to avoid SQLite variable limit |
| VA-API device discovery | Glob `/dev/dri/renderD*` instead of hardcoding `renderD128`/`renderD129`; fixes AMD APU and multi-GPU setups |
| `has_audio` probe | Detect audio tracks in local video files at scan time via ffmpeg stream inspection |
| IPC server tests | Unit tests for each `dispatch()` arm with mocked state and cache |

### v0.5 — Architecture (planned)

| Item | Description |
|------|-------------|
| Typed renderer errors | Replace `__reassert__`/`__fatal__` string matching with a proper `RendererSignal` enum |
| Renderer trait abstraction | Extract `Decoder` trait + headless implementation for unit-testable renderer logic |
| Rust 2024 Edition | MSRV bump, all three crates updated |
| EGL/GPU render path | Optional wl_egl_surface path for hardware-accelerated frame delivery (prerequisite for GPU transitions) |

---

## License

wpick source code is licensed under the [MIT License](LICENSE) © 2026 ShevelievS.

This binary links dynamically against:
- **FFmpeg** (`ffmpeg-next` / `ffmpeg-sys-next`) — [LGPL 2.1+](https://ffmpeg.org/legal.html)
- **PulseAudio** (`libpulse-binding`) — [LGPL 2.1](https://www.freedesktop.org/wiki/Software/PulseAudio/)

These libraries are not modified. Their licenses apply to their respective source code.
