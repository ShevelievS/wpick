# Changelog

## [0.1.0] - 2026-04-18

### Added

- Auto-discovery of Steam library (native, Flatpak, Snap)
- Video wallpaper playback on Wayland via wlr-layer-shell
- Audio playback with AAC/MP3 support
- Audio ducking — fade out when other apps play sound
- Global mute and volume control via PipeWire/ALSA
- SQLite metadata cache with mtime-based invalidation
- Terminal UI (ratatui): wallpaper list, detail panel, volume bar
- CLI subcommands: `list`, `set`, `volume`, `mute`, `info`, `kill`
- Unix socket IPC between daemon and TUI
- Automatic daemon startup when running `wpick` without arguments

### Known limitations

- Video wallpapers only — Scene and Web types planned for v0.2
- Single monitor support — multi-monitor planned for v0.2
- Preview images in TUI not yet rendered — planned for v0.2
