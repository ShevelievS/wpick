use std::time::Duration;

use anyhow::Context;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::flag::Flags;

use wpick_core::config::FitMode;

static FFMPEG_INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();

pub struct VideoDecoder {
    input_ctx:        ffmpeg::format::context::Input,
    video_stream_idx: usize,
    decoder:          ffmpeg::codec::decoder::Video,
    scaler:           ffmpeg::software::scaling::context::Context,
    fps:              f64,
    eof_sent:         bool,
    /// Frames decoded since the last seek_to_start().  Used to detect
    /// immediate-EOF-after-seek: if the first packet after a seek is EOF,
    /// the demuxer's AVIOContext::eof_reached flag was not cleared by the seek
    /// (happens on some containers with non-zero start_time).  We propagate
    /// this as Err so the caller recreates the decoder instead of spinning.
    frames_since_seek: u64,
    target_w:         u32,
    target_h:         u32,
    offset_x:         u32,
    offset_y:         u32,
}

impl VideoDecoder {
    /// Open a software video decoder.
    ///
    /// `target_w × target_h` is the screen canvas size.
    /// The scaler is configured once to produce BGRA at the exact blit dimensions,
    /// so the render loop can memcpy directly into the SHM slot.
    pub fn open(path: &str, target_w: u32, target_h: u32, fit: FitMode) -> anyhow::Result<Self> {
        FFMPEG_INIT.get_or_init(|| { ffmpeg::init().expect("ffmpeg init failed"); });

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

        let src_w  = decoder.width()  as f64;
        let src_h  = decoder.height() as f64;
        let tw     = target_w as f64;
        let th     = target_h as f64;

        // Compute blit/scaler geometry from FitMode.
        let (blit_w, blit_h, offset_x, offset_y, scaler_src_w, scaler_src_h) =
            match fit {
                FitMode::Fit => {
                    // Scale to fit inside target, preserving aspect ratio (letterbox/pillarbox).
                    let scale    = (tw / src_w).min(th / src_h);
                    let blit_w   = ((src_w * scale).round() as u32).max(1);
                    let blit_h   = ((src_h * scale).round() as u32).max(1);
                    let offset_x = (target_w.saturating_sub(blit_w)) / 2;
                    let offset_y = (target_h.saturating_sub(blit_h)) / 2;
                    (blit_w, blit_h, offset_x, offset_y,
                     decoder.width(), decoder.height())
                }
                FitMode::Fill => {
                    // Scale to fill target, center-crop source to preserve aspect ratio.
                    let scale      = (tw / src_w).max(th / src_h);
                    let crop_src_w = (tw / scale).round() as u32;
                    let crop_src_h = (th / scale).round() as u32;
                    (target_w, target_h, 0, 0,
                     crop_src_w.max(1), crop_src_h.max(1))
                }
                FitMode::Stretch => {
                    // Stretch to fill — no aspect ratio preservation.
                    (target_w, target_h, 0, 0,
                     decoder.width(), decoder.height())
                }
                FitMode::Center => {
                    // No scaling — 1:1 pixels, centered, black borders if smaller.
                    let blit_w   = decoder.width().min(target_w);
                    let blit_h   = decoder.height().min(target_h);
                    let offset_x = (target_w.saturating_sub(blit_w)) / 2;
                    let offset_y = (target_h.saturating_sub(blit_h)) / 2;
                    (blit_w, blit_h, offset_x, offset_y,
                     blit_w.max(1), blit_h.max(1))
                }
            };

        let scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            scaler_src_w,
            scaler_src_h,
            Pixel::BGRA,
            blit_w,
            blit_h,
            Flags::BILINEAR,
        )
        .context("Failed to create scaler context")?;

        Ok(Self {
            input_ctx,
            video_stream_idx,
            decoder,
            scaler,
            fps,
            eof_sent:          false,
            frames_since_seek: 0,
            target_w,
            target_h,
            offset_x,
            offset_y,
        })
    }

    /// Decode the next video frame and write it as BGRA into `dst`.
    ///
    /// `dst` must be at least `target_w * target_h * 4` bytes.
    /// Returns `Ok(true)` when a frame was written, `Ok(false)` at EOF.
    pub fn next_frame_bgra(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        loop {
            let mut decoded = ffmpeg::frame::Video::empty();
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                self.frames_since_seek += 1;
                return self.scale_and_blit(&decoded, dst);
            }

            if self.eof_sent {
                return Ok(false);
            }

            match self.input_ctx.packets().next() {
                None => {
                    // Immediate EOF on the first packet after a seek means the demuxer's
                    // AVIOContext::eof_reached flag was not cleared by seek().  This causes
                    // an infinite seek→EOF→seek loop that freezes the display.  Propagate
                    // as Err so the render loop recreates the decoder from scratch.
                    if self.frames_since_seek == 0 {
                        anyhow::bail!("demuxer EOF immediately after seek — decoder state corrupt");
                    }
                    self.decoder.send_eof().ok();
                    self.eof_sent = true;
                }
                Some((stream, packet)) => {
                    if stream.index() != self.video_stream_idx { continue; }
                    self.decoder.send_packet(&packet)
                        .context("send_packet failed")?;
                }
            }
        }
    }

    fn scale_and_blit(&mut self, decoded: &ffmpeg::frame::Video, dst: &mut [u8]) -> anyhow::Result<bool> {
        let mut bgra_frame = ffmpeg::frame::Video::empty();
        self.scaler.run(decoded, &mut bgra_frame)
            .context("scaler run failed")?;

        let width     = bgra_frame.width()  as usize;
        let height    = bgra_frame.height() as usize;
        let stride    = bgra_frame.stride(0);
        let src       = bgra_frame.data(0);
        let row_bytes = width * 4;
        let needed    = row_bytes * height;

        if src.len() < needed {
            tracing::warn!("Corrupt frame: src.len()={} < needed={}", src.len(), needed);
            return Ok(true); // skip but don't error
        }

        let full = self.target_w as usize * self.target_h as usize * 4;
        if dst.len() < full {
            tracing::warn!("dst too small: {} < {}", dst.len(), full);
            return Ok(true);
        }

        // Clear to black only when there is letterbox/pillarbox padding (Fit/Center modes).
        // For Fill/Stretch the decoder writes every pixel, so the fill is wasted bandwidth.
        let has_padding = self.offset_x > 0 || self.offset_y > 0;
        if has_padding {
            dst[..full].fill(0);
        }
        let dst_stride = self.target_w as usize * 4;
        for row in 0..height {
            let src_start = row * stride;
            let src_end   = src_start + row_bytes;
            if src_end > src.len() { break; }
            let dst_row = self.offset_y as usize + row;
            if dst_row >= self.target_h as usize { break; }
            let dst_col   = self.offset_x as usize;
            let dst_start = dst_row * dst_stride + dst_col * 4;
            if dst_start + row_bytes > dst.len() { break; }
            dst[dst_start..dst_start + row_bytes]
                .copy_from_slice(&src[src_start..src_end]);
        }
        Ok(true)
    }

    pub fn seek_to_start(&mut self) -> anyhow::Result<()> {
        // Seek to the stream's actual start_time rather than literal 0.
        // Files with a non-zero start_time (MPEG-TS, some mkv/webm) may leave
        // AVIOContext::eof_reached set when seek(0) lands before the first packet,
        // causing the demuxer to return EOF immediately on the next av_read_frame.
        let start_ts = self.input_ctx.streams()
            .nth(self.video_stream_idx)
            .map(|s| s.start_time())
            .filter(|&t| t != ffmpeg::ffi::AV_NOPTS_VALUE as i64)
            .unwrap_or(0)
            .max(0);
        self.input_ctx.seek(start_ts, ..).context("seek failed")?;
        self.decoder.flush();
        self.eof_sent         = false;
        self.frames_since_seek = 0;
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

    fn test_video_path() -> Option<&'static str> {
        static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        const PATH: &str = "/tmp/wpick_test.mp4";
        let ok = READY.get_or_init(|| {
            if std::path::Path::new(PATH).exists() { return true; }
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
        let dec = VideoDecoder::open(path, 320, 240, FitMode::Fit);
        assert!(dec.is_ok(), "open failed: {:?}", dec.err());
        let dec = dec.unwrap();
        let (w, h) = dec.dimensions();
        assert!(w > 0 && h > 0);
        assert!(dec.fps > 0.0);
    }

    #[test]
    fn test_decode_first_frame() {
        let Some(path) = test_video_path() else { return; };
        let mut dec = VideoDecoder::open(path, 320, 240, FitMode::Fit).unwrap();
        let mut buf = vec![0u8; 320 * 240 * 4];
        let ok = dec.next_frame_bgra(&mut buf).unwrap();
        assert!(ok, "expected a frame");
        assert_eq!(buf[3], 255, "alpha channel should be 255");
    }

    #[test]
    fn test_eof_returns_false_and_stays_at_eof() {
        let Some(path) = test_video_path() else { return; };
        let mut dec = VideoDecoder::open(path, 320, 240, FitMode::Fit).unwrap();
        let mut buf = vec![0u8; 320 * 240 * 4];
        let mut count = 0usize;
        while dec.next_frame_bgra(&mut buf).unwrap() { count += 1; }
        assert!(count > 0);
        assert!(!dec.next_frame_bgra(&mut buf).unwrap());
    }

    #[test]
    fn test_seek_to_start_resets_eof() {
        let Some(path) = test_video_path() else { return; };
        let mut dec = VideoDecoder::open(path, 320, 240, FitMode::Fit).unwrap();
        let mut buf = vec![0u8; 320 * 240 * 4];

        assert!(dec.next_frame_bgra(&mut buf).unwrap());
        dec.seek_to_start().unwrap();
        assert!(dec.next_frame_bgra(&mut buf).unwrap());

        while dec.next_frame_bgra(&mut buf).unwrap() {}
        dec.seek_to_start().unwrap();
        assert!(dec.next_frame_bgra(&mut buf).unwrap());
    }

    #[test]
    fn test_frame_duration_reasonable() {
        let Some(path) = test_video_path() else { return; };
        let dec = VideoDecoder::open(path, 320, 240, FitMode::Fit).unwrap();
        let dur = dec.frame_duration();
        assert!(dur.as_millis() > 10 && dur.as_millis() < 200);
    }

    #[test]
    fn test_fit_modes_open() {
        let Some(path) = test_video_path() else { return; };
        for fit in [FitMode::Fit, FitMode::Fill, FitMode::Stretch, FitMode::Center] {
            let dec = VideoDecoder::open(path, 640, 480, fit);
            assert!(dec.is_ok(), "open failed for {:?}: {:?}", fit, dec.err());
        }
    }
}
