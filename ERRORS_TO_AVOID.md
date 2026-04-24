# ERRORS_TO_AVOID.md — catalogue of bugs hit, and how to avoid them

> Every entry is a real bug that was either hit during development
> or explicitly reasoned about as "likely to be hit". Add a new entry
> the moment a pattern bites you — before the fix is even written.
> Entries are append-only. If an entry becomes obsolete (e.g. the API
> changed upstream), mark it `OBSOLETE: <reason>` instead of removing it.

Index by category:

- **Cargo / workspace** — E-01, E-02, E-26
- **Wayland** — E-05, E-06, E-07, E-08, E-31, E-32, E-35
- **wgpu** — E-09, E-30
- **ffmpeg** — E-10, E-11, E-15, E-26
- **Audio / rodio** — E-16, E-17, E-28, E-29
- **Async / tokio** — E-03, E-04, E-12, E-13
- **IPC** — E-14
- **ratatui / TUI** — E-18, E-19, E-20, E-21, E-22, E-23, E-24, E-25
- **PKG parsing** — E-27
- **Pause / system integration** — E-33, E-34

---

## Cargo / workspace

### E-01 — crate dependency rules violated

**Symptom:** `wpick-core` fails CI grep for `anyhow`.
**Cause:** Imported `anyhow::Result` in a core module.
**Fix:** Use `crate::Result` (re-exported from `error.rs`). See
`docs/ERRORS.md` for the style table.

### E-02 — workspace member not compiling standalone

**Symptom:** `cargo build -p wpick-tui` succeeds in the workspace
but fails when checked in isolation.
**Cause:** Features inherited from workspace weren't declared locally.
**Fix:** `cargo check -p <crate>` per crate in CI, not just
`--workspace`.

### E-26 — ffmpeg-next version vs system ffmpeg mismatch

**Symptom:** Link errors like `undefined reference to av_frame_alloc`
or segfaults on first `ffmpeg::format::input()`.
**Cause:** Cargo pin is `ffmpeg-next = "8"` but the system has
ffmpeg 6/7.
**Fix:** See `COMPAT.md §System ffmpeg`. Align the pin with the system
version. ffmpeg-next 6/7/8 share 95% of the API; a single-line change
in `Cargo.toml` is usually enough.

---

## Wayland

### E-05 — layer surface exclusive zone not set to -1

**Symptom:** Wallpaper draws but compositor reserves space for it,
shrinking other windows.
**Cause:** `set_exclusive_zone` defaults to 0 which means "normal
window"; for a wallpaper we want `-1` ("draw under everything and
don't reserve space").
**Fix:** `layer_surface.set_exclusive_zone(-1)` before the first
`wl_surface.commit()`.

### E-06 — missing second surface.commit() after ack_configure

**Symptom:** Surface stays black; configure roundtrip completes but
nothing renders.
**Cause:** After `ack_configure(serial)` the surface needs another
`wl_surface.commit()` to tell the compositor "yes, I'm ready".
**Fix:** Always commit twice: once to request the surface, once to
acknowledge after configure.

### E-07 — layer_shell bound at wrong version

**Symptom:** `zwlr_layer_shell_v1` creation fails or produces
surfaces without `get_layer_surface` v4 features.
**Cause:** Requested `version: 1` instead of `version: 4`.
**Fix:** Bind `zwlr_layer_shell_v1` at version 4 when available.

### E-08 — wgpu surface built from WlSurface pointer instead of raw handles

**Symptom:** `Surface::get_current_texture` returns
`Outdated` / `Lost` constantly.
**Cause:** Passed a bare Wayland struct to wgpu. wgpu wants
`raw-window-handle` types, not the wayland-client handles directly.
**Fix:** Implement `HasWindowHandle` + `HasDisplayHandle` for a small
newtype that wraps both pointers, then pass that to
`Instance::create_surface`.

### E-31 — acting on non-CURRENT wl_output::Mode events

**Symptom:** Renderer repeatedly resizes textures and surfaces as the
compositor announces multiple modes per output.
**Cause:** `wl_output::Event::Mode` fires for every supported mode
and for the preferred mode, not only the active one.
**Fix:** Check the `CURRENT` flag and only apply the current mode:

```rust
if flags.contains(WEnum::Value(wl_output::Mode::Current)) { /* apply */ }
```

### E-32 — non-idempotent renderer drop on GlobalRemove

**Symptom:** Panic on `assertion failed: self.inner.is_alive()` when
the compositor announces the output removal twice or when we already
dropped due to a surface error.
**Cause:** Destroy logic assumed first-time removal.
**Fix:** Renderer `Drop` must be idempotent: `if let Some(r) =
renderers.remove(&id) { drop(r) }`. Never `panic!` on "output already
gone".

### E-35 — Hyprland socket drop on compositor reload

**Symptom:** Pause stops responding to fullscreen events after a
`hyprctl reload` or similar.
**Cause:** The IPC socket closes when Hyprland reloads; our reader
got EOF and the task exited.
**Fix:** Wrap the socket read loop in a `loop { try_connect;
read_until_eof; sleep 5s }` pattern. Log info on disconnect/reconnect
exactly once per cycle.

---

## wgpu

### E-09 — no compatible adapter found

**Symptom:** `Instance::request_adapter().await.unwrap()` panics on
machines with only OpenGL available.
**Cause:** Default `RequestAdapterOptions` requires a surface-compatible
Vulkan adapter; Vulkan may not be installed.
**Fix:** Fallback chain: Vulkan → GL (`Backends::VULKAN | Backends::GL`),
plus a friendly error message pointing to `vulkan-radeon` /
`nvidia-utils` / `vulkan-intel`.

### E-30 — per-surface wgpu::Device in multi-monitor

**Symptom:** Slow startup with multiple monitors; each hotplug
blocks for 200ms+. GPU memory usage doubles per output.
**Cause:** Creating a `Device`/`Queue` per output instead of sharing.
**Fix:** Construct `Instance`, `Adapter`, `Device`, `Queue` once in
`RendererManager::init`. Each `OutputRenderer` gets a reference.
Pipelines can also be shared when surface formats match, which they
usually do.

---

## ffmpeg

### E-10 — per-frame Vec allocation in hot path

**Symptom:** Heaptrack shows 30 allocations/sec totalling ~240 MB/s
during playback; `valgrind --tool=massif` agrees.
**Cause:** `rgba.data(0).to_vec()` on every decoded frame.
**Fix (v0.2):** `VideoDecoder` holds `frame_buf: Vec<u8>`. `next_frame_rgba`
clears it, refills with slice/extend, and returns `&[u8]` borrowed from
`self`. See `docs/DAEMON.md §video.rs`.

### E-11 — scaler output format RGB24 instead of RGBA

**Symptom:** GPU texture upload fails with "size mismatch" or
produces pink-tinted frames.
**Cause:** wgpu textures are 4-byte-per-pixel; scaler configured for
3.
**Fix:** `Pixel::RGBA` as the scaler output format, `row_bytes = width * 4`.

### E-15 — seek-to-start without decoder.flush()

**Symptom:** First few frames after a video loop show green/pink
garbage.
**Cause:** Decoder internal buffers carry state from before the seek.
**Fix:** Always `decoder.flush()` after `ctx.seek(0, ..)`.

---

## Audio / rodio

### E-16 — sink.append on empty source

**Symptom:** Silence with no error.
**Cause:** Source iterator was consumed or `None` on first call.
**Fix:** Sanity-check `Source::total_duration()` or preroll one chunk
before `sink.append`.

### E-17 — audio thread spawned without its own runtime

**Symptom:** `tokio::sync::watch::Receiver::changed()` hangs forever
inside the audio thread.
**Cause:** `std::thread::spawn` gives a bare thread; watch channels
need a tokio runtime context.
**Fix:** Build a `current_thread` runtime inside the audio thread and
`block_on` the async function.

### E-28 — StreamingSource returns None on underrun

**Symptom:** Audio plays for ~100 ms and stops. `rodio::Sink` reports
empty even though the decoder thread is alive.
**Cause:** `Iterator::next()` returned `None` instead of `Some(0.0)`
during an underrun (channel empty).
**Fix:** Never return `None` from `StreamingSource::next`. Return
`Some(0.0)` (silence) when `rx.recv()` fails. rodio treats `None` as
"source exhausted" and drops the sink.

### E-29 — shutdown channel drop not observed by decoder thread

**Symptom:** Decoder thread keeps running after the source is
dropped; thread count grows monotonically.
**Cause:** `shutdown_rx.try_recv()` only checked on each packet, but
the decoder was blocked inside `ctx.packets()` reading a slow seek
target.
**Fix:** Also treat `tx.send(chunk).is_err()` as a shutdown signal
(the consumer dropped the receiver). That covers the common case
without needing async interrupts.

---

## Async / tokio

### E-03 — spawn_blocking missing around CPU-bound work

**Symptom:** TUI freezes for 5–15s during cache scan; daemon doesn't
respond to other IPC connections.
**Cause:** PKG extraction + SQLite writes run directly inside a
`tokio::spawn`, blocking the worker thread.
**Fix:** Wrap the scan body in `tokio::task::spawn_blocking`.

### E-04 — mpsc::Sender dropped prematurely

**Symptom:** `Receiver` gets `None` right after startup.
**Cause:** Sender only held in a local scope; was dropped when the
setup closure returned.
**Fix:** Keep the sender in a long-lived owner (`DaemonState`).

### E-12 — MutexGuard held across .await

**Symptom:** Deadlock; `tokio::sync::Mutex` shows contention with no
apparent holder; `tracing` logs stop.
**Cause:** `let g = state.lock().await; do_io().await?;` — guard
lives across the second await.
**Fix:** Extract data and drop guard before awaiting:

```rust
let value = {
    let g = state.lock().await;
    g.field.clone()
};
do_io(value).await?;
```

### E-13 — watch::Sender::send with identical value does not signal "unchanged"

**Symptom:** Receiver sees `has_changed()` fire on every publish even
when the value is the same.
**Cause:** `send` always marks the channel as changed regardless of
equality.
**Fix:** Guard with `if new != old { tx.send(new) }` at the publish
site. For `paused_tx`, this avoids redundant `sink.pause()` calls.

---

## IPC

### E-14 — flush() missing after write_all

**Symptom:** Client hangs on `read_line` forever; server thinks it
wrote the response but it's stuck in `BufWriter`.
**Cause:** `BufWriter::write_all` only fills the internal buffer.
**Fix:** `writer.flush().await?` after every message. All send helpers
in `wpick-core::ipc` do this; do the same in any hand-rolled code.

---

## ratatui / TUI

### E-18 — render_widget instead of render_stateful_widget for List

**Symptom:** Selected item not highlighted.
**Cause:** `render_widget` ignores list state.
**Fix:** `frame.render_stateful_widget(list, area, &mut app.list_state)`.

### E-19 — println! from inside TUI

**Symptom:** Garbled terminal after a log message.
**Cause:** `println!` writes to stdout which the alternate-screen
captures.
**Fix:** `tracing::info!` etc. goes to stderr by default.

### E-20 — terminal state not restored after panic

**Symptom:** Terminal left in raw mode with cursor hidden after a
crash.
**Cause:** No panic hook; teardown only runs on normal exit.
**Fix:** Set a `std::panic::set_hook` that calls `disable_raw_mode`
and `LeaveAlternateScreen`.

### E-21 — crash on empty wallpaper list

**Symptom:** Panic: "index out of bounds: the len is 0".
**Cause:** `wallpapers[selected]` without bounds check.
**Fix:** Always branch on `wallpapers.is_empty()` before indexing.

### E-22 — missing arm in crossterm Event match

**Symptom:** `unreachable!` panic on mouse move.
**Cause:** `match Event { Key(..) => ... }` with no catch-all.
**Fix:** Add `_ => {}`.

### E-23 — holding &mut IpcClient across .await

**Symptom:** Borrow checker errors; or at runtime, serialisation
corruption when two futures write concurrently.
**Cause:** `self.client.as_mut()` held while awaiting something else.
**Fix:** Scope the `as_mut` borrow tightly; clone outputs before
awaiting on other things.

### E-24 — reconnect not attempted after daemon restart

**Symptom:** TUI shows "Disconnected" forever until user quits and
restarts.
**Cause:** `try_reconnect` gated by `last_attempt < 2s` but never
reset after a successful connect.
**Fix:** Set `last_reconnect_attempt = None` on successful reconnect.

### E-25 — MouseCapture enabled without handling mouse

**Symptom:** Mouse events spam the event queue; poll returns busy.
**Cause:** `EnableMouseCapture` in setup but no mouse branch.
**Fix:** Either handle mouse events or don't capture them.

---

## PKG parsing

### E-27 — length-prefixed PKG variant not detected

**Symptom:** Extraction consumes file names and produces zero-byte
outputs.
**Cause:** Some tools prefix the archive with `u32_le(total_len)`
before the `PKGV` magic; our reader started parsing the length as if
it were the header.
**Fix:** Peek first 8 bytes; if the magic is `"PKGV"` directly, use
format A; if it's `u32 + "PKGV"`, return `Ok(None)` (unsupported,
usually Scene-type). Log as `debug!`.

---

## Pause / system integration

### E-33 — missing HYPRLAND_INSTANCE_SIGNATURE crashes pause task

**Symptom:** Daemon exits immediately under non-Hyprland sessions
with `thread panicked: NotPresent` from `std::env::var`.
**Cause:** `PauseManager` called `.unwrap()` on the env var.
**Fix:** Treat absence as "fullscreen source disabled":

```rust
match std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
    Ok(sig) => spawn_hyprland_task(sig, flag),
    Err(_)  => {
        tracing::info!("pause: source fullscreen disabled (not under Hyprland)");
    }
}
```

### E-34 — sysfs `online` file missing on desktops

**Symptom:** On a PC without a battery, `on_battery` source's first
read fails and the whole pause task returns error.
**Cause:** `/sys/class/power_supply/*/online` glob returns empty.
**Fix:** Check the glob at startup; if empty, disable the source
with a single `info!` and continue.

### E-35 — Hyprland socket drop on compositor reload

See E-35 in the Wayland section above.

---

## Appending new entries

- Use the next available `E-NN` number (do not reuse).
- Include: Symptom (what a user/dev sees), Cause (root mechanism),
  Fix (exact pattern), cross-reference to doc if relevant.
- Keep examples tight. The point is pattern recognition next time,
  not a full tutorial.