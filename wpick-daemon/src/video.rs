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

        Ok(Self {
            input_ctx,
            video_stream_idx,
            decoder,
            scaler,
            fps,
        })
    }

    pub fn next_frame_rgba(&mut self) -> anyhow::Result<Option<(Vec<u8>, u32, u32)>> {
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
                        let mut rgba = ffmpeg::frame::Video::empty();
                        self.scaler.run(&decoded, &mut rgba)
                            .context("scaler run failed")?;

                        let width  = rgba.width();
                        let height = rgba.height();
                        let stride = rgba.stride(0);

                        let data = if stride == (width * 4) as usize {
                            rgba.data(0).to_vec()
                        } else {
                            let mut packed = Vec::with_capacity((width * height * 4) as usize);
                            let src = rgba.data(0);
                            for row in 0..height as usize {
                                let start = row * stride;
                                packed.extend_from_slice(&src[start..start + width as usize * 4]);
                            }
                            packed
                        };

                        return Ok(Some((data, width, height)));
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
        let _ = data[0]; // byte is always valid u8
    }

    #[test]
    fn test_eof_returns_none() {
        let mut dec = VideoDecoder::open(TEST_VIDEO).unwrap();
        let mut count = 0usize;
        while dec.next_frame_rgba().unwrap().is_some() {
            count += 1;
        }
        assert!(count > 0, "no frames decoded");
    }

    #[test]
    fn test_seek_to_start() {
        let mut dec = VideoDecoder::open(TEST_VIDEO).unwrap();
        let frame1_before = dec.next_frame_rgba().unwrap();
        dec.seek_to_start().unwrap();
        let frame1_after = dec.next_frame_rgba().unwrap();
        assert!(frame1_before.is_some());
        assert!(frame1_after.is_some());
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
}
