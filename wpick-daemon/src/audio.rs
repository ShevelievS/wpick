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
    fn current_frame_len(&self) -> Option<usize>             { None }
    fn channels(&self)          -> u16                        { self.channels }
    fn sample_rate(&self)       -> u32                        { self.sample_rate }
    fn total_duration(&self)    -> Option<std::time::Duration> { None }
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

    // Flush resampler
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
    mut pause_rx:     tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // _output_stream must outlive every Sink created from stream_handle (E-16)
    let (_output_stream, stream_handle) = rodio::OutputStream::try_default()
        .map_err(|e| anyhow::anyhow!("Audio output init failed: {}", e))?;

    let mut current_sink: Option<Arc<rodio::Sink>> = None;

    loop {
        if wallpaper_rx.has_changed()? {
            let new_wp = wallpaper_rx.borrow_and_update().clone();

            // Stop current audio
            if let Some(sink) = current_sink.take() {
                sink.stop();
            }

            if let Some(ref info) = new_wp {
                if info.has_audio {
                    let path  = info.file_path.clone();
                    let title = info.title.clone();

                    match tokio::task::spawn_blocking(move || decode_audio_to_f32(&path)).await? {
                        Ok((samples, rate, ch)) => {
                            match rodio::Sink::try_new(&stream_handle) {
                                Ok(sink) => {
                                    let (vol, muted) = *volume_rx.borrow();
                                    sink.set_volume(if muted { 0.0 } else { vol });
                                    sink.append(AudioSamples {
                                        samples,
                                        pos: 0,
                                        sample_rate: rate,
                                        channels: ch,
                                    });
                                    let sink = Arc::new(sink);
                                    // Register with ducker so it can fade volume
                                    duck.register_sink(Arc::clone(&sink), vol);
                                    current_sink = Some(sink);
                                    tracing::info!("Audio started for: {}", title);
                                }
                                Err(e) => tracing::warn!("Sink creation failed: {}", e),
                            }
                        }
                        Err(e) => {
                            tracing::info!("No audio decoded for wallpaper: {}", e);
                        }
                    }
                }
            }
        }

        if volume_rx.has_changed()? {
            let (vol, muted) = *volume_rx.borrow_and_update();
            // Update ducker's target volume so fade-in restores the right level
            duck.set_volume(if muted { 0.0 } else { vol });
            // Also apply directly if not currently ducked
            if let Some(ref sink) = current_sink {
                sink.set_volume(if muted { 0.0 } else { vol });
            }
        }

        if pause_rx.has_changed()? {
            let paused = *pause_rx.borrow_and_update();
            if let Some(ref sink) = current_sink {
                if paused { sink.pause(); } else { sink.play(); }
            }
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

    #[test]
    fn test_decode_audio_from_file() {
        let result = decode_audio_to_f32("/tmp/wpick_test_audio.mp4");
        assert!(result.is_ok(), "decode failed: {:?}", result.err());
        let (samples, rate, ch) = result.unwrap();
        assert_eq!(rate, 48000);
        assert_eq!(ch, 2);
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn test_no_audio_stream_returns_error() {
        let result = decode_audio_to_f32("/tmp/wpick_test.mp4");
        assert!(result.is_err());
    }
}