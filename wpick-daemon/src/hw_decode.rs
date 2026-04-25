// VA-API hardware video decoder.
//
// Decodes video frames on the GPU via VA-API, then transfers them to CPU-side
// NV12 (semi-planar YCbCr 4:2:0). The renderer uploads Y and UV planes
// separately and converts to RGB in the fragment shader — no swscale needed.
//
// Usage:
//   if let Some(dec) = HwDecoder::try_open(path) { ... }  // None → use VideoDecoder
//
// Safety invariants (upheld internally):
//   - All raw pointers are non-null after construction succeeds.
//   - Ownership is exclusive; the struct is never shared across threads.
//   - Drop frees every resource in the correct order (packet → frames → codec → format → hw_device).

use std::ptr;
use std::time::Duration;

use ffmpeg_sys_next as ffsys;

// AVERROR(EAGAIN) = -EAGAIN. EAGAIN = 11 on Linux (POSIX, stable).
const AVERROR_EAGAIN: i32 = -11;

// ─── Public types ─────────────────────────────────────────────────────────────

/// Compact NV12 frame with stride padding stripped.
///
/// Layout:
/// - `y`  : `width × height` bytes (luma, 1 byte per pixel)
/// - `uv` : `width × (height/2)` bytes (chroma, interleaved U+V, 2 bytes per pair)
///
/// UV texture dimensions: `(width/2, height/2)` @ `Rg8Unorm`.
pub struct Nv12Frame {
    pub y:      Vec<u8>,
    pub uv:     Vec<u8>,
    pub width:  u32,
    pub height: u32,
}

impl Nv12Frame {
    /// UV texture dimensions: (width/2, height/2).
    pub fn uv_dims(&self) -> (u32, u32) {
        (self.width / 2, self.height / 2)
    }
}

// ─── HwDecoder ────────────────────────────────────────────────────────────────

pub struct HwDecoder {
    format_ctx:    *mut ffsys::AVFormatContext,
    codec_ctx:     *mut ffsys::AVCodecContext,
    stream_idx:    i32,
    fps:           f64,
    width:         u32,
    height:        u32,
    hw_device_ctx: *mut ffsys::AVBufferRef,
    packet:        *mut ffsys::AVPacket,
    vaapi_frame:   *mut ffsys::AVFrame,
    nv12_frame:    *mut ffsys::AVFrame,
    eof_sent:      bool,
    // B-4: reused across frames — avoids 3 MB/frame of allocation at 1080p30
    y_buf:         Vec<u8>,
    uv_buf:        Vec<u8>,
}

// SAFETY: HwDecoder exclusively owns all raw pointers and is never aliased.
unsafe impl Send for HwDecoder {}

impl Drop for HwDecoder {
    fn drop(&mut self) {
        unsafe {
            ffsys::av_packet_free(&mut self.packet);
            ffsys::av_frame_free(&mut self.vaapi_frame);
            ffsys::av_frame_free(&mut self.nv12_frame);
            ffsys::avcodec_free_context(&mut self.codec_ctx);
            ffsys::avformat_close_input(&mut self.format_ctx);
            ffsys::av_buffer_unref(&mut self.hw_device_ctx);
        }
    }
}

impl HwDecoder {
    /// Try to open a VA-API hardware decoder, trying each DRM render node in turn.
    /// Returns `None` if hw decode is unavailable for this file — caller should
    /// fall back to `VideoDecoder`.
    pub fn try_open(path: &str) -> Option<Self> {
        let path_c = std::ffi::CString::new(path).ok()?;
        for device in ["/dev/dri/renderD128", "/dev/dri/renderD129"] {
            if !std::path::Path::new(device).exists() {
                continue;
            }
            let device_c = match std::ffi::CString::new(device) {
                Ok(c) => c,
                Err(_) => continue,
            };
            match unsafe { Self::open_inner(&path_c, &device_c) } {
                Ok(dec) => {
                    tracing::info!("HW decode active: VA-API on {}", device);
                    return Some(dec);
                }
                Err(e) => {
                    tracing::debug!("HW decode unavailable on {}: {}", device, e);
                }
            }
        }
        None
    }

    unsafe fn open_inner(
        path_c:   &std::ffi::CStr,
        device_c: &std::ffi::CStr,
    ) -> anyhow::Result<Self> {
        ffmpeg_next::init().ok(); // idempotent

        // ── 1. Open format context ────────────────────────────────────────────
        let mut format_ctx: *mut ffsys::AVFormatContext = ptr::null_mut();
        let ret = ffsys::avformat_open_input(
            &mut format_ctx,
            path_c.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        if ret < 0 {
            anyhow::bail!("avformat_open_input: {}", ret);
        }

        let ret = ffsys::avformat_find_stream_info(format_ctx, ptr::null_mut());
        if ret < 0 {
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("avformat_find_stream_info: {}", ret);
        }

        // ── 2. Find best video stream ─────────────────────────────────────────
        let mut codec: *const ffsys::AVCodec = ptr::null();
        let stream_idx = ffsys::av_find_best_stream(
            format_ctx,
            ffsys::AVMediaType::AVMEDIA_TYPE_VIDEO,
            -1, -1,
            &mut codec,
            0,
        );
        if stream_idx < 0 || codec.is_null() {
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("no video stream");
        }

        // ── 3. Verify codec has VA-API support ────────────────────────────────
        if !has_vaapi_config(codec) {
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("codec has no VA-API config");
        }

        // ── 4. Create VA-API device context ──────────────────────────────────
        let mut hw_device_ctx: *mut ffsys::AVBufferRef = ptr::null_mut();
        let ret = ffsys::av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            ffsys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            device_c.as_ptr(),
            ptr::null_mut(),
            0,
        );
        if ret < 0 {
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("av_hwdevice_ctx_create(VAAPI): {}", ret);
        }

        // ── 5. Set up codec context ───────────────────────────────────────────
        let codec_ctx = ffsys::avcodec_alloc_context3(codec);
        if codec_ctx.is_null() {
            ffsys::av_buffer_unref(&mut hw_device_ctx);
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("avcodec_alloc_context3 OOM");
        }

        let streams = std::slice::from_raw_parts(
            (*format_ctx).streams,
            (*format_ctx).nb_streams as usize,
        );
        let stream   = streams[stream_idx as usize];
        let codecpar = (*stream).codecpar;

        let ret = ffsys::avcodec_parameters_to_context(codec_ctx, codecpar);
        if ret < 0 {
            ffsys::avcodec_free_context(&mut { codec_ctx });
            ffsys::av_buffer_unref(&mut hw_device_ctx);
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("avcodec_parameters_to_context: {}", ret);
        }

        // Wire up hw device + pixel-format selection callback
        (*codec_ctx).hw_device_ctx = ffsys::av_buffer_ref(hw_device_ctx);
        (*codec_ctx).get_format     = Some(get_format_vaapi);

        let ret = ffsys::avcodec_open2(codec_ctx, codec, ptr::null_mut());
        if ret < 0 {
            ffsys::avcodec_free_context(&mut { codec_ctx });
            ffsys::av_buffer_unref(&mut hw_device_ctx);
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("avcodec_open2: {}", ret);
        }

        // ── 6. FPS + dimensions ───────────────────────────────────────────────
        let fps = {
            let r = (*stream).avg_frame_rate;
            let v = r.num as f64 / r.den.max(1) as f64;
            if v < 1.0 {
                tracing::warn!(
                    "HW: fps={:.3} (num={} den={}) — clamping to 1.0",
                    v, r.num, r.den,
                );
            }
            v.max(1.0)
        };
        let width  = (*codecpar).width  as u32;
        let height = (*codecpar).height as u32;

        // ── 7. Allocate reusable buffers ──────────────────────────────────────
        let packet      = ffsys::av_packet_alloc();
        let vaapi_frame = ffsys::av_frame_alloc();
        let nv12_frame  = ffsys::av_frame_alloc();
        if packet.is_null() || vaapi_frame.is_null() || nv12_frame.is_null() {
            ffsys::av_packet_free(&mut { packet });
            ffsys::av_frame_free(&mut { vaapi_frame });
            ffsys::av_frame_free(&mut { nv12_frame });
            ffsys::avcodec_free_context(&mut { codec_ctx });
            ffsys::av_buffer_unref(&mut hw_device_ctx);
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("allocation failed");
        }

        let y_buf  = Vec::with_capacity((width  * height)      as usize);
        let uv_buf = Vec::with_capacity((width  * height / 2) as usize);

        Ok(Self {
            format_ctx,
            codec_ctx,
            stream_idx,
            fps,
            width,
            height,
            hw_device_ctx,
            packet,
            vaapi_frame,
            nv12_frame,
            eof_sent: false,
            y_buf,
            uv_buf,
        })
    }

    // ── Public interface ──────────────────────────────────────────────────────

    /// Decode the next frame and return compact NV12.
    /// `Ok(None)` means EOF — call `seek_to_start()` for seamless loop.
    /// B-4: internal staging buffers (`y_buf`/`uv_buf`) reuse their heap
    /// allocation across frames so no zero-init or realloc occurs after
    /// the first frame when resolution is constant.
    pub fn next_nv12_frame(&mut self) -> anyhow::Result<Option<Nv12Frame>> {
        unsafe { self.next_nv12_inner() }
    }

    unsafe fn next_nv12_inner(&mut self) -> anyhow::Result<Option<Nv12Frame>> {
        loop {
            // ── Try to drain decoder first (avoids unnecessary av_read_frame) ──
            ffsys::av_frame_unref(self.vaapi_frame);
            let ret = ffsys::avcodec_receive_frame(self.codec_ctx, self.vaapi_frame);

            if ret == 0 {
                // Got a VAAPI frame — transfer to CPU NV12
                match self.transfer_to_nv12() {
                    Ok(Some(frame)) => return Ok(Some(frame)),
                    Ok(None)        => continue, // corrupt/skipped frame
                    Err(e)          => {
                        tracing::warn!("HW: transfer_to_nv12: {}", e);
                        continue;
                    }
                }
            }

            if ret == ffsys::AVERROR_EOF { return Ok(None); }

            if ret != AVERROR_EAGAIN {
                tracing::warn!("HW: avcodec_receive_frame: {}", ret);
                // Not a fatal error — keep trying
            }

            // Need more input — read a packet
            if self.eof_sent {
                // Decoder flushed but returned EOF already handled above
                return Ok(None);
            }

            ffsys::av_packet_unref(self.packet);
            let ret = ffsys::av_read_frame(self.format_ctx, self.packet);

            if ret == ffsys::AVERROR_EOF {
                // Send flush packet to drain remaining decoder frames
                ffsys::avcodec_send_packet(self.codec_ctx, ptr::null());
                self.eof_sent = true;
                continue;
            }
            if ret < 0 {
                anyhow::bail!("av_read_frame: {}", ret);
            }
            if (*self.packet).stream_index != self.stream_idx {
                ffsys::av_packet_unref(self.packet);
                continue;
            }

            let ret = ffsys::avcodec_send_packet(self.codec_ctx, self.packet);
            ffsys::av_packet_unref(self.packet);
            if ret < 0 && ret != AVERROR_EAGAIN {
                tracing::warn!("HW: avcodec_send_packet: {}", ret);
            }
        }
    }

    unsafe fn transfer_to_nv12(&mut self) -> anyhow::Result<Option<Nv12Frame>> {
        ffsys::av_frame_unref(self.nv12_frame);
        // Request NV12 output — VA-API will pick a compatible format automatically
        (*self.nv12_frame).format = ffsys::AVPixelFormat::AV_PIX_FMT_NV12 as i32;

        let ret = ffsys::av_hwframe_transfer_data(self.nv12_frame, self.vaapi_frame, 0);
        if ret < 0 {
            tracing::warn!("HW: av_hwframe_transfer_data: {}", ret);
            return Ok(None);
        }

        let frame     = self.nv12_frame;
        let width     = self.width;
        let height    = self.height;
        let stride_y  = (*frame).linesize[0] as usize;
        let stride_uv = (*frame).linesize[1] as usize;

        // Validate pointers and strides before slicing
        if (*frame).data[0].is_null() || (*frame).data[1].is_null() {
            tracing::warn!("HW: NV12 plane pointer is null — skipping frame");
            return Ok(None);
        }
        let y_row  = width as usize;
        let uv_row = width as usize; // width/2 pairs × 2 bytes = width bytes
        if stride_y < y_row {
            tracing::warn!("HW: Y stride {} < width {} — skipping frame", stride_y, y_row);
            return Ok(None);
        }
        if stride_uv < uv_row {
            tracing::warn!("HW: UV stride {} < row_bytes {} — skipping frame", stride_uv, uv_row);
            return Ok(None);
        }

        let raw_y   = std::slice::from_raw_parts((*frame).data[0], stride_y  * height as usize);
        let uv_rows = height as usize / 2;
        let raw_uv  = std::slice::from_raw_parts((*frame).data[1], stride_uv * uv_rows);

        let y_len  = y_row  * height as usize;
        let uv_len = uv_row * uv_rows;

        // B-4: fill staging buffers without zero-init.
        // clear() preserves heap allocation; extend_from_slice copies row by row
        // stripping the ffmpeg stride padding (E-40). On frames after the first
        // these clear+extend calls don't reallocate when resolution is unchanged.
        self.y_buf.clear();
        for row in 0..height as usize {
            self.y_buf.extend_from_slice(
                &raw_y[row * stride_y .. row * stride_y + y_row],
            );
        }

        self.uv_buf.clear();
        for row in 0..uv_rows {
            self.uv_buf.extend_from_slice(
                &raw_uv[row * stride_uv .. row * stride_uv + uv_row],
            );
        }

        // Swap staging buffers out — caller gets ownership, decoder gets
        // pre-sized empty replacements for the next frame (no realloc next call).
        let y  = std::mem::replace(&mut self.y_buf,  Vec::with_capacity(y_len));
        let uv = std::mem::replace(&mut self.uv_buf, Vec::with_capacity(uv_len));

        Ok(Some(Nv12Frame { y, uv, width, height }))
    }

    pub fn seek_to_start(&mut self) -> anyhow::Result<()> {
        unsafe {
            let ret = ffsys::av_seek_frame(
                self.format_ctx, -1, 0, ffsys::AVSEEK_FLAG_BACKWARD,
            );
            anyhow::ensure!(ret >= 0, "av_seek_frame: {}", ret);
            ffsys::avcodec_flush_buffers(self.codec_ctx);
            self.eof_sent = false;
        }
        Ok(())
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps)
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Returns true if the codec has at least one VA-API hw config that supports
/// the `HW_DEVICE_CTX` method (what we use).
unsafe fn has_vaapi_config(codec: *const ffsys::AVCodec) -> bool {
    let mut i = 0i32;
    loop {
        let cfg = ffsys::avcodec_get_hw_config(codec, i);
        if cfg.is_null() { return false; }
        if (*cfg).device_type == ffsys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI
            && ((*cfg).methods & ffsys::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0
        {
            return true;
        }
        i += 1;
    }
}

/// Pixel-format selection callback: prefer `AV_PIX_FMT_VAAPI` over SW formats.
unsafe extern "C" fn get_format_vaapi(
    _ctx: *mut ffsys::AVCodecContext,
    fmt:  *const ffsys::AVPixelFormat,
) -> ffsys::AVPixelFormat {
    let mut p = fmt;
    while *p != ffsys::AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == ffsys::AVPixelFormat::AV_PIX_FMT_VAAPI {
            return ffsys::AVPixelFormat::AV_PIX_FMT_VAAPI;
        }
        p = p.add(1);
    }
    // Codec has no VAAPI format in the list — decoder will fall back to SW
    ffsys::AVPixelFormat::AV_PIX_FMT_NONE
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_VIDEO: &str = "/tmp/wpick_test.mp4";

    /// Checks whether VA-API is available on this machine.
    /// We detect by attempting to create a hw device context.
    fn vaapi_available() -> bool {
        let device = if std::path::Path::new("/dev/dri/renderD128").exists() {
            "/dev/dri/renderD128"
        } else if std::path::Path::new("/dev/dri/renderD129").exists() {
            "/dev/dri/renderD129"
        } else {
            return false;
        };
        let device_c = std::ffi::CString::new(device).unwrap();
        let mut ctx: *mut ffsys::AVBufferRef = ptr::null_mut();
        let ret = unsafe {
            ffmpeg_next::init().ok();
            ffsys::av_hwdevice_ctx_create(
                &mut ctx,
                ffsys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                device_c.as_ptr(),
                ptr::null_mut(),
                0,
            )
        };
        if ret >= 0 {
            unsafe { ffsys::av_buffer_unref(&mut ctx); }
            true
        } else {
            false
        }
    }

    /// HwDecoder opens successfully when VA-API is present, skips otherwise.
    #[test]
    fn test_hw_decoder_open_or_skip() {
        if !vaapi_available() {
            eprintln!("VA-API not available — skipping hw_decode tests");
            return;
        }
        let dec = HwDecoder::try_open(TEST_VIDEO);
        assert!(dec.is_some(), "expected HW decoder to open on VA-API machine");
    }

    /// try_open returns None (does not panic) on a non-existent file.
    #[test]
    fn test_hw_decoder_nonexistent_file() {
        let dec = HwDecoder::try_open("/tmp/wpick_nonexistent_99999.mp4");
        assert!(dec.is_none(), "expected None for nonexistent file");
    }

    /// Decode at least one frame and verify NV12 plane sizes.
    #[test]
    fn test_hw_decoder_nv12_frame_dimensions() {
        if !vaapi_available() { return; }
        let mut dec = match HwDecoder::try_open(TEST_VIDEO) {
            Some(d) => d,
            None    => return,
        };
        let frame = dec.next_nv12_frame().unwrap();
        assert!(frame.is_some(), "expected at least one frame");
        let f = frame.unwrap();
        assert_eq!(f.width,  320, "Y width");
        assert_eq!(f.height, 240, "Y height");
        assert_eq!(f.y.len(),  (320 * 240) as usize, "Y plane size");
        assert_eq!(f.uv.len(), (320 * 120) as usize, "UV plane size (width * height/2)");
        let (uw, uh) = f.uv_dims();
        assert_eq!(uw, 160, "UV texture width");
        assert_eq!(uh, 120, "UV texture height");
    }

    /// Decode all frames and verify seek-to-start gives another frame.
    #[test]
    fn test_hw_decoder_seek_and_loop() {
        if !vaapi_available() { return; }
        let mut dec = match HwDecoder::try_open(TEST_VIDEO) {
            Some(d) => d,
            None    => return,
        };
        // Drain to EOF
        while dec.next_nv12_frame().unwrap().is_some() {}
        // Seek back
        dec.seek_to_start().unwrap();
        let after_seek = dec.next_nv12_frame().unwrap();
        assert!(after_seek.is_some(), "expected frame after seek to start");
    }

    /// frame_duration is within a sane range for a 30fps clip.
    #[test]
    fn test_hw_decoder_frame_duration() {
        if !vaapi_available() { return; }
        let dec = match HwDecoder::try_open(TEST_VIDEO) {
            Some(d) => d,
            None    => return,
        };
        let dur = dec.frame_duration();
        assert!(
            dur.as_millis() > 10 && dur.as_millis() < 200,
            "unexpected frame duration: {:?}", dur,
        );
    }
}
