/// Audio playback task.
///
/// Fixes applied here:
///   H-1 — full audio in RAM: replaced one-shot Vec decode with streaming background thread.
///   H-2 — watch-channel race during decode: streaming is non-blocking, no await during decode.
///   H-3 — set_sink ignored mute: muted state passed to DuckHandle on every sink creation/update.
use std::sync::Arc;
use std::time::Duration;

use ffmpeg_next as ffmpeg;
use wpick_core::model::WallpaperInfo;

// ─── Streaming constants ──────────────────────────────────────────────────────

/// Samples per chunk sent from the decode thread to StreamingSource.
/// 8 192 samples ≈ 170 ms at 48 kHz stereo — small enough for quick startup,
/// large enough to avoid excessive channel traffic.
const CHUNK_SIZE: usize = 8_192;

/// Channel capacity in chunks (backpressure bound).
/// 16 chunks × 170 ms ≈ 2.7 s of audio buffer — ample margin for a decode thread.
const CHANNEL_CAP: usize = 16;

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
                // E-28: Never return None — rodio treats None as "source exhausted"
                // and silently drops the sink. Return silence on empty channel or
                // disconnected sender so the sink stays alive.
                Err(_) => return Some(0.0),
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
fn start_streaming_decoder(path: String) -> std::sync::mpsc::Receiver<Vec<f32>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(CHANNEL_CAP);
    std::thread::Builder::new()
        .name("wpick-audio-dec".into())
        .spawn(move || {
            ffmpeg::init().ok(); // idempotent
            loop {
                match decode_and_send(&path, &tx) {
                    Ok(())  => {} // EOF — loop seamlessly
                    Err(_)  => break, // E-29: channel closed or file error
                }
            }
        })
        .ok();
    rx
}

/// Decode one complete pass of `path` (any format with an audio stream),
/// resampled to f32 stereo @ 48 kHz, and send CHUNK_SIZE-sized chunks to `tx`.
/// Returns Err when the receiver has been dropped (time to stop looping).
fn decode_and_send(
    path: &str,
    tx:   &std::sync::mpsc::SyncSender<Vec<f32>>,
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

    let mut chunk = Vec::<f32>::with_capacity(CHUNK_SIZE);

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
                if chunk.len() >= CHUNK_SIZE {
                    flush_chunk(std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK_SIZE)))?;
                }
            }
        }
    }

    // Flush resampler tail
    let mut out = ffmpeg::frame::Audio::empty();
    if resampler.flush(&mut out).is_ok() {
        for c in out.data(0).chunks_exact(4) {
            chunk.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            if chunk.len() >= CHUNK_SIZE {
                flush_chunk(std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK_SIZE)))?;
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
    if muted {
        sink.set_volume(0.0);
        sink.pause();
    } else {
        sink.set_volume(vol.clamp(0.0, 1.0));
        sink.play();
    }
}

// ─── Public async run loop ────────────────────────────────────────────────────

pub async fn run(
    duck:             crate::ducking::DuckHandle,
    mut wallpaper_rx: tokio::sync::watch::Receiver<Option<WallpaperInfo>>,
    mut volume_rx:    tokio::sync::watch::Receiver<(f32, bool)>,
) -> anyhow::Result<()> {
    tracing::info!("Audio task started");

    let (_output_stream, stream_handle) = rodio::OutputStream::try_default()
        .map_err(|e| anyhow::anyhow!("Audio output init failed: {}", e))?;

    tracing::info!("Audio OutputStream ready");

    let mut current_sink: Option<Arc<rodio::Sink>> = None;

    loop {
        if wallpaper_rx.has_changed()? {
            let new_wp = wallpaper_rx.borrow_and_update().clone();

            // Stop old sink — this drops StreamingSource's Receiver, signalling
            // the decode thread to exit (E-29).
            if let Some(sink) = current_sink.take() {
                sink.stop();
            }

            if let Some(ref info) = new_wp {
                if info.has_audio {
                    // H-1: Non-blocking — returns immediately, decode runs in background.
                    // H-2: No .await here so watch-channel can't race with a blocking decode.
                    let rx = start_streaming_decoder(info.file_path.clone());

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

                            // borrow_and_update clears the "changed" flag so the
                            // volume branch below doesn't double-apply this tick.
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

        if volume_rx.has_changed()? {
            let (vol, muted) = *volume_rx.borrow_and_update();
            // H-3: keep ducking and the sink in sync on every volume change.
            duck.update_volume(vol, muted);
            if let Some(ref sink) = current_sink {
                apply_volume(sink, vol, muted);
            }
            tracing::debug!("Volume: {:.0}% muted={}", vol * 100.0, muted);
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// StreamingSource returns silence (Some(0.0)) when the channel is empty — never None.
    /// Returning None would cause rodio to drop the sink (E-28).
    #[test]
    fn test_streaming_source_silence_on_empty_channel() {
        let (_tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(1);
        let mut src = StreamingSource { rx, buf: Vec::new(), pos: 0, sample_rate: 48_000, channels: 2 };
        assert_eq!(src.next(), Some(0.0), "must return silence, not None");
    }

    /// StreamingSource returns silence when the sender is disconnected.
    #[test]
    fn test_streaming_source_silence_on_disconnect() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(1);
        drop(tx);
        let mut src = StreamingSource { rx, buf: Vec::new(), pos: 0, sample_rate: 48_000, channels: 2 };
        assert_eq!(src.next(), Some(0.0), "disconnected sender must yield silence");
    }

    /// StreamingSource drains a pre-loaded chunk then returns silence on underrun.
    #[test]
    fn test_streaming_source_drains_chunk_then_silence() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(1);
        tx.send(vec![1.0_f32, 2.0, 3.0]).unwrap();
        drop(tx); // disconnect after first chunk
        let mut src = StreamingSource { rx, buf: Vec::new(), pos: 0, sample_rate: 48_000, channels: 2 };
        assert_eq!(src.next(), Some(1.0));
        assert_eq!(src.next(), Some(2.0));
        assert_eq!(src.next(), Some(3.0));
        assert_eq!(src.next(), Some(0.0), "after chunk exhausted: silence");
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
}
