# wpick

> Native Wayland live wallpaper manager for Wallpaper Engine (Steam)
> content — no Wine, no `linux-wallpaperengine`, no DRM hacks.

[![AUR version](https://img.shields.io/aur/version/wpick-bin)](https://aur.archlinux.org/packages/wpick-bin)
[![Nix flake](https://img.shields.io/badge/nix-flake-blue)](https://github.com/ederadar/wpick)
[![CI](https://github.com/ederadar/wpick/actions/workflows/release.yml/badge.svg)](https://github.com/ederadar/wpick/actions)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green)](LICENSE)

wpick plays Wallpaper Engine `.pkg` video wallpapers directly on
`wlr-layer-shell` background surfaces, with audio, automatic pausing
on fullscreen / battery, multi-monitor support, and a TUI for
browsing your Steam Workshop subscriptions.

![screenshot placeholder](docs/screenshot.png)

---

## Status

| Area                          | State       |
|-------------------------------|-------------|
| Video wallpapers              | ✅          |
| Audio with PulseAudio ducking | ✅          |
| Multi-monitor                 | ✅ **(v0.2)**|
| Auto-pause (fullscreen, battery, lid) | ✅ **(v0.2)** |
| Scene wallpapers              | ❌ (v0.3)   |
| Web wallpapers                | ❌ (v0.3)   |
| TUI preview images            | ❌ (v0.3)   |
| Systemd user service          | ✅ **(v0.2)** |
| Shell completions + man       | ✅ **(v0.2)** |
| AUR packaging                 | ✅          |
| Nix flake                     | ✅ **(v0.2)** |
| Flatpak                       | ❌ (v0.3)   |

Tested on:

- Arch Linux + Hyprland + AMD RDNA3 (primary)
- Arch Linux + Hyprland + NVIDIA 565
- Arch Linux + Sway + AMD RDNA3
- NixOS unstable + Hyprland

GNOME and KDE are **not** supported (no `wlr-layer-shell`).

---

## Features (v0.2)

- Native Wayland playback (`wlr-layer-shell` + `wgpu` + `ffmpeg-next`).
- Per-output wallpapers with hotplug. Plug in a second monitor and
  the configured wallpaper auto-applies.
- Auto-pause: fullscreen detection (Hyprland IPC), battery state,
  lid close. CPU drops from ~5% to <0.5% while paused.
- Streaming audio decoder — no more 100 MB pre-loads per track.
- Audio ducking: wpick fades out when other apps play sound.
- Persistent config at `~/.config/wpick/config.toml` — full reference
  in [docs/CONFIG.md](docs/CONFIG.md).
- ratatui TUI + plain CLI for scripting.
- SQLite metadata cache — initial scan on first run, fast rescans after.
- Shell completions (bash/zsh/fish) and man pages.
- systemd user service with `ProtectHome`, memory caps, journal logging.

---

## Install

See [docs/INSTALL.md](docs/INSTALL.md) for full instructions. Quick
paths:

### Arch Linux

```bash
paru -S wpick-bin
```

### NixOS / Nix

```bash
nix run github:ederadar/wpick -- list
```

### From source

```bash
git clone https://github.com/ederadar/wpick
cd wpick
cargo build --workspace --release
sudo install -Dm755 target/release/wpick        /usr/local/bin/wpick
sudo install -Dm755 target/release/wpick-daemon /usr/local/bin/wpick-daemon
```

Full manual-install steps (completions, man, systemd unit) in
[docs/INSTALL.md](docs/INSTALL.md#manual-build-from-source).

### Autostart via systemd

```bash
systemctl --user daemon-reload
systemctl --user enable --now wpick-daemon
```

See [docs/SYSTEMD.md](docs/SYSTEMD.md) for hardening details and
troubleshooting.

---

## Usage

Interactive TUI:

```bash
wpick
```

One-shot CLI:

```bash
wpick list
wpick outputs
wpick set 1234567890                        # all monitors
wpick set 1234567890 --monitor HDMI-A-1     # one monitor
wpick volume 60
wpick mute
wpick pause                                  # manual pause
wpick resume                                 # back to automatic
wpick status
wpick kill                                   # stop daemon
```

Key bindings inside the TUI: [docs/TUI.md](docs/TUI.md#key-bindings).

---

## Documentation

Full docs in the [`docs/`](docs/) folder:

- [PROJECT.md](docs/PROJECT.md) — architecture, IPC protocol, file layout.
- [INSTALL.md](docs/INSTALL.md) — installation on Arch, NixOS, and source.
- [CONFIG.md](docs/CONFIG.md) — every config field, defaults, migration.
- [MULTIMONITOR.md](docs/MULTIMONITOR.md) — per-output renderer design.
- [PAUSE.md](docs/PAUSE.md) — auto-pause sources and logic.
- [SYSTEMD.md](docs/SYSTEMD.md) — user service, diagnostics, logging.
- [TUI.md](docs/TUI.md) — TUI state, key bindings, CLI surface.
- [DAEMON.md](docs/DAEMON.md) — Wayland + wgpu + audio pipeline spec.
- [CORE.md](docs/CORE.md) — library API reference.
- [ERRORS.md](docs/ERRORS.md) — error handling policy.

Internal development references:

- [COMPAT.md](COMPAT.md) — verified version combinations.
- [CHANGELOG.md](CHANGELOG.md) — release history.
- [ERRORS_TO_AVOID.md](ERRORS_TO_AVOID.md) — bug catalogue.
- [SEQUENCE.md](SEQUENCE.md) — release block sequence.

---

## Contributing

wpick is MIT OR Apache-2.0. Bug reports via GitHub Issues. For PRs:

1. Match the style and structure of existing modules.
2. Respect the crate dependency rules in
   [docs/PROJECT.md](docs/PROJECT.md#crate-dependency-rules). CI will
   grep-check them.
3. Every new bug hit during development → entry in
   [ERRORS_TO_AVOID.md](ERRORS_TO_AVOID.md).
4. Run before pushing:

   ```bash
   cargo test  --workspace
   cargo clippy --workspace -- -D warnings
   cargo build --workspace --release
   ```

See [SEQUENCE.md](SEQUENCE.md) for the block-by-block development
process we use across releases.

---

## License

Dual-licensed under MIT or Apache-2.0, at your option. See
[LICENSE](LICENSE).