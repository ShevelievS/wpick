# Changelog

All notable changes to wpick are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

_Nothing yet._

---

## [0.2.0] — 2026-MM-DD

Second release. Multi-monitor, auto-pause, streaming audio, and distro
packaging.

### Added

- **Extended config schema** — `~/.config/wpick/config.toml` now
  supports `[pause]`, `[audio]`, `[monitors."<name>"]` sections and an
  `autostart` flag. Old v0.1 configs load unchanged (every new field
  has `#[serde(default)]`). See `docs/CONFIG.md` for the full reference.
- **Multi-monitor support** — each connected `wl_output` gets its own
  renderer with shared `wgpu::Device`. Hotplug (plug/unplug, resolution
  change) handled without restart. Per-output wallpaper and fit mode
  persisted in config. See `docs/MULTIMONITOR.md`.
- **New IPC commands** — `ListOutputs`, `Status`, `Pause`, `Resume`
  (manual override). `Set` gains an optional `monitor` field.
- **Auto-pause manager** — skips decode + render when any window is
  fullscreen (Hyprland IPC), when laptop is on battery, or when the
  lid is closed. All three triggers independently configurable.
  CPU usage drops from ~5% to <0.5% while paused. See `docs/PAUSE.md`.
- **Streaming audio decoder** — audio is chunked through a bounded
  channel (`chunk_frames = 2048` by default, ~42 ms per chunk at 48 kHz).
  Eliminates the ~110 MB pre-load per 5-minute track and cuts start
  latency to <500 ms.
- **Frame buffer reuse** — `VideoDecoder` now holds a reusable
  `Vec<u8>` for RGBA frames. At 1080p 30 fps this removes ~240 MB/s
  of allocator churn.
- **CLI additions** — `wpick outputs`, `wpick status`, `wpick pause`,
  `wpick resume`, `wpick set <id> --monitor <name>`.
- **Shell completions** — bash, zsh, fish generated via
  `wpick completions <shell>`. Installed by AUR and Nix packages.
- **Man pages** — `wpick.1` and `wpick-daemon.1` generated via
  `wpick man` / `wpick-daemon man` (`clap_mangen`).
- **Systemd user service** — `dist/systemd/wpick-daemon.service`.
  Hardening with `ProtectHome=read-only`, resource limits
  (`MemoryMax=500M`, `CPUQuota=50%`), journal logging via
  `$JOURNAL_STREAM` auto-detection. See `docs/SYSTEMD.md`.
- **Distro packaging** — AUR `wpick-bin` / `wpick` / `wpick-git`,
  Nix flake with `nix run github:ederadar/wpick`.
- **New docs** — `docs/CONFIG.md`, `docs/MULTIMONITOR.md`,
  `docs/PAUSE.md`, `docs/SYSTEMD.md`, `docs/INSTALL.md`.

### Changed

- `ClientCommand::Set` now carries `monitor: Option<String>`.
  **Breaking change for direct IPC consumers.** TUI and CLI
  preserve v0.1 behaviour by passing `None` when `--monitor` is
  omitted. Legacy v0.1 client payloads (`{"type":"Set","id":123}`
  without `monitor`) continue to deserialise thanks to
  `#[serde(default)]`.
- Audio pipeline no longer pre-loads entire track into RAM.
- Daemon logging auto-switches between file rotation (standalone) and
  stderr without ANSI (under systemd).
- `VideoDecoder::next_frame_rgba` now returns `Option<(&[u8], u32, u32)>`
  instead of `Option<(Vec<u8>, u32, u32)>`. Only relevant for code
  embedding the daemon as a library.

### Fixed

- Daemon no longer re-parses `WpickConfig` on every volume change
  (state now owns volume between persists).
- `seek_to_start` now calls `decoder.flush()`; first frames after a
  loop are no longer green garbage.
- Socket file reliably removed on SIGINT and SIGTERM.

### Known limitations

- Scene and Web wallpaper types still not supported (→ v0.3).
- TUI image previews not rendered (→ v0.3).
- TUI monitor selector UI missing; use the CLI's `--monitor` flag.
- Flatpak packaging blocked by `$HOME/.wpick.sock` location; v0.3
  will move the socket under `$XDG_RUNTIME_DIR` with a fallback.
- Hardware video decode (VAAPI / NVDEC) not used; all decoding is CPU.

---

## [0.1.0] — 2025-MM-DD

Initial public release.

### Added
- Three-crate Rust workspace: `wpick-core`, `wpick-daemon`, `wpick-tui`.
- Steam Wallpaper Engine library discovery via `libraryfolders.vdf`.
- PKG extraction with mtime-based caching.
- Video playback on Wayland via `wlr-layer-shell` + `wgpu` + `ffmpeg-next`.
- Audio playback via `rodio` with `libpulse-binding` ducking.
- SQLite metadata cache.
- Newline-JSON IPC over `~/.wpick.sock`.
- TUI with `ratatui`: browse list, apply wallpaper, detail view,
  volume and mute controls, daemon auto-start on first TUI launch.
- CLI: `list`, `set`, `volume`, `mute`, `info`, `daemon`, `kill`.
- AUR `PKGBUILD` for `wpick`.

### Known limitations (all resolved in v0.2 unless noted)
- Single monitor only.
- No pause mechanism — daemon renders continuously.
- Entire audio track pre-loaded to RAM.
- No shell completions or man pages.
- No systemd integration.