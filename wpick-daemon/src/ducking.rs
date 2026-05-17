/// Audio ducking via libpulse polling.
///
/// Every POLL_MS ms we count non-wpick sink-inputs. If any exist → fade out.
/// When none remain → fade in.
///
/// Fixes applied here:
///   C-1  — fade race: `fade_gen` counter cancels stale fade threads.
///   H-3  — set_sink ignored mute: `SharedData.muted` respected everywhere.
///   M-8  — ducking thread never stopped: `DuckHandle::Drop` sets stop flag.
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::{Context, FlagSet as CtxFlagSet};
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::proplist::Proplist;

const OUR_APP:    &str = "wpick-daemon";
const FADE_SECS:  f32  = 1.0;
const FADE_STEPS: u32  = 40;
const POLL_MS:    u64  = 500;

// ─── Shared state ─────────────────────────────────────────────────────────────

struct SharedData {
    sink:          Option<Arc<rodio::Sink>>,
    target_volume: f32,
    muted:         bool,  // H-3: track mute state so fade/set_sink respect it
    ducked:        bool,
    foreign_count: u32,
    fade_gen:      u64,   // C-1: each new fade increments; old threads exit on mismatch
}

#[derive(Clone)]
struct Shared(Arc<Mutex<SharedData>>);

impl Shared {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(SharedData {
            sink:          None,
            target_volume: 0.8,
            muted:         false,
            ducked:        false,
            foreign_count: 0,
            fade_gen:      0,
        })))
    }

    // H-3: muted is now passed so set_sink never overrides a muted state.
    fn set_sink(&self, sink: Arc<rodio::Sink>, volume: f32, muted: bool) {
        let mut d = self.0.lock().unwrap_or_else(|e| e.into_inner());
        d.target_volume = volume;
        d.muted         = muted;
        let effective = if d.ducked || muted { 0.0 } else { volume };
        sink.set_volume(effective);
        d.sink = Some(sink);
    }

    // H-3: volume and muted updated together so they stay in sync.
    fn update_volume(&self, vol: f32, muted: bool) {
        let mut d = self.0.lock().unwrap_or_else(|e| e.into_inner());
        d.target_volume = vol;
        d.muted         = muted;
        if let Some(ref s) = d.sink {
            let effective = if d.ducked || muted { 0.0 } else { vol };
            s.set_volume(effective);
        }
    }

    /// Returns (should_fade_out, should_fade_in).
    fn update(&self, new_foreign: u32) -> (bool, bool) {
        let mut d      = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let was_ducked = d.ducked;
        d.foreign_count = new_foreign;
        d.ducked        = new_foreign > 0;
        (d.ducked && !was_ducked, !d.ducked && was_ducked)
    }

    fn fade(&self, duck: bool) {
        let shared = self.clone();
        // C-1: bump generation; old fade threads exit when they see a different gen.
        let gen = {
            let mut d = shared.0.lock().unwrap_or_else(|e| e.into_inner());
            d.fade_gen += 1;
            d.fade_gen
        };
        std::thread::Builder::new()
            .name(if duck { "wpick-fade-out" } else { "wpick-fade-in" }.into())
            .spawn(move || {
                let step_ms = (FADE_SECS * 1000.0 / FADE_STEPS as f32) as u64;
                for i in 0..=FADE_STEPS {
                    std::thread::sleep(Duration::from_millis(step_ms));
                    let d = shared.0.lock().unwrap_or_else(|e| e.into_inner());
                    // C-1: a newer fade started — stop immediately.
                    if d.fade_gen != gen { break; }
                    // H-3: if muted, keep volume at 0 regardless of duck direction.
                    if d.muted {
                        if let Some(ref s) = d.sink { s.set_volume(0.0); }
                        continue;
                    }
                    let t   = i as f32 / FADE_STEPS as f32;
                    let vol = if duck {
                        d.target_volume * (1.0 - t)
                    } else {
                        d.target_volume * t
                    };
                    if let Some(ref s) = d.sink { s.set_volume(vol.clamp(0.0, 1.0)); }
                }
            })
            .ok();
    }
}

// ─── Public handle ────────────────────────────────────────────────────────────

pub struct DuckHandle {
    shared: Option<Shared>,
    // M-8: stop flag; set by Drop so ducking thread exits within POLL_MS.
    stop:   Arc<AtomicBool>,
}

impl Drop for DuckHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl DuckHandle {
    // H-3: muted is now required so set_sink respects the current mute state.
    pub fn register_sink(&self, sink: Arc<rodio::Sink>, volume: f32, muted: bool) {
        if let Some(ref s) = self.shared { s.set_sink(sink, volume, muted); }
    }

    // H-3: replaces set_volume(vol); passes muted together with volume.
    pub fn update_volume(&self, vol: f32, muted: bool) {
        if let Some(ref s) = self.shared { s.update_volume(vol, muted); }
    }
}

// ─── Start ducking thread ─────────────────────────────────────────────────────

/// No-op handle used when `audio.ducking_enabled = false` in config.
/// All methods are safe to call — they simply do nothing.
pub fn start_noop() -> DuckHandle {
    DuckHandle { shared: None, stop: Arc::new(AtomicBool::new(false)) }
}

pub fn start() -> DuckHandle {
    let shared = Shared::new();
    let stop   = Arc::new(AtomicBool::new(false));

    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);

    let shared_clone = shared.clone();
    let stop_clone   = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("wpick-ducking".into())
        .spawn(move || {
            if let Err(e) = pa_loop(shared_clone, tx, stop_clone) {
                tracing::warn!("Ducking loop: {}", e);
            }
            tracing::info!("Ducking thread exited");
        })
        .map_err(|e| tracing::warn!("Failed to spawn ducking thread: {}", e))
        .ok();

    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(_) => DuckHandle { shared: Some(shared), stop },
        Err(_) => {
            tracing::warn!("Ducking unavailable: PulseAudio not responding");
            DuckHandle { shared: None, stop }
        }
    }
}

// ─── PA mainloop ──────────────────────────────────────────────────────────────

fn pa_loop(
    shared:       Shared,
    connected_tx: std::sync::mpsc::SyncSender<()>,
    stop:         Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut ml = Mainloop::new()
        .ok_or_else(|| anyhow::anyhow!("PA mainloop init failed"))?;

    let mut ctx = {
        let mut pl = Proplist::new()
            .ok_or_else(|| anyhow::anyhow!("PA proplist init failed"))?;
        pl.set_str(pulse::proplist::properties::APPLICATION_NAME, "wpick").ok();
        Context::new_with_proplist(&ml, "wpick", &pl)
            .ok_or_else(|| anyhow::anyhow!("PA context init failed"))?
    };

    ctx.connect(None, CtxFlagSet::NOFLAGS, None)
        .map_err(|e| anyhow::anyhow!("PA connect failed: {:?}", e))?;

    // Wait for Ready; signal start() once connected.
    loop {
        match ml.iterate(false) {
            IterateResult::Quit(_) => anyhow::bail!("PA mainloop quit during connect"),
            IterateResult::Err(_)  => anyhow::bail!("PA mainloop error during connect"),
            IterateResult::Success(_) => {}
        }
        match ctx.get_state() {
            pulse::context::State::Ready => {
                connected_tx.send(()).ok();
                break;
            }
            pulse::context::State::Failed
            | pulse::context::State::Terminated =>
                anyhow::bail!("PA context failed to connect"),
            _ => {}
        }
    }

    tracing::info!("Ducking: PulseAudio connected");

    loop {
        // M-8: exit cleanly when DuckHandle is dropped.
        if stop.load(Ordering::Relaxed) {
            tracing::info!("Ducking: stop requested");
            break;
        }

        let foreign = count_foreign(&mut ml, &ctx);
        let (fade_out, fade_in) = shared.update(foreign);

        if fade_out {
            tracing::info!("Ducking: {} foreign stream(s) → fading out", foreign);
            shared.fade(true);
        } else if fade_in {
            tracing::info!("Ducking: no foreign streams → fading in");
            shared.fade(false);
        }

        std::thread::sleep(Duration::from_millis(POLL_MS));
    }

    Ok(())
}

// ─── Count non-wpick sink-inputs ──────────────────────────────────────────────

fn count_foreign(ml: &mut Mainloop, ctx: &Context) -> u32 {
    use std::sync::atomic::{AtomicBool, AtomicU32};
    let count: Arc<AtomicU32>  = Arc::new(AtomicU32::new(0));
    let done:  Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    let count_cb = count.clone();
    let done_cb  = done.clone();

    ctx.introspect().get_sink_input_info_list(move |result| {
        match result {
            ListResult::Item(info) => {
                let name = info.proplist
                    .get_str("application.name")
                    .unwrap_or_default();
                if !name.contains(OUR_APP) && !info.corked {
                    count_cb.fetch_add(1, Ordering::Relaxed);
                }
            }
            ListResult::End | ListResult::Error => {
                done_cb.store(true, Ordering::Release);
            }
        }
    });

    loop {
        if done.load(Ordering::Acquire) { break; }
        match ml.iterate(true) {
            IterateResult::Quit(_) | IterateResult::Err(_) => break,
            IterateResult::Success(_) => {}
        }
    }

    count.load(Ordering::Relaxed)
}
