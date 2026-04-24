# SEQUENCE.md — implementation block sequence

> Ordered list of implementation blocks across all releases, with
> dependency edges and gate criteria. Each block maps to a prompt in
> `PROMPTS.md` (v0.1) or `PROMPTS_V0.2.md` (v0.2).

---

## v0.1.0 — foundation (completed)

### Phase 1 — Foundation

| Block | Title                          | Depends on | Gate                                         |
|-------|--------------------------------|------------|----------------------------------------------|
| 0     | Workspace scaffold             | —          | `cargo build --workspace` succeeds           |
| 1     | `error.rs` + `model.rs`         | 0          | Core compiles with empty modules replaced    |
| 2     | `config.rs`                     | 1          | `config::tests` green                        |

### Phase 2 — Data Pipeline

| Block | Title             | Depends on | Gate                                     |
|-------|-------------------|------------|------------------------------------------|
| 3     | `discovery.rs`    | 2          | Parses real Steam library                |
| 4     | `pkg.rs`          | 3          | Extracts a test `.pkg`; mtime skip works |
| 5     | `cache.rs`        | 1          | SQLite round-trips `WallpaperInfo`       |

### Phase 3 — IPC Contract

| Block | Title       | Depends on | Gate                                     |
|-------|-------------|------------|------------------------------------------|
| 6     | `ipc.rs`    | 1, 2       | Round-trip for every variant; pipe test  |

### Phase 4 — Daemon

| Block | Title                     | Depends on | Gate                                       |
|-------|---------------------------|------------|---------------------------------------------|
| 7     | `state.rs` + `ipc_server` | 6          | `echo '{"type":"List"}' \| nc -U` responds |
| 8     | `video.rs`                | 4          | Decodes a test .mp4 at expected fps         |
| 9     | `audio.rs`                | 8          | Plays test audio; volume honoured            |
| 10    | `renderer.rs` + `main.rs` | 7, 8, 9    | Full daemon renders a video on a surface    |

### Phase 5 — TUI

| Block | Title                   | Depends on | Gate                                   |
|-------|-------------------------|------------|----------------------------------------|
| 11    | `client.rs`             | 6          | Connects to daemon; round-trips a cmd  |
| 12    | `app.rs`                | 11         | Reconnect after daemon restart works    |
| 13    | `ui.rs` + `main.rs` (tui)| 12        | TUI renders list; keys apply wallpaper  |

### Phase 6 — Integration

| Block | Title                           | Depends on | Gate                         |
|-------|---------------------------------|------------|-------------------------------|
| 14    | End-to-end smoke test           | all        | Manual: TUI apply → plays     |
| 15    | Packaging + AUR PKGBUILD        | 14         | `makepkg -si` on a VM         |
| 16    | CI (GitHub Actions)             | 14         | Green on PR + tag             |
| 17    | Release v0.1.0                  | 15, 16     | Tag pushed, binaries on Release |

───────────────────── shipped as v0.1.0 ─────────────────────

---

## v0.2.0 — multi-monitor, auto-pause, distro

### Phase A — Foundation

| Block | Title                       | Depends on | Gate                                                   |
|-------|-----------------------------|------------|--------------------------------------------------------|
| 18    | Extended config schema      | v0.1       | `config::tests` green incl. v0.1 forward-compat test    |
| 19    | Frame buffer reuse          | v0.1       | `cargo clippy` green; heaptrack: <1 alloc/frame in loop |

### Phase B — Feature depth

| Block | Title                | Depends on | Gate                                                      |
|-------|----------------------|------------|-----------------------------------------------------------|
| 20    | Streaming audio      | 18         | `ps -o rss` ≤ 80 MB on 5-min track; start latency <500 ms  |
| 21    | Multi-monitor        | 18         | Two monitors show independent wallpapers; hotplug survives |
| 22    | Pause manager        | 18         | `pidstat 1` shows <0.5% CPU when fullscreen is open       |

### Phase C — Ecosystem & distribution

| Block | Title                          | Depends on | Gate                                        |
|-------|--------------------------------|------------|---------------------------------------------|
| 23    | CLI completions + man pages    | v0.1       | `wpick completions bash` produces valid bash |
| 24    | Systemd user service           | v0.1       | `systemctl --user start wpick-daemon` works  |
| 25    | Multi-distro packaging         | 23, 24     | `makepkg -si` + `nix build` both green      |

### Phase D — Release

| Block | Title                                    | Depends on | Gate                                            |
|-------|------------------------------------------|------------|-------------------------------------------------|
| 26    | Docs sync, version bump, CHANGELOG, tag  | all        | Tag v0.2.0 on GitHub; release workflow green    |

### Dependency graph (v0.2)

```
18 ──┬─► 20
     ├─► 21 ─┐
     └─► 22 ─┤
             ├─► 26
19 ──────────┤
23 ─┐        │
24 ─┼─► 25 ──┘
```

Critical path: **18 → 21 → 26**. Blocks 19, 23, 24 parallelise.

### Estimated timing

| Block | Rough effort |
|-------|--------------|
| 18    | 0.5 day      |
| 19    | 0.5 day      |
| 20    | 2 days       |
| 21    | 5–7 days     |
| 22    | 2–3 days     |
| 23    | 0.5 day      |
| 24    | 0.5 day      |
| 25    | 1.5 days     |
| 26    | 0.5 day      |

**Total:** 13–17 days of focused work. Plan: 1–1.5 months wall-clock
with debug buffer.

───────────────────── target: v0.2.0 ─────────────────────

---

## v0.3 (planned — not scheduled)

Deferred items, in rough priority order:

- TUI preview images (ratatui-image + Kitty graphics protocol).
- Scene wallpaper type — renderer support via JSON scene graph.
- Web wallpaper type — WebKit embed (Wayland).
- TUI monitor selector UI.
- Socket migration to `$XDG_RUNTIME_DIR/wpick.sock` with fallback.
- Flatpak packaging (blocked on socket path fix).
- Hardware video decode (VAAPI / NVDEC) via ffmpeg-next bindings.
- UPower / logind D-Bus integration for lid detection.
- Per-output audio via PipeWire sinks.

---

## Process rules

### Gate criteria

Each block has a **single concrete gate**. Gate passes → merge block
and move to the next. Gate fails → block does not ship; write
entries in `ERRORS_TO_AVOID.md`, fix, re-run.

### New-bug rule

Any bug encountered during a block gets an entry in
`ERRORS_TO_AVOID.md` **before** the fix. No exceptions. The "E-NN"
numbering is append-only.

### Skill updates vs CHANGELOG updates

- **Local** (inside each block's prompt): update relevant `skills/*.md`
  and module-specific `docs/*.md`.
- **Global** (Block 26 only): `CHANGELOG.md`, `COMPAT.md`, `README.md`
  Status table, `SEQUENCE.md` "released as" marker, version bump in
  all three `Cargo.toml` + `PKGBUILD` + `flake.nix`.

### Verification before tag

Block 26 runs the full regression pass:

```bash
cargo test  --workspace
cargo clippy --workspace -- -D warnings
cargo build --workspace --release
# Manual smoke: daemon start, wpick set, pause on fullscreen,
# wpick outputs, unplug/replug monitor, journalctl shows clean logs.
```

Only after all of the above: `git tag v0.2.0 && git push origin v0.2.0`.