use std::time::Duration;

use anyhow::Context;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::flag::Flags;

#[allow(dead_code)]
pub struct VideoDecoder {
    input_ctx:        ffmpeg::format::context::Input,
    video_stream_idx: usize,
    decoder:          ffmpeg::codec::decoder::Video,
    scaler:           ffmpeg::software::scaling::context::Context,
    fps:              f64,
    // v0.2: reused across every next_frame_rgba call — zero hot-path allocations
    // once the Vec grows to frame size (width * height * 4 bytes).
    frame_buf:        Vec<u8>,
}

#[allow(dead_code)]
impl VideoDecoder {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        ffmpeg::init().context("ffmpeg::init failed")?;

        let input_ctx = ffmpeg::format::input(&path)
            .context("Failed to open video file")?;

        let stream = input_ctx
            .streams()
            .best(Type::Video)
            .ok_or_else(|| anyhow::anyhow!("No video stream found in {}", path))?;

        let video_stream_idx = stream.index();

        let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .context("Failed to create codec context")?;

        let decoder = context.decoder().video()
            .context("Failed to create video decoder")?;

        let fps = {
            let r = stream.avg_frame_rate();
            let v = r.numerator() as f64 / r.denominator().max(1) as f64;
            if v < 1.0 {
                tracing::warn!(
                    "Video reports fps={:.3} (num={} den={}) — clamping to 1.0",
                    v, r.numerator(), r.denominator()
                );
            }
            v.max(1.0)
        };

        let scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            Pixel::RGBA,
            decoder.width(),
            decoder.height(),
            Flags::BILINEAR,
        )
        .context("Failed to create scaler context")?;

        // Pre-size the buffer so the first frame doesn't trigger a realloc.
        let frame_buf = Vec::with_capacity(
            (decoder.width() * decoder.height() * 4) as usize,
        );

        Ok(Self {
            input_ctx,
            video_stream_idx,
            decoder,
            scaler,
            fps,
            frame_buf,
        })
    }

    /// Decode the next video frame into `self.frame_buf` and return a slice of it.
    ///
    /// Returns `Ok(None)` at EOF. The returned slice borrows `self` — the
    /// caller must not call any other `&mut self` method while the slice is live.
    /// In `renderer.rs` this is fine: `queue.write_texture` consumes the slice
    /// immediately and the frame loop proceeds on the next iteration.
    pub fn next_frame_rgba(&mut self) -> anyhow::Result<Option<(&[u8], u32, u32)>> {
        loop {
            match self.input_ctx.packets().next() {
                None => return Ok(None),
                Some((stream, packet)) => {
                    if stream.index() != self.video_stream_idx {
                        continue;
                    }

                    self.decoder.send_packet(&packet)
                        .context("send_packet failed")?;

                    let mut decoded = ffmpeg::frame::Video::empty();
                    if self.decoder.receive_frame(&mut decoded).is_ok() {
                        let mut rgba_frame = ffmpeg::frame::Video::empty();
                        self.scaler.run(&decoded, &mut rgba_frame)
                            .context("scaler run failed")?;

                        let width     = rgba_frame.width();
                        let height    = rgba_frame.height();
                        let stride    = rgba_frame.stride(0);
                        let src       = rgba_frame.data(0);
                        let row_bytes = (width * 4) as usize;

                        self.frame_buf.clear();

                        if stride == row_bytes {
                            // Packed layout — one shot
                            let needed = row_bytes * height as usize;
                            if src.len() < needed {
                                tracing::warn!(
                                    "Corrupt frame: src.len()={} < needed={}, skipping",
                                    src.len(), needed
                                );
                                continue;
                            }
                            self.frame_buf.extend_from_slice(&src[..needed]);
                        } else {
                            // ffmpeg added per-row padding — strip it
                            self.frame_buf.reserve(row_bytes * height as usize);
                            let mut corrupt = false;
                            for row in 0..height as usize {
                                let start = row * stride;
                                let end   = start + row_bytes;
                                if end > src.len() {
                                    tracing::warn!(
                                        "Corrupt frame: stride={} row_bytes={} src.len()={} at row={}, skipping",
                                        stride, row_bytes, src.len(), row
                                    );
                                    corrupt = true;
                                    break;
                                }
                                self.frame_buf.extend_from_slice(&src[start..end]);
                            }
                            if corrupt {
                                self.frame_buf.clear();
                                continue;
                            }
                        }

                        return Ok(Some((&self.frame_buf, width, height)));
                    }
                }
            }
        }
    }

    pub fn seek_to_start(&mut self) -> anyhow::Result<()> {
        self.input_ctx.seek(0, ..).context("seek failed")?;
        self.decoder.flush();
        Ok(())
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps)
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.decoder.width(), self.decoder.height())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_VIDEO: &str = "/tmp/wpick_test.mp4";

    #[test]
    fn test_open_valid_file() {
        let dec = VideoDecoder::open(TEST_VIDEO);
        assert!(dec.is_ok(), "open failed: {:?}", dec.err());
        let dec = dec.unwrap();
        assert_eq!(dec.dimensions(), (320, 240));
        assert!(dec.fps > 0.0);
    }

    #[test]
    fn test_decode_first_frame() {
        let mut dec = VideoDecoder::open(TEST_VIDEO).unwrap();
        let frame = dec.next_frame_rgba().unwrap();
        assert!(frame.is_some());
        let (data, w, h) = frame.unwrap();
        assert_eq!(w, 320);
        assert_eq!(h, 240);
        assert_eq!(data.len(), (320 * 240 * 4) as usize);
        let _ = data[0];
    }

    #[test]
    fn test_eof_returns_none() {
        let mut dec = VideoDecoder::open(TEST_VIDEO).unwrap();
        let mut count = 0usize;
        // .is_some() on the result drops the temporary (including the &[u8] slice)
        // before the next iteration, so dec is free to be borrowed again.
        while dec.next_frame_rgba().unwrap().is_some() {
            count += 1;
        }
        assert!(count > 0, "no frames decoded");
    }

    #[test]
    fn test_seek_to_start() {
        let mut dec = VideoDecoder::open(TEST_VIDEO).unwrap();
        // Evaluate .is_some() so the temporary slice is dropped before seek.
        let before = dec.next_frame_rgba().unwrap().is_some();
        dec.seek_to_start().unwrap();
        let after = dec.next_frame_rgba().unwrap().is_some();
        assert!(before);
        assert!(after);
    }

    #[test]
    fn test_frame_duration_reasonable() {
        let dec = VideoDecoder::open(TEST_VIDEO).unwrap();
        let dur = dec.frame_duration();
        assert!(
            dur.as_millis() > 10 && dur.as_millis() < 200,
            "unexpected duration: {:?}",
            dur
        );
    }

    /// Verify that two consecutive calls both produce valid frames and that
    /// the buffer is reused (no panic, correct size on both calls).
    #[test]
    fn test_consecutive_frames_buffer_reuse() {
        let mut dec = VideoDecoder::open(TEST_VIDEO).unwrap();

        // First call — copy data out before second call overwrites frame_buf.
        let (w1, h1, len1) = {
            let (data, w, h) = dec.next_frame_rgba().unwrap()
                .expect("expected frame 1");
            assert_eq!(data.len(), (w * h * 4) as usize, "frame 1 size mismatch");
            (w, h, data.len())
            // data's borrow of dec ends here (block closes)
        };

        // Second call — dec is free again; frame_buf is overwritten in-place.
        let (data2, w2, h2) = dec.next_frame_rgba().unwrap()
            .expect("expected frame 2");
        assert_eq!((w2, h2), (w1, h1), "dimensions must match between frames");
        assert_eq!(data2.len(), len1, "buffer size must match between frames");
    }
}
