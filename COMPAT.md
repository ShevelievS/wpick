# COMPAT.md — version compatibility

> Verified version combinations for building wpick. If a dependency
> is missing here, it's not a hard pin — add it in `Cargo.toml` at a
> reasonable version and document the result in this file.

---

## Workspace

| Setting          | Value                                |
|------------------|--------------------------------------|
| Rust edition     | `2021`                               |
| MSRV             | `1.75`                               |
| Tokio runtime    | `multi_thread` for daemon, `full` for TUI |
| License          | `MIT OR Apache-2.0`                  |

---

## Third-party crate versions (v0.2.0)

### wpick-core

| Crate              | Version | Features                             | Notes                                         |
|--------------------|---------|--------------------------------------|-----------------------------------------------|
| `thiserror`        | `1`     | —                                    |                                               |
| `serde`            | `1`     | `derive`                             |                                               |
| `serde_json`       | `1`     | —                                    |                                               |
| `keyvalues-serde`  | `0.2`   | —                                    | Steam VDF parser                              |
| `toml`             | `0.8`   | —                                    |                                               |
| `rusqlite`         | `0.31`  | `bundled`                            | Bundled so we don't fight libsqlite versions |
| `tokio`            | `1`     | `io-util`, `fs`, `sync`, `macros`    | Narrow subset — core is a library             |
| `tracing`          | `0.1`   | —                                    |                                               |
| `dirs`             | `5`     | —                                    | XDG path resolution                           |
| `flate2`           | `1`     | —                                    | Used by some PKG variants                     |
| `byteorder`        | `1`     | —                                    | PKG header parsing                            |

Dev:

| Crate       | Version | Features                        |
|-------------|---------|---------------------------------|
| `tempfile`  | `3`     | —                               |
| `tokio`     | `1`     | `macros`, `rt`, `io-util`       |

### wpick-daemon

| Crate                      | Version | Features                                                        | Notes                       |
|----------------------------|---------|-----------------------------------------------------------------|------------------------------|
| `wpick-core`               | path    | —                                                               |                              |
| `anyhow`                   | `1`     | —                                                               |                              |
| `tokio`                    | `1`     | `full`                                                          |                              |
| `tracing`                  | `0.1`   | —                                                               |                              |
| `tracing-subscriber`       | `0.3`   | `env-filter`, `fmt`                                             |                              |
| `tracing-appender`         | `0.2`   | —                                                               | File rotation (standalone)   |
| `wayland-client`           | `0.31`  | —                                                               |                              |
| `wayland-protocols`        | `0.31`  | `client`, `staging`                                             | xdg-output lives in staging  |
| `wayland-protocols-wlr`    | `0.2`   | `client`                                                        | layer-shell                  |
| `raw-window-handle`        | `0.6`   | —                                                               |                              |
| `wgpu`                     | `0.20`  | `wgsl`                                                          | Default backends: Vulkan + GL|
| `pollster`                 | `0.3`   | —                                                               | Block on wgpu init futures   |
| `bytemuck`                 | `1`     | `derive`                                                        | Uniform buffer pod           |
| `ffmpeg-next`              | `8`     | —                                                               | See §System ffmpeg below     |
| `rodio`                    | `0.19`  | `wav`, `vorbis`, `flac`, `mp3`, `symphonia-all`, `no-default-features` | symphonia-all = broad codec support |
| `libpulse-binding`         | `2`     | —                                                               | Ducking (v0.2 unchanged)     |
| `libpulse-simple-binding`  | `2`     | —                                                               |                              |

Dev:

| Crate       | Version |
|-------------|---------|
| `tempfile`  | `3`     |

### wpick-tui

| Crate                | Version | Features                       | Notes                           |
|----------------------|---------|--------------------------------|---------------------------------|
| `wpick-core`         | path    | —                              |                                 |
| `anyhow`             | `1`     | —                              |                                 |
| `clap`               | `4`     | `derive`, `env`                |                                 |
| `clap_complete`      | `4`     | —                              | **v0.2** — shell completions    |
| `clap_mangen`        | `0.2`   | —                              | **v0.2** — man page generation  |
| `tokio`              | `1`     | `full`                         |                                 |
| `tracing`            | `0.1`   | —                              |                                 |
| `tracing-subscriber` | `0.3`   | `env-filter`, `fmt`            |                                 |
| `ratatui`            | `0.28`  | —                              |                                 |
| `crossterm`          | `0.28`  | —                              | Must match ratatui's crossterm  |

---

## System ffmpeg

`ffmpeg-next` is pinned to major version `8`. This requires ffmpeg
**8.x** headers and libraries at build time.

| Distro                     | ffmpeg version in 2026 | Compatible pin          |
|----------------------------|-------------------------|--------------------------|
| Arch Linux                 | 8.x                     | `ffmpeg-next = "8"`     |
| NixOS unstable             | 8.x                     | `ffmpeg-next = "8"`     |
| Ubuntu 24.04 LTS           | 6.x                     | pin to `"6"` (rebuild)  |
| Ubuntu 25.10+              | 7.x                     | pin to `"7"` (rebuild)  |
| Fedora 40+                 | 7.x                     | pin to `"7"` (rebuild)  |
| Debian 12 stable           | 5.x                     | unsupported — backport or use AppImage |

See `ERRORS_TO_AVOID.md` E-26 for what a version mismatch looks like
and how to recover.

---

## Verified combinations (2026-04)

| OS                  | Kernel  | Compositor | GPU          | ffmpeg | Rust    | Status                |
|---------------------|---------|------------|---------------|--------|---------|-----------------------|
| Arch Linux          | 6.13    | Hyprland   | AMD RDNA3     | 8.0    | 1.86    | ✅ primary dev target |
| Arch Linux          | 6.13    | Hyprland   | NVIDIA 565+   | 8.0    | 1.86    | ✅ (tested)           |
| Arch Linux          | 6.13    | Sway       | AMD RDNA3     | 8.0    | 1.86    | ✅ (tested)           |
| NixOS unstable      | 6.13    | Hyprland   | AMD RDNA3     | 8.0    | 1.86    | ✅ (flake build)      |
| Fedora 40           | 6.11    | Hyprland   | Intel Arc     | 7.0    | 1.83    | ⚠  rebuild with ffmpeg-next="7" |
| Ubuntu 24.04 LTS    | 6.8     | Hyprland   | NVIDIA 550    | 6.0    | 1.78    | ⚠  rebuild with ffmpeg-next="6" |

Update this table whenever a combination is tested.

---

## Breaking protocol versions

`wpick` communicates with two unstable Wayland extensions:

| Protocol                   | Version  | Used for                              |
|----------------------------|----------|----------------------------------------|
| `zwlr_layer_shell_v1`      | v4+      | Layer-shell wallpaper surface          |
| `zxdg_output_manager_v1`   | v3+      | Fallback for `wl_output` name         |
| `wl_output`                | v4+      | Preferred; has `name` event           |

Compositors:

- **Hyprland** — tested with 0.46+. Fullscreen IPC uses socket2 protocol.
- **Sway** — tested with 1.10+. No fullscreen IPC integration (pause
  source `on_fullscreen` becomes a no-op).
- **river** — tested briefly; appears to work, not a CI target.
- **GNOME Mutter** — not supported (no layer-shell).
- **KDE KWin** — not supported (no layer-shell).

---

## Breaking IPC versions

v0.2 adds an optional `monitor` field to `ClientCommand::Set`.
Because `#[serde(default)]` makes the field optional, v0.1 client
payloads still deserialise. We do **not** version-stamp IPC
messages; the contract is implicit via serde defaults.

Going forward, all new fields must be additive and default-able.
Breaking the wire requires a major version bump (v1.0) and a
compat shim.