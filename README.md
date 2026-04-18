# wpick

A native Wayland live wallpaper manager for Wallpaper Engine (Steam) content, written in Rust.

> **Status**: Phase 3 complete — IPC daemon working. Renderer in progress.

## What it does

Plays Wallpaper Engine video wallpapers natively on Wayland without Wine or linux-wallpaperengine.
Parses Steam library structure, extracts `.pkg` archives, decodes video via ffmpeg,
renders frames as GPU textures on a `wlr-layer-shell` background surface.

## Architecture
wpick-core    — library: Steam discovery, PKG parsing, SQLite cache, IPC protocol
wpick-daemon  — background process: Wayland renderer, audio playback, IPC server
wpick-tui     — terminal UI: wallpaper browser, keyboard control

## Status

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | Foundation: config, error types, models | ✅ Done |
| 2 | Data pipeline: Steam discovery, PKG extraction, SQLite cache, IPC types | ✅ Done |
| 3 | Daemon core: IPC server, Unix socket, signal handling | ✅ Done |
| 4 | Media: video decode (ffmpeg), audio (rodio), Wayland renderer (wgpu) | ✅ Done |
| 5 | TUI: ratatui interface | ✅ Done |
| 6 | Integration & release | ⬜ Planned |

## Requirements

- Rust 1.78+
- Wayland compositor with `wlr-layer-shell` support (Hyprland, Sway, river...)
- System ffmpeg: `sudo pacman -S ffmpeg`
- Steam with Wallpaper Engine installed

## Build

```bash
# Install system dependencies (Arch)
sudo pacman -S ffmpeg wayland wayland-protocols pkgconf

# Build
cargo build --workspace

# Run daemon
cargo run -p wpick-daemon

# Run TUI (in another terminal)
cargo run -p wpick-tui
```

## License

MIT
EOF
