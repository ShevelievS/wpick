/// Audio ducking via libpulse polling.
///
/// Every POLL_MS ms we count non-wpick sink-inputs. If any exist → fade out.
/// When none remain → fade in. Runs on a dedicated OS thread.
use std::sync::{Arc, Mutex};
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
    ducked:        bool,
    foreign_count: u32,
}

#[derive(Clone)]
struct Shared(Arc<Mutex<SharedData>>);

impl Shared {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(SharedData {
            sink:          None,
            target_volume: 0.8,
            ducked:        false,
            foreign_count: 0,
        })))
    }

    fn set_sink(&self, sink: Arc<rodio::Sink>, volume: f32) {
        let mut d = self.0.lock().unwrap();
        d.target_volume = volume;
        if d.ducked { sink.set_volume(0.0); } else { sink.set_volume(volume); }
        d.sink = Some(sink);
    }

    fn set_volume(&self, v: f32) {
        let mut d = self.0.lock().unwrap();
        d.target_volume = v;
        if !d.ducked {
            if let Some(ref s) = d.sink { s.set_volume(v); }
        }
    }

    /// Returns (should_fade_out, should_fade_in)
    fn update(&self, new_foreign: u32) -> (bool, bool) {
        let mut d      = self.0.lock().unwrap();
        let was_ducked = d.ducked;
        d.foreign_count = new_foreign;
        d.ducked        = new_foreign > 0;
        (d.ducked && !was_ducked, !d.ducked && was_ducked)
    }

    fn fade(&self, duck: bool) {
        let shared = self.clone();
        std::thread::Builder::new()
            .name(if duck { "wpick-fade-out" } else { "wpick-fade-in" }.into())
            .spawn(move || {
                let ms = (FADE_SECS * 1000.0 / FADE_STEPS as f32) as u64;
                for i in 0..=FADE_STEPS {
                    let t = i as f32 / FADE_STEPS as f32;
                    let d = shared.0.lock().unwrap();
                    let v = if duck {
                        d.target_volume * (1.0 - t)
                    } else {
                        d.target_volume * t
                    };
                    if let Some(ref s) = d.sink {
                        s.set_volume(v.clamp(0.0, 1.0));
                    }
                    drop(d);
                    std::thread::sleep(Duration::from_millis(ms));
                }
            })
            .ok();
    }
}

// ─── Public handle ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DuckHandle {
    shared: Option<Shared>,
}

impl DuckHandle {
    pub fn register_sink(&self, sink: Arc<rodio::Sink>, volume: f32) {
        if let Some(ref s) = self.shared { s.set_sink(sink, volume); }
    }

    pub fn set_volume(&self, v: f32) {
        if let Some(ref s) = self.shared { s.set_volume(v); }
    }
}

// ─── Start ducking thread ─────────────────────────────────────────────────────

pub fn start() -> DuckHandle {
    let shared = Shared::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);

    let shared_clone = shared.clone();
    std::thread::Builder::new()
        .name("wpick-ducking".into())
        .spawn(move || {
            if let Err(e) = pa_loop(shared_clone, tx) {
                tracing::warn!("Ducking loop: {}", e);
            }
        })
        .expect("ducking thread");

    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(_) => DuckHandle { shared: Some(shared) },
        Err(_) => {
            tracing::warn!("Ducking unavailable: PulseAudio not responding");
            DuckHandle { shared: None }
        }
    }
}

// ─── PA mainloop ──────────────────────────────────────────────────────────────

fn pa_loop(shared: Shared, connected_tx: std::sync::mpsc::SyncSender<()>) -> anyhow::Result<()> {
    let mut ml = Mainloop::new()
        .ok_or_else(|| anyhow::anyhow!("PA mainloop init failed"))?;

    let mut ctx = {
        let mut pl = Proplist::new().unwrap();
        pl.set_str(pulse::proplist::properties::APPLICATION_NAME, "wpick").ok();
        Context::new_with_proplist(&ml, "wpick", &pl)
            .ok_or_else(|| anyhow::anyhow!("PA context init failed"))?
    };

    ctx.connect(None, CtxFlagSet::NOFLAGS, None)
        .map_err(|e| anyhow::anyhow!("PA connect failed: {:?}", e))?;

    // Wait for Ready state; signal start() via channel once connected
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
            // connected_tx is dropped when bail! returns, unblocking recv_timeout
            _ => {}
        }
    }

    tracing::info!("Ducking: PulseAudio connected");

    loop {
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
}

// ─── Count non-wpick sink-inputs ──────────────────────────────────────────────

fn count_foreign(ml: &mut Mainloop, ctx: &Context) -> u32 {
    // Use a simple counter protected by Mutex.
    // We store count and done flag separately to avoid borrow issues.
    let count: Arc<Mutex<u32>>  = Arc::new(Mutex::new(0));
    let done:  Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    let count_cb = count.clone();
    let done_cb  = done.clone();

    ctx.introspect().get_sink_input_info_list(move |result| {
        match result {
            ListResult::Item(info) => {
                let name = info.proplist
                    .get_str("application.name")
                    .unwrap_or_default();
                if !name.contains(OUR_APP) && !info.corked {
                    if let Ok(mut c) = count_cb.lock() {
                        *c += 1;
                    }
                }
            }
            ListResult::End | ListResult::Error => {
                if let Ok(mut d) = done_cb.lock() {
                    *d = true;
                }
            }
        }
    });

    // Drive mainloop until introspection callback completes
    loop {
        if *done.lock().unwrap() { break; }
        match ml.iterate(true) {
            IterateResult::Quit(_) | IterateResult::Err(_) => break,
            IterateResult::Success(_) => {}
        }
    }

    // Read final count — use a local copy to avoid temporary borrow issue
    let result = *count.lock().unwrap();
    result
}