use std::sync::Arc;

use anyhow::Context;
use ffmpeg_next as ffmpeg;
use wpick_core::model::WallpaperInfo;

// ─── AudioSamples — looping rodio Source ─────────────────────────────────────

struct AudioSamples {
    samples:     Vec<f32>,
    pos:         usize,
    sample_rate: u32,
    channels:    u16,
}

impl Iterator for AudioSamples {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.samples.is_empty() { return None; }
        let val  = self.samples[self.pos];
        self.pos = (self.pos + 1) % self.samples.len();
        Some(val)
    }
}

impl rodio::Source for AudioSamples {
    fn current_frame_len(&self) -> Option<usize>              { None }
    fn channels(&self)          -> u16                         { self.channels }
    fn sample_rate(&self)       -> u32                         { self.sample_rate }
    fn total_duration(&self)    -> Option<std::time::Duration> { None }
}

// ─── Volume/mute helper ───────────────────────────────────────────────────────
//
// rodio 0.19 + ALSA/PipeWire: set_volume() is ignored for playing sinks.
// Use sink.pause() for mute and sink.play() for unmute instead.
fn apply_volume(sink: &rodio::Sink, vol: f32, muted: bool) {
    if muted {
        sink.set_volume(0.0);
        sink.pause();
    } else {
        sink.set_volume(vol.clamp(0.0, 1.0));
        sink.play();
    }
}

// ─── Audio decode ─────────────────────────────────────────────────────────────

fn decode_audio_to_f32(path: &str) -> anyhow::Result<(Vec<f32>, u32, u16)> {
    ffmpeg::init().context("ffmpeg::init failed")?;

    let mut ctx = ffmpeg::format::input(&path)
        .context("Failed to open audio file")?;

    let audio_stream = ctx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or_else(|| anyhow::anyhow!("No audio stream in {}", path))?;

    let stream_idx = audio_stream.index();

    let mut decoder =
        ffmpeg::codec::context::Context::from_parameters(audio_stream.parameters())
            .context("codec context")?
            .decoder()
            .audio()
            .context("audio decoder")?;

    use ffmpeg::software::resampling::context::Context as Resampler;
    let mut resampler = Resampler::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
        ffmpeg::ChannelLayout::STEREO,
        48000,
    ).context("resampler init")?;

    let mut samples = Vec::<f32>::new();

    for (stream, packet) in ctx.packets() {
        if stream.index() != stream_idx { continue; }
        decoder.send_packet(&packet).context("send_packet")?;
        let mut frame = ffmpeg::frame::Audio::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            let mut resampled = ffmpeg::frame::Audio::empty();
            resampler.run(&frame, &mut resampled).context("resample run")?;
            let data = resampled.data(0);
            for chunk in data.chunks_exact(4) {
                samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
    }

    let mut resampled = ffmpeg::frame::Audio::empty();
    if resampler.flush(&mut resampled).is_ok() {
        let data = resampled.data(0);
        for chunk in data.chunks_exact(4) {
            samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }

    Ok((samples, 48000, 2))
}

// ─── Public async run loop ────────────────────────────────────────────────────

pub async fn run(
    duck:             crate::ducking::DuckHandle,
    mut wallpaper_rx: tokio::sync::watch::Receiver<Option<WallpaperInfo>>,
    mut volume_rx:    tokio::sync::watch::Receiver<(f32, bool)>,
) -> anyhow::Result<()> {
    let (_output_stream, stream_handle) = rodio::OutputStream::try_default()
        .map_err(|e| anyhow::anyhow!("Audio output init failed: {}", e))?;

    let mut current_sink: Option<Arc<rodio::Sink>> = None;

    loop {
        if wallpaper_rx.has_changed()? {
            let new_wp = wallpaper_rx.borrow_and_update().clone();

            if let Some(sink) = current_sink.take() {
                sink.stop();
            }

            if let Some(ref info) = new_wp {
                if info.has_audio {
                    let path  = info.file_path.clone();
                    let title = info.title.clone();

                    match tokio::task::spawn_blocking(move || {
                        decode_audio_to_f32(&path)
                    }).await? {
                        Ok((samples, rate, ch)) => {
                            match rodio::Sink::try_new(&stream_handle) {
                                Ok(sink) => {
                                    sink.append(AudioSamples {
                                        samples,
                                        pos: 0,
                                        sample_rate: rate,
                                        channels:    ch,
                                    });

                                    let sink = Arc::new(sink);

                                    // Read latest mute/volume — borrow_and_update
                                    // so we get state set during long decode
                                    let (vol, muted) = *volume_rx.borrow_and_update();
                                    apply_volume(&sink, vol, muted);

                                    duck.register_sink(Arc::clone(&sink), vol);

                                    tracing::info!("Audio: {} vol={:.0}% muted={}",
                                        title, vol * 100.0, muted);

                                    current_sink = Some(sink);
                                }
                                Err(e) => tracing::warn!("Sink creation failed: {}", e),
                            }
                        }
                        Err(e) => tracing::info!("No audio: {}", e),
                    }
                }
            }
        }

        if volume_rx.has_changed()? {
            let (vol, muted) = *volume_rx.borrow_and_update();
            duck.set_volume(vol);
            if let Some(ref sink) = current_sink {
                apply_volume(sink, vol, muted);
            }
            tracing::debug!("Volume: {:.0}% muted={}", vol * 100.0, muted);
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_samples_loops() {
        let src = AudioSamples {
            samples: vec![1.0_f32, 2.0, 3.0],
            pos: 0, sample_rate: 48000, channels: 2,
        };
        let collected: Vec<f32> = src.take(7).collect();
        assert_eq!(collected, vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0, 1.0]);
    }

    #[test]
    fn test_audio_samples_empty_returns_none() {
        let mut src = AudioSamples {
            samples: vec![], pos: 0, sample_rate: 48000, channels: 2,
        };
        assert_eq!(src.next(), None);
    }
}