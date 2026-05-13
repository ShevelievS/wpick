# Changelog

All notable changes to wpick are documented here.  
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).  
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.3.0] — 2026-05-12

### Added

- **TUI image preview** — `ratatui-image` v11 renders wallpaper thumbnails
  in the right panel. Protocol auto-detected at startup: Kitty graphics →
  Sixel → halfblocks unicode fallback. Preview reloads on selection change;
  encoded data cached in `StatefulProtocol` so resizes are the only
  re-encode cost. (`wpick-tui`)

- **Competitor wallpaper tool handling** — on startup wpick scans `/proc`
  for known competing daemons (hyprpaper, swww-daemon, swaybg, mpvpaper,
  wpaperd, feh, nitrogen, xwallpaper).
  - Default (`pause_competitors = false`): SIGTERM + 500 ms grace → SIGKILL.
    A watchdog re-kills any process restarted by the shell (5 s interval).
  - Safe mode (`pause_competitors = true`): SIGSTOP freezes the process;
    the shell sees it as alive and does not restart it. wpick sends SIGCONT
    on clean exit. Recommended for QuickShell / rice setups. (`wpick-daemon`)

- **Fullscreen recovery** — background thread monitors Hyprland's `socket2`
  event stream. `fullscreen>>1` / `fullscreen>>0` events pause/resume
  frame delivery. On exit the surface is recreated after 300 ms to restore
  z-order above any competitor restarted by the shell. (`wpick-daemon`)

- **Multi-workspace fullscreen detection** — `workspace>>` and
  `focusedmon>>` events query `j/activeworkspace` for `hasfullscreen` so
  that switching to/from a workspace with a fullscreen window is handled
  correctly, without corrupting the `fullscreen>>` state machine.
  (`wpick-daemon`)

- **Frame callback recovery** — when the compositor stops delivering frame
  callbacks (e.g. surface hidden behind a fullscreen window), a 300 ms
  timeout forces `frame_ready = true` so the render loop does not stall.
  (`wpick-daemon`)

- **Multi-monitor support** — each connected `wl_output` gets its own
  `wl_surface` and renderer. Hotplug (connect, disconnect, resolution
  change) handled without daemon restart. Per-output wallpaper and fit
  mode configurable in `[monitors."<name>"]`. (`wpick-daemon`)

- **Extended config** — `~/.config/wpick/config.toml` now supports
  `[pause]`, `[audio]`, `[monitors."<name>"]`, `pause_competitors`, and
  `autostart` fields. Old configs load unchanged (all new fields have
  `#[serde(default)]`). (`wpick-core`)

- **Scan progress streaming** — `Scan` IPC command streams `ScanProgress`
  responses while scanning; TUI shows live `Scanning… N/total` counter.
  (`wpick-core`, `wpick-daemon`, `wpick-tui`)

- **IPC `Status` command** — returns current wallpaper id, volume, muted
  state. TUI syncs on reconnect. (`wpick-core`, `wpick-daemon`, `wpick-tui`)

- **Streaming audio decoder** — audio is delivered in configurable chunks
  (`chunk_frames`, default 2048 ≈ 42 ms at 48 kHz stereo) through a
  bounded channel. Eliminates full-track RAM pre-load; start latency < 500 ms.
  (`wpick-daemon`)

- **Frame buffer reuse** — `VideoDecoder` holds a reusable `Vec<u8>` for
  decoded frames, removing ~240 MB/s of allocator churn at 1080p 30 fps.
  (`wpick-daemon`)

### Changed

- Renderer replaced `wgpu` / Vulkan with CPU `wl_shm` shared-memory
  buffers. Eliminates the GPU context dependency while keeping VA-API for
  hardware decode (NV12 → BGRA CPU conversion). (`wpick-daemon`)
- TUI right panel redesigned: preview area now fills all available height
  (was fixed 40 %); Details block fixed at 7 rows with compact one-line
  metadata (title + type / audio / size / id). (`wpick-tui`)
- `ratatui` upgraded 0.29 → 0.30; `crossterm` 0.28 → 0.29. (`wpick-tui`)

### Fixed

- **Wayland event loop** — `dispatch_pending` in `wayland-client` 0.31
  does not read from the socket; added explicit `conn.prepare_read() +
  guard.read()` before each dispatch, fixing a black-screen bug where only
  the first frame was rendered.
- **Hyprland socket path** — ≥ 0.30 moved the IPC socket to
  `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`; path now resolved
  dynamically with fallback to `/tmp/hypr/`.
- **Wrong JSON field** — fullscreen query used `fullscreenmode` but
  Hyprland returns `hasfullscreen`.
- `seek_to_start` now flushes the decoder; first frames after a loop are
  no longer corrupt.
- Socket file reliably removed on SIGINT and SIGTERM.

### Known limitations

- Scene and Web wallpaper types are parsed and displayed in the TUI but
  not rendered.
- Wallpaper selection is not persisted across daemon restarts (fixed in v0.4.0).
- Audio does not pause on `on_battery` / `on_lid_close` / fullscreen — only
  frame rendering is suspended (architectural constraint of the streaming decoder).
- No systemd user service, shell completions, or man pages yet (planned v0.4.0).

---

## [0.1.0] — 2025-05-01

Initial public release.

### Added

- Three-crate Rust workspace: `wpick-core`, `wpick-daemon`, `wpick-tui`.
- Steam Wallpaper Engine library discovery via `libraryfolders.vdf`.
- PKG extraction with mtime-based caching.
- Video playback on Wayland via `wlr-layer-shell` + `wgpu` + `ffmpeg-next`.
- Audio playback via `rodio` with `libpulse-binding` ducking.
- SQLite metadata cache (`rusqlite` bundled).
- Newline-JSON IPC over `~/.wpick.sock`.
- TUI with `ratatui`: browse list, apply wallpaper, detail view,
  volume and mute controls, daemon auto-start.
- CLI: `list`, `set`, `volume`, `mute`, `info`, `scan`, `daemon`, `kill`.
- AUR `PKGBUILD`.
