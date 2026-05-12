use std::time::Duration;

use anyhow::Context;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::flag::Flags;

pub struct VideoDecoder {
    input_ctx:        ffmpeg::format::context::Input,
    video_stream_idx: usize,
    decoder:          ffmpeg::codec::decoder::Video,
    scaler:           ffmpeg::software::scaling::context::Context,
    fps:              f64,
    /// Set to true once we've sent a flush (null) packet so we stop reading.
    eof_sent:         bool,
}

impl VideoDecoder {
    /// Open a software video decoder.
    ///
    /// `target_w` × `target_h` is the screen/canvas size.  The scaler is
    /// configured once here to output BGRA at those exact dimensions, so the
    /// render loop can memcpy directly into the SHM slot — no per-frame
    /// scaling or colour-conversion math.
    pub fn open(path: &str, target_w: u32, target_h: u32) -> anyhow::Result<Self> {
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

        // Scaler: source native format → BGRA at screen dimensions.
        // One SIMD pass replaces the former (RGBA + manual scale-in-renderer).
        let scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            Pixel::BGRA,
            target_w,
            target_h,
            Flags::BILINEAR,
        )
        .context("Failed to create scaler context")?;

        Ok(Self {
            input_ctx,
            video_stream_idx,
            decoder,
            scaler,
            fps,
            eof_sent: false,
        })
    }

    /// Decode the next video frame and write it as BGRA directly into `dst`.
    ///
    /// `dst` must be exactly `target_w * target_h * 4` bytes (the SHM slot).
    ///
    /// Returns `Ok(true)` when a frame was written, `Ok(false)` at EOF.
    /// On EOF the caller should call `seek_to_start()` for a seamless loop.
    ///
    /// Uses the standard ffmpeg drain pattern (receive → send → send_eof → drain)
    /// so all buffered B/P frames are emitted before returning EOF.
    pub fn next_frame_bgra(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        loop {
            // ── 1. Try to receive a frame from the decoder first ──────────────
            let mut decoded = ffmpeg::frame::Video::empty();
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                let mut bgra_frame = ffmpeg::frame::Video::empty();
                self.scaler.run(&decoded, &mut bgra_frame)
                    .context("scaler run failed")?;

                let width     = bgra_frame.width()  as usize;
                let height    = bgra_frame.height() as usize;
                let stride    = bgra_frame.stride(0);
                let src       = bgra_frame.data(0);
                let row_bytes = width * 4;
                let needed    = row_bytes * height;

                if dst.len() < needed {
                    tracing::warn!("dst too small: {} < {} — skipping frame", dst.len(), needed);
                    continue;
                }

                if stride == row_bytes {
                    if src.len() < needed {
                        tracing::warn!(
                            "Corrupt frame: src.len()={} < needed={}, skipping",
                            src.len(), needed
                        );
                        continue;
                    }
                    dst[..needed].copy_from_slice(&src[..needed]);
                } else {
                    // ffmpeg added per-row padding — strip it
                    let mut corrupt = false;
                    for row in 0..height {
                        let src_start = row * stride;
                        let src_end   = src_start + row_bytes;
                        if src_end > src.len() {
                            tracing::warn!(
                                "Corrupt frame row {}: stride={} src.len()={}, skipping",
                                row, stride, src.len()
                            );
                            corrupt = true;
                            break;
                        }
                        let dst_start = row * row_bytes;
                        dst[dst_start..dst_start + row_bytes]
                            .copy_from_slice(&src[src_start..src_end]);
                    }
                    if corrupt { continue; }
                }

                return Ok(true);
            }

            // ── 2. Decoder has no more frames — feed it ───────────────────────
            if self.eof_sent {
                // Decoder fully drained
                return Ok(false);
            }

            match self.input_ctx.packets().next() {
                None => {
                    // File exhausted: flush decoder to get remaining buffered frames
                    // (H.264/H.265 B-frame delay = up to ~16 frames without this).
                    self.decoder.send_eof().ok();
                    self.eof_sent = true;
                    // loop back to drain
                }
                Some((stream, packet)) => {
                    if stream.index() != self.video_stream_idx {
                        continue;
                    }
                    self.decoder.send_packet(&packet)
                        .context("send_packet failed")?;
                }
            }
        }
    }

    pub fn seek_to_start(&mut self) -> anyhow::Result<()> {
        self.input_ctx.seek(0, ..).context("seek failed")?;
        self.decoder.flush();
        self.eof_sent = false;
        Ok(())
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps)
    }

    /// Source video dimensions (for logging).
    pub fn dimensions(&self) -> (u32, u32) {
        (self.decoder.width(), self.decoder.height())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate (once) a 2-second 320×240 30fps H.264 test video with a stereo
    /// audio track.  Returns `None` when ffmpeg is not in PATH — every test that
    /// needs the file must skip gracefully when this returns `None`.
    fn test_video_path() -> Option<&'static str> {
        static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        const PATH: &str = "/tmp/wpick_test.mp4";
        let ok = READY.get_or_init(|| {
            if std::path::Path::new(PATH).exists() {
                return true;
            }
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-f", "lavfi", "-i", "color=c=blue:size=320x240:rate=30",
                    "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
                    "-t", "2",
                    "-c:v", "libx264", "-c:a", "aac", "-pix_fmt", "yuv420p",
                    PATH,
                ])
                .status();
            matches!(status, Ok(s) if s.success())
        });
        if *ok { Some(PATH) } else { None }
    }

    #[test]
    fn test_open_valid_file() {
        let Some(path) = test_video_path() else { return; };
        let dec = VideoDecoder::open(path, 320, 240);
        assert!(dec.is_ok(), "open failed: {:?}", dec.err());
        let dec = dec.unwrap();
        // dimensions() returns SOURCE video size, not target — just check it's non-zero.
        let (w, h) = dec.dimensions();
        assert!(w > 0 && h > 0, "source dimensions should be positive, got {}×{}", w, h);
        assert!(dec.fps > 0.0, "fps must be positive, got {}", dec.fps);
    }

    #[test]
    fn test_decode_first_frame() {
        let Some(path) = test_video_path() else { return; };
        let mut dec = VideoDecoder::open(path, 320, 240).unwrap();
        let mut buf = vec![0u8; 320 * 240 * 4];
        let ok = dec.next_frame_bgra(&mut buf).unwrap();
        assert!(ok, "expected a frame");
        // sws_scale fills alpha channel with 0xFF for BGRA output.
        assert_eq!(buf[3], 255, "alpha channel should be 255");
    }

    #[test]
    fn test_eof_returns_false_and_stays_at_eof() {
        let Some(path) = test_video_path() else { return; };
        let mut dec = VideoDecoder::open(path, 320, 240).unwrap();
        let mut buf = vec![0u8; 320 * 240 * 4];
        let mut count = 0usize;
        while dec.next_frame_bgra(&mut buf).unwrap() {
            count += 1;
        }
        assert!(count > 0, "no frames decoded at all");
        // eof_sent is now true — a second call must also return false (no panic).
        assert!(!dec.next_frame_bgra(&mut buf).unwrap(),
            "must remain at EOF on repeated call");
    }

    #[test]
    fn test_seek_to_start_resets_eof() {
        let Some(path) = test_video_path() else { return; };
        let mut dec = VideoDecoder::open(path, 320, 240).unwrap();
        let mut buf = vec![0u8; 320 * 240 * 4];

        // Basic seek after one frame.
        assert!(dec.next_frame_bgra(&mut buf).unwrap());
        dec.seek_to_start().unwrap();
        assert!(dec.next_frame_bgra(&mut buf).unwrap(), "must get frame after seek");

        // Drain to EOF, then seek — verifies eof_sent is cleared by seek_to_start.
        while dec.next_frame_bgra(&mut buf).unwrap() {}
        dec.seek_to_start().unwrap();
        assert!(
            dec.next_frame_bgra(&mut buf).unwrap(),
            "must get frame after EOF+seek (eof_sent must be reset to false)"
        );
    }

    #[test]
    fn test_frame_duration_reasonable() {
        let Some(path) = test_video_path() else { return; };
        let dec = VideoDecoder::open(path, 320, 240).unwrap();
        let dur = dec.frame_duration();
        assert!(
            dur.as_millis() > 10 && dur.as_millis() < 200,
            "unexpected frame duration: {:?} (expected 10–200 ms for typical wallpaper fps)",
            dur,
        );
    }

    #[test]
    fn test_scaler_writes_bgra_at_target_size() {
        // Verifies that the scaler actually runs and fills a 640×480 BGRA buffer.
        let Some(path) = test_video_path() else { return; };
        let mut dec = VideoDecoder::open(path, 640, 480).unwrap();
        let mut buf = vec![0u8; 640 * 480 * 4];
        let ok = dec.next_frame_bgra(&mut buf).unwrap();
        assert!(ok, "expected at least one frame at 640×480 target");
        assert_eq!(buf[3], 255, "BGRA alpha must be 255 after sws_scale");
    }
}
