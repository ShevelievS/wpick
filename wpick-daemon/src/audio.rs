/// Audio playback task.
///
/// Fixes applied here:
///   H-1 — full audio in RAM: replaced one-shot Vec decode with streaming background thread.
///   H-2 — watch-channel race during decode: streaming is non-blocking, no await during decode.
///   H-3 — set_sink ignored mute: muted state passed to DuckHandle on every sink creation/update.
use std::sync::Arc;
use std::time::Duration;
use ffmpeg_next as ffmpeg;
use wpick_core::config::AudioConfig;
use wpick_core::model::WallpaperInfo;

// ─── Streaming constants ──────────────────────────────────────────────────────

/// Default chunk size used when config is not provided (tests / legacy paths).
const CHUNK_SIZE_DEFAULT: usize = 8_192;
/// Default channel capacity (backpressure bound).
const CHANNEL_CAP_DEFAULT: usize = 16;
/// Audio fade-out duration when switching wallpapers (milliseconds).
const AUDIO_FADE_MS: u64 = 1_500;
/// Number of volume steps in the fade-out ramp.
const AUDIO_FADE_STEPS: u32 = 30;

// ─── StreamingSource — rodio Source backed by a background decode thread ──────

struct StreamingSource {
    rx:          std::sync::mpsc::Receiver<Vec<f32>>,
    buf:         Vec<f32>,
    pos:         usize,
    sample_rate: u32,
    channels:    u16,
}

impl Iterator for StreamingSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if self.pos < self.buf.len() {
                let v = self.buf[self.pos];
                self.pos += 1;
                return Some(v);
            }
            match self.rx.try_recv() {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                // Channel empty — block until decoder sends a chunk.
                // try_recv + immediate Some(0.0) caused the rodio mixer thread to spin
                // at ~96 000 calls/sec (sample_rate × channels) burning a full CPU core
                // whenever the decoder was briefly behind.  Blocking here is safe: the
                // channel holds CHANNEL_CAP_DEFAULT chunks (~680 ms at 48 kHz stereo),
                // so we only block during a genuine underrun.
                // E-28: on Disconnected we still return Some(0.0) — never None.
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Use recv_timeout instead of blocking recv so the rodio mixer thread
                    // is never stalled longer than one audio device buffer period (~20 ms).
                    match self.rx.recv_timeout(std::time::Duration::from_millis(20)) {
                        Ok(chunk)                                            => { self.buf = chunk; self.pos = 0; }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)     => return Some(0.0), // brief underrun → silence
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            tracing::warn!("audio decoder thread disconnected — dropping sink");
                            return None;
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    tracing::warn!("audio decoder channel disconnected — dropping sink");
                    return None;
                }
            }
        }
    }
}

impl rodio::Source for StreamingSource {
    fn current_frame_len(&self) -> Option<usize>     { None }
    fn channels(&self)          -> u16                { self.channels }
    fn sample_rate(&self)       -> u32                { self.sample_rate }
    fn total_duration(&self)    -> Option<Duration>   { None }
}

// ─── Background decode thread ─────────────────────────────────────────────────

/// Spawn a decode thread that loops through `path` indefinitely,
/// sending f32 stereo@48kHz chunks into the returned Receiver.
///
/// H-1: No blocking await — returns immediately; decode runs in background.
/// H-2: When the Receiver is dropped (sink stopped), the SyncSender gets an
///      error and the thread exits (E-29 pattern).
fn start_streaming_decoder(
    path:       String,
    chunk_size: usize,
    chan_cap:   usize,
) -> std::sync::mpsc::Receiver<Vec<f32>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(chan_cap);
    std::thread::Builder::new()
        .name("wpick-audio-dec".into())
        .spawn(move || {
            ffmpeg::init().ok(); // idempotent
            // Loop until channel closed or file error (E-29).
            while decode_and_send(&path, &tx, chunk_size).is_ok() {}
        })
        .ok();
    rx
}

/// Decode one complete pass of `path` (any format with an audio stream),
/// resampled to f32 stereo @ 48 kHz, and send `chunk_size`-sized chunks to `tx`.
/// Returns Err when the receiver has been dropped (time to stop looping).
fn decode_and_send(
    path:       &str,
    tx:         &std::sync::mpsc::SyncSender<Vec<f32>>,
    chunk_size: usize,
) -> anyhow::Result<()> {
    use ffmpeg::software::resampling::context::Context as Resampler;

    let mut ctx = ffmpeg::format::input(&path)?;

    let audio_stream = ctx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or_else(|| anyhow::anyhow!("no audio stream"))?;
    let stream_idx = audio_stream.index();

    let mut decoder =
        ffmpeg::codec::context::Context::from_parameters(audio_stream.parameters())?
            .decoder()
            .audio()?;

    let mut resampler = Resampler::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
        ffmpeg::ChannelLayout::STEREO,
        48_000,
    )?;

    let mut chunk = Vec::<f32>::with_capacity(chunk_size);

    // Flush a full chunk to the channel; returns Err if receiver dropped.
    let flush_chunk = |chunk: Vec<f32>| -> anyhow::Result<()> {
        tx.send(chunk).map_err(|_| anyhow::anyhow!("channel closed"))
    };

    for (stream, packet) in ctx.packets() {
        if stream.index() != stream_idx { continue; }
        decoder.send_packet(&packet).ok();
        let mut frame = ffmpeg::frame::Audio::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            let mut out = ffmpeg::frame::Audio::empty();
            resampler.run(&frame, &mut out).ok();
            for c in out.data(0).chunks_exact(4) {
                chunk.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                if chunk.len() >= chunk_size {
                    flush_chunk(std::mem::replace(&mut chunk, Vec::with_capacity(chunk_size)))?;
                }
            }
        }
    }

    // Flush resampler tail
    let mut out = ffmpeg::frame::Audio::empty();
    if resampler.flush(&mut out).is_ok() {
        for c in out.data(0).chunks_exact(4) {
            chunk.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            if chunk.len() >= chunk_size {
                flush_chunk(std::mem::replace(&mut chunk, Vec::with_capacity(chunk_size)))?;
            }
        }
    }

    if !chunk.is_empty() {
        flush_chunk(chunk)?;
    }

    Ok(())
}

// ─── Volume/mute helper ───────────────────────────────────────────────────────

fn apply_volume(sink: &rodio::Sink, vol: f32, muted: bool) {
    // Never pause — pausing blocks the decode thread's SyncSender when the
    // channel fills up, causing a stuck thread + audio delay on unmute.
    // Silence is achieved by volume=0.0 while the source keeps draining.
    sink.set_volume(if muted { 0.0 } else { vol.clamp(0.0, 1.0) });
}

// ─── Public async run loop ────────────────────────────────────────────────────

pub async fn run(
    duck:             crate::ducking::DuckHandle,
    mut wallpaper_rx: tokio::sync::watch::Receiver<Option<WallpaperInfo>>,
    mut volume_rx:    tokio::sync::watch::Receiver<(f32, bool)>,
    audio_cfg:        AudioConfig,
) -> anyhow::Result<()> {
    let chunk_size = if audio_cfg.chunk_frames > 0 { audio_cfg.chunk_frames } else { CHUNK_SIZE_DEFAULT };
    // Cap channel capacity so that chunk_size × cap stays under max_preload_mb.
    let chan_cap = {
        let bytes_per_chunk = chunk_size * 4; // f32 = 4 bytes
        let max_bytes = (audio_cfg.max_preload_mb as usize).saturating_mul(1_048_576);
        let cap = if max_bytes > 0 { max_bytes / bytes_per_chunk } else { CHANNEL_CAP_DEFAULT };
        cap.clamp(2, 64)
    };
    tracing::info!("Audio task started (chunk={} cap={})", chunk_size, chan_cap);

    let (_output_stream, stream_handle) = rodio::OutputStream::try_default()
        .map_err(|e| anyhow::anyhow!("Audio output init failed: {}", e))?;

    tracing::info!("Audio OutputStream ready");

    let mut current_sink: Option<Arc<rodio::Sink>> = None;

    loop {
        // B-3: tokio::select! replaces the 50ms polling loop — events wake
        // this task immediately, zero CPU burn between changes.
        tokio::select! {
            res = wallpaper_rx.changed() => {
                res?; // Sender dropped → run() exits cleanly
                let new_wp = wallpaper_rx.borrow_and_update().clone();

                // Fade out old sink smoothly instead of stopping it abruptly.
                // The task owns the Arc exclusively (current_sink.take()), so
                // volume ramps can't race with the volume_rx handler.
                // When the ramp completes (or the sink drains naturally), stop()
                // drops the StreamingSource Receiver → decoder thread exits.
                if let Some(old_sink) = current_sink.take() {
                    tokio::spawn(async move {
                        let start_vol = old_sink.volume();
                        let step_ms   = AUDIO_FADE_MS / AUDIO_FADE_STEPS as u64;
                        for i in (0..AUDIO_FADE_STEPS).rev() {
                            if old_sink.empty() { break; }
                            old_sink.set_volume(start_vol * i as f32 / AUDIO_FADE_STEPS as f32);
                            tokio::time::sleep(std::time::Duration::from_millis(step_ms)).await;
                        }
                        old_sink.stop();
                    });
                }

                if let Some(ref info) = new_wp {
                    if info.has_audio {
                        // H-1: Non-blocking — returns immediately, decode runs in background.
                        // H-2: No .await here so watch-channel can't race with a blocking decode.
                        let rx = start_streaming_decoder(
                            info.file_path.clone(), chunk_size, chan_cap,
                        );

                        match rodio::Sink::try_new(&stream_handle) {
                            Ok(sink) => {
                                sink.append(StreamingSource {
                                    rx,
                                    buf: Vec::new(),
                                    pos: 0,
                                    sample_rate: 48_000,
                                    channels:    2,
                                });
                                let sink = Arc::new(sink);

                                let (vol, muted) = *volume_rx.borrow_and_update();
                                apply_volume(&sink, vol, muted);
                                // H-3: pass muted so ducking never overrides mute.
                                duck.register_sink(Arc::clone(&sink), vol, muted);
                                tracing::info!(
                                    "Audio streaming: {} vol={:.0}% muted={}",
                                    info.title, vol * 100.0, muted,
                                );
                                current_sink = Some(sink);
                            }
                            Err(e) => tracing::warn!("Sink creation failed: {}", e),
                        }
                    }
                }
            }

            res = volume_rx.changed() => {
                res?;
                let (vol, muted) = *volume_rx.borrow_and_update();
                // H-3: keep ducking and the sink in sync on every volume change.
                duck.update_volume(vol, muted);
                if let Some(ref sink) = current_sink {
                    apply_volume(sink, vol, muted);
                }
                tracing::debug!("Volume: {:.0}% muted={}", vol * 100.0, muted);
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// StreamingSource returns None (end-of-stream) when the sender is disconnected.
    /// None causes rodio to drop the sink, which is correct — a dead decoder should
    /// not keep a silent sink alive forever.
    #[test]
    fn test_streaming_source_ends_on_disconnect() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(1);
        drop(tx);
        let mut src = StreamingSource { rx, buf: Vec::new(), pos: 0, sample_rate: 48_000, channels: 2 };
        assert_eq!(src.next(), None, "disconnected sender must yield None to end the sink");
    }

    /// StreamingSource drains a pre-loaded chunk then returns None on decoder death.
    #[test]
    fn test_streaming_source_drains_chunk_then_ends() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(1);
        tx.send(vec![1.0_f32, 2.0, 3.0]).unwrap();
        drop(tx); // disconnect after first chunk
        let mut src = StreamingSource { rx, buf: Vec::new(), pos: 0, sample_rate: 48_000, channels: 2 };
        assert_eq!(src.next(), Some(1.0));
        assert_eq!(src.next(), Some(2.0));
        assert_eq!(src.next(), Some(3.0));
        assert_eq!(src.next(), None, "after chunk exhausted and sender dropped: end-of-stream");
    }

    /// StreamingSource transitions across chunk boundaries correctly.
    #[test]
    fn test_streaming_source_multi_chunk() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(4);
        tx.send(vec![0.1_f32, 0.2]).unwrap();
        tx.send(vec![0.3_f32, 0.4]).unwrap();
        drop(tx);
        let mut src = StreamingSource { rx, buf: Vec::new(), pos: 0, sample_rate: 48_000, channels: 2 };
        let vals: Vec<f32> = (0..4).map(|_| src.next().unwrap()).collect();
        assert_eq!(vals, vec![0.1, 0.2, 0.3, 0.4]);
    }

    /// rodio reads sample_rate/channels from the Source trait to configure its
    /// resampler.  Wrong values would cause pitch shift or stereo collapse.
    #[test]
    fn test_streaming_source_metadata() {
        use rodio::Source;
        let (_tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(1);
        let src = StreamingSource { rx, buf: Vec::new(), pos: 0, sample_rate: 48_000, channels: 2 };
        assert_eq!(src.sample_rate(), 48_000, "sample_rate must be 48 kHz");
        assert_eq!(src.channels(),    2,      "must be stereo");
        assert!(src.current_frame_len().is_none(), "streaming source has no fixed frame len");
        assert!(src.total_duration().is_none(),    "streaming source has no total duration");
    }

    /// apply_volume must set volume without ever pausing the sink.
    /// Pausing the sink blocks the decode thread's SyncSender when the channel fills.
    #[test]
    fn test_apply_volume_never_pauses_sink() {
        // Skip cleanly when no audio output device is available (CI without sound card).
        let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
            eprintln!("no audio output device — skipping apply_volume test");
            return;
        };
        let Ok(sink) = rodio::Sink::try_new(&handle) else { return; };

        apply_volume(&sink, 0.5, false);
        assert!((sink.volume() - 0.5).abs() < 1e-4, "volume should be 0.5 when not muted");
        assert!(!sink.is_paused(), "sink must not be paused when not muted");

        apply_volume(&sink, 0.8, true);
        assert!(sink.volume() < 1e-4, "volume should be 0.0 when muted");
        assert!(!sink.is_paused(), "sink must NEVER be paused (blocks decode thread)");
    }
}
