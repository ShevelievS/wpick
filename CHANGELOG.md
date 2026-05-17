# Changelog

All notable changes to wpick are documented here.  
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).  
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.4.2] — 2026-05-17

### Added

- **Global hotkey** — `[hotkey]` config block lets you bind a key combination
  (e.g. `super+w`) to open the wpick TUI in a floating popup terminal.
  Supported modifiers: `super`, `ctrl`, `shift`, `alt`. Terminal auto-detected
  (foot → kitty → alacritty → wezterm → xterm). Hyprland: popup spawned with
  `[float;center;size W H]` rules; refocuses if already open.
  Requires the daemon user to be in the `input` group.
  (`wpick-core`, `wpick-daemon`)

- **Wallpaper timer** — `SetTimer { ids, interval_secs, shuffle }` IPC command
  rotates a playlist of wallpapers on a fixed interval. `StopTimer` halts it;
  `GetTimerState` queries remaining seconds and active IDs. TUI exposes a timer
  dialog (`T` key). (`wpick-core`, `wpick-daemon`, `wpick-tui`)

- **Favorites** — wallpapers can be starred (`*` key in TUI) and appear in a
  dedicated Favorites section in the left navigation panel. Saved to config.
  (`wpick-core`, `wpick-tui`)

- **Most Played (Frequent)** — daemon tracks `play_count` and `last_played_secs`
  per wallpaper in SQLite; TUI shows a Frequent section (top 10 by plays).
  (`wpick-core`, `wpick-daemon`, `wpick-tui`)

- **Packs** — named collections of wallpapers. Create with `p` in TUI, assign
  wallpapers from list. Packs appear as nav sections and can be used as timer
  playlists. (`wpick-core`, `wpick-tui`)

- **Spotify-style navigation panel** — left panel shows Favorites / Frequent /
  Packs / Source sections. Toggle with `[` key; navigate with `←→`. Right
  preview panel toggled with `]`. (`wpick-tui`)

- **Sort dialog** — `o` key opens sort options: Default / Name / Size /
  Resolution. (`wpick-tui`)

- **Settings dialog** — `S` key opens a two-level settings panel: Theme,
  Colors, Layout, Volume style, Now-playing position. (`wpick-tui`)

- **Help overlay** — `?` key shows full key binding reference. (`wpick-tui`)

- **Theme presets** — `dark` (default), `nord`, `dracula`, `tokyo`, `forrest`,
  `deep`. Set via `[tui] theme = "nord"` or Settings dialog. (`wpick-core`,
  `wpick-tui`)

- **Windowed TUI mode** — `[tui] windowed = true` renders the TUI in an
  82 × 82 % centered sub-area instead of fullscreen. (`wpick-tui`)

- **Screen-lock reassert** — Hyprland `lockguard>>0` event (screen unlock)
  schedules a surface reassert after 1 s so wpick re-appears on top of
  QuickShell's background layer, which re-renders on unlock. (`wpick-daemon`)

- **`[tui] surface_reassert_secs`** — configurable delay (default 12 s) before
  the renderer reinitialises its Wayland surfaces on startup. Set to `0` to
  disable. Previously the delay was always 8 s and not configurable.
  (`wpick-core`, `wpick-daemon`)

- **`RecordPlay` IPC command** — TUI sends `RecordPlay { id }` after each
  wallpaper application so the daemon can update play statistics without
  relying on the renderer's internal state. (`wpick-core`, `wpick-daemon`,
  `wpick-tui`)

### Fixed

- **Shuffle fairness** — timer shuffle replaced `DefaultHasher` seeded by
  `SystemTime` (non-uniform, deterministic within a second) with `fastrand::shuffle`
  (uniform Fisher-Yates). (`wpick-daemon`)

- **IPC idle leak** — `recv_command` now has a 30-second timeout; stalled
  client connections are closed automatically instead of leaking tokio tasks.
  (`wpick-daemon`)

- **Kill command auth** — `Kill` IPC command now checks `SO_PEERCRED`; only
  the process owner (same UID as the daemon) can issue it. Fails closed:
  if credential retrieval fails the command is denied. (`wpick-daemon`)

- **Symlink traversal in extra_dirs** — `WalkDir` changed from
  `follow_links(true)` to `follow_links(false)` when scanning user-defined
  video directories. Prevents reads outside declared paths via crafted symlinks.
  (`wpick-core`)

### Changed

- **`license = "MIT"`** added to all three crate `Cargo.toml` files. SPDX
  field was previously absent. (`wpick-core`, `wpick-daemon`, `wpick-tui`)

---

## [0.4.1] — 2026-05-15

### Added

- **Shell completions** — `wpick completions bash|zsh|fish` prints a
  completion script for the respective shell. PKGBUILD installs them to
  the system paths automatically. (`wpick-tui`)

- **Man page** — `wpick man` prints a troff man page; `wpick man > wpick.1
  && man ./wpick.1` to read locally. (`wpick-tui`)

### Fixed

- `Completions` and `Man` subcommands were hidden from `--help`; now
  visible and documented. (`wpick-tui`)

- PKGBUILD: added `libpulse` runtime dependency; fixed Fedora dep name
  (`pulseaudio-libs-devel`); PKGBUILD now installs completions, man page,
  and systemd service. (`PKGBUILD`)

- Clippy `-D warnings` clean across all three crates. (`wpick-core`,
  `wpick-daemon`, `wpick-tui`)

---

## [0.4.0] — 2026-05-15

### Added

- **Wallpaper persistence** — active wallpaper ID is saved to config and
  restored on daemon restart (`last_wallpaper_id`). (`wpick-daemon`,
  `wpick-core`)

- **Per-monitor wallpaper selection** — TUI shows a monitor picker when
  pressing `Enter`; each connected `wl_output` can have an independent
  wallpaper set independently via IPC `Set { id, monitor }`.
  (`wpick-tui`, `wpick-daemon`)

- **TUI folder picker** — press `s` to open an interactive directory browser
  and add custom video folders (`paths.extra_dirs`) without editing the
  config file manually. System paths (`/usr`, `/proc`, `/sys`, `/dev`,
  `/run`) are blocked. (`wpick-tui`)

- **Custom video folders** — `paths.extra_dirs` in config accepts a list of
  absolute paths; video files found there appear as
  `Local { label: <dirname> }` wallpapers and are deduplicated by stable
  FNV-1a ID. (`wpick-core`, `wpick-daemon`)

- **Source filter** — TUI `Tab` cycles between All / Workshop / Local
  source views. (`wpick-tui`)

- **FitMode per monitor** — `SetFit { fit, monitor }` IPC command applies a
  scale mode to one output or all; persisted in `[monitors."<name>"]`.
  (`wpick-core`, `wpick-daemon`)

- **XDG socket path** — daemon socket moves to
  `$XDG_RUNTIME_DIR/wpick.sock` (falls back to `~/.wpick.sock` for TTY
  sessions). (`wpick-core`)

- **systemd user service** — `dist/systemd/wpick-daemon.service` ships in
  the package for `systemctl --user enable wpick-daemon`. (`dist/`)

- **MIT license** — `LICENSE` file added.

### Changed

- **Video-only** — Scene and Web wallpaper types removed; wpick is
  video/image focused. (`wpick-core`, `wpick-daemon`)

- **Smart scaling** — `WallpaperInfo` now carries `width`/`height`;
  renderer skips upscale when source matches display resolution.
  Letterbox (`Fit`) and center (`Center`) modes preserve aspect ratio.
  (`wpick-core`, `wpick-daemon`)

- **Stderr redirect** — daemon redirects `fd 2` to the rolling log file
  so VA-API / ffmpeg C-library noise does not appear in the terminal.
  (`wpick-daemon`)

- **Scan non-blocking** — `Scan` IPC command runs in a background task;
  TUI remains responsive during large library scans. (`wpick-daemon`)

- **Status messages simplified** — TUI status bar shows short strings
  (`Applied`, `Folder added`, etc.) with no embedded paths. (`wpick-tui`)

### Fixed

- **fd leak** — `dup2()` for stderr redirect previously leaked the source
  file descriptor; `close()` is now called after `dup2()`. (`wpick-daemon`)

- **Cursor jitter on high-refresh displays** — the render loop replaced a
  blind 1 ms `sleep()` with `poll()` on the Wayland connection fd.  The
  thread now blocks until a frame callback (or next-frame deadline) arrives,
  giving the compositor more CPU time and eliminating micro-stutter visible
  at 120 Hz+ with two wallpapers playing simultaneously. (`wpick-daemon`)

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
