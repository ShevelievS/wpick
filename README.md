# wpick

> Native Wayland live wallpaper daemon for Wallpaper Engine (Steam) content.
> No Wine, no `linux-wallpaperengine`, no DRM hacks.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green)](LICENSE)

wpick plays Wallpaper Engine `.pkg` video wallpapers directly on
`wlr-layer-shell` background surfaces, with streaming audio, PulseAudio
ducking, hardware-accelerated decode (VA-API), and a ratatui TUI for
browsing your Steam Workshop library.

---

## Status — v0.1.x (current)

| Feature | State |
|---------|-------|
| Video wallpapers (H.264 / VP9 / AV1 / …) | ✅ |
| Hardware decode — VA-API → NV12 | ✅ |
| Software decode fallback (swscale → RGBA) | ✅ |
| Streaming audio (low RAM, no pre-load) | ✅ |
| PulseAudio ducking (fade out on foreign audio) | ✅ |
| Volume / mute control | ✅ |
| ratatui TUI — browse, search, filter | ✅ |
| CLI one-shot commands | ✅ |
| SQLite metadata cache | ✅ |
| Single monitor | ✅ |
| Multi-monitor | 🔜 v0.2.0 |
| Wallpaper persist on restart | 🔜 v0.2.0 |
| Pause / Resume command | 🔜 v0.2.0 |
| Auto-pause (fullscreen, battery, lid) | 🔜 v0.2.0 |
| Systemd user service | 🔜 v0.2.0 |
| Scene / Web wallpapers | ❌ v0.3 |
| TUI image preview | ❌ v0.3 |

**Compositor requirements:** `wlr-layer-shell` (Hyprland, Sway, river, niri).
GNOME and KDE are not supported.

Tested on:

- Arch Linux + Hyprland + Intel UHD (ADL GT2, VA-API confirmed)
- Arch Linux + Hyprland + AMD RDNA3
- Arch Linux + Sway + AMD RDNA3

---

## Install

### From source (recommended until AUR/Nix packages are updated)

**Dependencies:**

```bash
# Arch
pacman -S ffmpeg vulkan-icd-loader vulkan-validation-layers \
           libpulse wayland wayland-protocols

# Fedora / RHEL
dnf install ffmpeg-devel vulkan-loader libpulse-devel wayland-devel
```

**Build:**

```bash
git clone https://github.com/ederadar/wpick
cd wpick
cargo build --workspace --release
sudo install -Dm755 target/release/wpick        /usr/local/bin/wpick
sudo install -Dm755 target/release/wpick-daemon /usr/local/bin/wpick-daemon
```

`wpick-daemon` must be in `PATH` — `wpick` auto-starts it when needed.

---

## Quick start

```bash
# Launch TUI (auto-starts daemon in background on first run)
wpick

# Or use the CLI directly
wpick list              # list all wallpapers in cache
wpick set 1234567890    # set wallpaper by ID
wpick volume 60         # set volume to 60%
wpick mute              # toggle mute
wpick info 1234567890   # show wallpaper details
wpick kill              # stop daemon
```

On first run, press `r` in the TUI (or run `wpick list` after a fresh start)
to trigger a scan of your Steam Workshop directory.

---

## Usage — TUI

```
wpick
```

| Key | Action |
|-----|--------|
| `↑/↓` or `j/k` | Navigate list |
| `Enter` | Apply selected wallpaper |
| `+` / `-` | Volume up / down |
| `m` | Toggle mute |
| `r` | Rescan wallpaper library |
| `/` | Live search |
| `Tab` | Cycle type filter (All / Video / Scene / Web) |
| `i` | Toggle detail / full-screen view |
| `q` | Quit TUI (daemon keeps running) |
| `Q` or `Ctrl-C` | Quit TUI **and** kill daemon |

---

## Configuration

Config file: `~/.config/wpick/config.toml`

Created automatically on first run with defaults. Example:

```toml
[general]
volume = 0.8
muted  = false

[audio]
ducking_enabled = true   # fade out when another app plays sound
chunk_frames    = 8192   # streaming decode buffer size

[pause]
on_fullscreen = true     # auto-pause when fullscreen app detected (v0.2)
on_battery    = false    # auto-pause on battery (v0.2)
on_lid_close  = false    # auto-pause on lid close (v0.2)
```

---

## Architecture

```
wpick (TUI + CLI)
  └── Unix socket → wpick-daemon
                      ├── renderer task  (Wayland layer-shell + wgpu/Vulkan)
                      │     ├── HwDecoder  (VA-API → CPU NV12 → Y+UV upload)
                      │     └── VideoDecoder (ffmpeg SW → RGBA upload)
                      ├── audio task     (rodio + streaming ffmpeg decoder)
                      │     └── DuckHandle (PulseAudio polling, fade in/out)
                      └── IPC server     (JSON-newline over Unix socket)
```

Rendering pipeline:

- **HW path (VA-API):** `ffmpeg VA-API` → CPU-side NV12 → wgpu `R8Unorm` (Y) +
  `Rg8Unorm` (UV) textures → BT.709 WGSL fragment shader → Vulkan swapchain.
  CPU upload bandwidth ~93 MB/s @ 1080p30.
- **SW path (fallback):** `ffmpeg + swscale` → RGBA → wgpu `Rgba8UnormSrgb`
  texture → identity WGSL shader. Used when VA-API is unavailable or fails.

---

## Known limitations (v0.1.x)

- Single monitor only — multi-monitor support is the primary v0.2.0 goal.
- Wallpaper selection is not persisted across daemon restarts.
- No pause / resume command yet.
- `FitMode` (Fill / Fit / Stretch / Center) is parsed from config but not
  yet applied by the renderer — all wallpapers render as stretched fill.

---

## Contributing

wpick is MIT OR Apache-2.0. Bug reports via GitHub Issues.

Before submitting a PR:

```bash
cargo test   --workspace
cargo clippy --workspace -- -D warnings
cargo build  --workspace --release
```

Any new bug hit during development → entry in
[ERRORS_TO_AVOID.md](ERRORS_TO_AVOID.md) before the fix is written.

---

## License

Dual-licensed under MIT or Apache-2.0, at your option. See [LICENSE](LICENSE).
