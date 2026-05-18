// VA-API hardware video decoder with integrated swscale.
//
// Decode path:
//   avformat → avcodec (VA-API) → vaapi_frame (GPU)
//   → av_hwframe_transfer_data → nv12_frame (CPU, NV12)
//   → sws_scale                → dst (BGRA at target_w × target_h)
//
// The sws_scale call writes directly into the caller-supplied SHM slot,
// eliminating staging buffers (y_buf / uv_buf) used in previous versions.
//
// Safety invariants (upheld internally):
//   - All raw pointers are non-null after construction succeeds.
//   - Ownership is exclusive; the struct is never shared across threads.
//   - Drop frees every resource in reverse-init order.

use std::ptr;
use std::time::Duration;

use ffmpeg_sys_next as ffsys;

// AVERROR(EAGAIN) = -EAGAIN. EAGAIN = 11 on Linux.
const AVERROR_EAGAIN: i32 = -11;

// ─── HwDecoder ────────────────────────────────────────────────────────────────

pub struct HwDecoder {
    format_ctx:    *mut ffsys::AVFormatContext,
    codec_ctx:     *mut ffsys::AVCodecContext,
    stream_idx:    i32,
    fps:           f64,
    width:         u32,
    height:        u32,
    target_w:      u32,
    target_h:      u32,
    hw_device_ctx: *mut ffsys::AVBufferRef,
    packet:        *mut ffsys::AVPacket,
    vaapi_frame:   *mut ffsys::AVFrame,
    nv12_frame:    *mut ffsys::AVFrame,
    sws_ctx:       *mut ffsys::SwsContext,
    eof_sent:      bool,
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
            if !self.sws_ctx.is_null() {
                ffsys::sws_freeContext(self.sws_ctx);
            }
        }
    }
}

impl HwDecoder {
    /// Try to open a VA-API hardware decoder for `path`, scaling output to
    /// `target_w × target_h` BGRA.  Returns `None` if hw decode is unavailable
    /// — caller should fall back to `VideoDecoder`.
    pub fn try_open(path: &str, target_w: u32, target_h: u32) -> Option<Self> {
        let path_c = std::ffi::CString::new(path).ok()?;
        let render_nodes: Vec<std::path::PathBuf> = {
            let Ok(entries) = std::fs::read_dir("/dev/dri") else { return None };
            let mut v: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("renderD"))
                        .unwrap_or(false)
                })
                .collect();
            v.sort();
            v
        };
        if render_nodes.is_empty() { return None; }

        for device_path in &render_nodes {
            let Ok(device_c) = std::ffi::CString::new(device_path.to_string_lossy().as_bytes()) else { continue };
            match unsafe { Self::open_inner(&path_c, &device_c, target_w, target_h) } {
                Ok(dec) => {
                    tracing::info!("HW decode active: VA-API on {}", device_path.display());
                    return Some(dec);
                }
                Err(e) => {
                    tracing::debug!("HW decode unavailable on {}: {}", device_path.display(), e);
                }
            }
        }
        None
    }

    unsafe fn open_inner(
        path_c:   &std::ffi::CStr,
        device_c: &std::ffi::CStr,
        target_w: u32,
        target_h: u32,
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

        // ── 7. Allocate reusable frame objects ────────────────────────────────
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
            anyhow::bail!("frame/packet allocation failed");
        }

        // ── 8. Create swscale context: NV12 (src dims) → BGRA (target dims) ──
        let sws_ctx = ffsys::sws_getContext(
            width    as libc::c_int,
            height   as libc::c_int,
            ffsys::AVPixelFormat::AV_PIX_FMT_NV12,
            target_w as libc::c_int,
            target_h as libc::c_int,
            ffsys::AVPixelFormat::AV_PIX_FMT_BGRA,
            ffsys::SwsFlags::SWS_BILINEAR as libc::c_int,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        if sws_ctx.is_null() {
            ffsys::av_packet_free(&mut { packet });
            ffsys::av_frame_free(&mut { vaapi_frame });
            ffsys::av_frame_free(&mut { nv12_frame });
            ffsys::avcodec_free_context(&mut { codec_ctx });
            ffsys::av_buffer_unref(&mut hw_device_ctx);
            ffsys::avformat_close_input(&mut format_ctx);
            anyhow::bail!("sws_getContext failed (NV12→BGRA)");
        }

        Ok(Self {
            format_ctx,
            codec_ctx,
            stream_idx,
            fps,
            width,
            height,
            target_w,
            target_h,
            hw_device_ctx,
            packet,
            vaapi_frame,
            nv12_frame,
            sws_ctx,
            eof_sent: false,
        })
    }

    // ── Public interface ──────────────────────────────────────────────────────

    /// Decode the next frame and write it as BGRA into `dst`.
    ///
    /// `dst` must be at least `target_w * target_h * 4` bytes.
    /// Returns `Ok(true)` when a frame was written, `Ok(false)` at EOF.
    pub fn next_frame_bgra(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        let required = self.target_w as usize * self.target_h as usize * 4;
        anyhow::ensure!(
            dst.len() >= required,
            "dst too small: {} < {} bytes ({}×{}×4)",
            dst.len(), required, self.target_w, self.target_h,
        );
        unsafe { self.next_frame_inner(dst) }
    }

    unsafe fn next_frame_inner(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        loop {
            // ── Try to drain decoder first ────────────────────────────────────
            ffsys::av_frame_unref(self.vaapi_frame);
            let ret = ffsys::avcodec_receive_frame(self.codec_ctx, self.vaapi_frame);

            if ret == 0 {
                match self.transfer_and_scale(dst) {
                    Ok(true)  => return Ok(true),
                    Ok(false) => continue, // corrupt/skipped frame — try next
                    Err(e)    => {
                        tracing::warn!("HW: transfer_and_scale: {}", e);
                        continue;
                    }
                }
            }

            if ret == ffsys::AVERROR_EOF { return Ok(false); }

            if ret != AVERROR_EAGAIN {
                // Persistent errors (GPU driver reset, VA context loss, corrupted
                // stream) are neither EOF nor EAGAIN.  Warn-and-continue causes an
                // infinite spin with a frozen display.  Propagate as Err so the render
                // loop sets hw_ok=false and falls back to the SW decoder.
                anyhow::bail!("avcodec_receive_frame: {}", ret);
            }

            // Need more input
            if self.eof_sent {
                return Ok(false);
            }

            ffsys::av_packet_unref(self.packet);
            let ret = ffsys::av_read_frame(self.format_ctx, self.packet);

            if ret == ffsys::AVERROR_EOF {
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

    /// Transfer GPU frame to CPU NV12, then swscale NV12→BGRA into `dst`.
    /// Returns `Ok(false)` for corrupt/skippable frames.
    unsafe fn transfer_and_scale(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        ffsys::av_frame_unref(self.nv12_frame);
        (*self.nv12_frame).format = ffsys::AVPixelFormat::AV_PIX_FMT_NV12 as i32;

        let ret = ffsys::av_hwframe_transfer_data(self.nv12_frame, self.vaapi_frame, 0);
        if ret < 0 {
            tracing::warn!("HW: av_hwframe_transfer_data: {}", ret);
            // Unref before returning — the transfer may have partially allocated a
            // buffer in nv12_frame before failing.  Without this unref the buffer
            // stays alive for one extra loop iteration, starving the VA surface pool
            // under repeated GPU errors (e.g. suspend/resume).
            ffsys::av_frame_unref(self.nv12_frame);
            return Ok(false);
        }

        // Validate pointers
        if (*self.nv12_frame).data[0].is_null() || (*self.nv12_frame).data[1].is_null() {
            tracing::warn!("HW: NV12 plane pointer is null — skipping frame");
            return Ok(false);
        }

        let stride_y  = (*self.nv12_frame).linesize[0];
        let stride_uv = (*self.nv12_frame).linesize[1];
        if stride_y <= 0 || stride_uv <= 0 {
            tracing::warn!("HW: invalid NV12 strides {} {} — skipping", stride_y, stride_uv);
            return Ok(false);
        }

        let dst_stride = (self.target_w * 4) as libc::c_int;
        let dst_ptr    = dst.as_mut_ptr();

        // src_data: Y plane + UV plane (NV12 = 2-plane format)
        let src_data: [*const u8; 8] = [
            (*self.nv12_frame).data[0],
            (*self.nv12_frame).data[1],
            ptr::null(), ptr::null(), ptr::null(), ptr::null(), ptr::null(), ptr::null(),
        ];
        let src_linesize: [libc::c_int; 8] = [
            stride_y, stride_uv, 0, 0, 0, 0, 0, 0,
        ];
        // dst_data: single BGRA plane
        let dst_data: [*mut u8; 8] = [
            dst_ptr, ptr::null_mut(), ptr::null_mut(), ptr::null_mut(),
            ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut(),
        ];
        let dst_linesizes: [libc::c_int; 8] = [
            dst_stride, 0, 0, 0, 0, 0, 0, 0,
        ];

        let rows = ffsys::sws_scale(
            self.sws_ctx,
            src_data.as_ptr(),
            src_linesize.as_ptr(),
            0,
            self.height as libc::c_int,
            dst_data.as_ptr() as *mut *mut u8,
            dst_linesizes.as_ptr(),
        );
        anyhow::ensure!(rows >= 0, "sws_scale failed: {}", rows);

        Ok(true)
    }

    pub fn seek_to_start(&mut self) -> anyhow::Result<()> {
        unsafe {
            // Unref in-flight frames before seek so VA surfaces are returned to the
            // driver pool. Without this, surface refcount grows with each loop cycle
            // until the VA driver exhausts its surface budget and decode fails.
            ffsys::av_frame_unref(self.vaapi_frame);
            ffsys::av_frame_unref(self.nv12_frame);
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

    /// Source video dimensions (for logging).
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

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
    ffsys::AVPixelFormat::AV_PIX_FMT_NONE
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Same fixture used by video.rs tests — generates the file once via ffmpeg.
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

    #[test]
    fn test_hw_decoder_nonexistent_file() {
        // Does not require a test video — must return None for a missing file.
        let dec = HwDecoder::try_open("/tmp/wpick_nonexistent_99999.mp4", 320, 240);
        assert!(dec.is_none(), "expected None for nonexistent file");
    }

    #[test]
    fn test_hw_decoder_open_or_skip() {
        let Some(path) = test_video_path() else { return; };
        if !vaapi_available() {
            eprintln!("VA-API not available — skipping hw_decode tests");
            return;
        }
        let dec = HwDecoder::try_open(path, 320, 240);
        assert!(dec.is_some(), "expected HW decoder to open on a VA-API machine");
    }

    #[test]
    fn test_hw_decoder_frame_bgra() {
        let Some(path) = test_video_path() else { return; };
        if !vaapi_available() { return; }
        let mut dec = match HwDecoder::try_open(path, 320, 240) {
            Some(d) => d,
            None    => return,
        };
        let mut buf = vec![0u8; 320 * 240 * 4];
        let ok = dec.next_frame_bgra(&mut buf).unwrap();
        assert!(ok, "expected at least one frame");
        assert_eq!(buf[3], 255, "BGRA alpha should be 255");
    }

    #[test]
    fn test_hw_decoder_seek_and_loop() {
        let Some(path) = test_video_path() else { return; };
        if !vaapi_available() { return; }
        let mut dec = match HwDecoder::try_open(path, 320, 240) {
            Some(d) => d,
            None    => return,
        };
        let mut buf = vec![0u8; 320 * 240 * 4];
        while dec.next_frame_bgra(&mut buf).unwrap() {}
        dec.seek_to_start().unwrap();
        assert!(dec.next_frame_bgra(&mut buf).unwrap(), "expected frame after seek");
    }

    #[test]
    fn test_hw_decoder_frame_duration() {
        let Some(path) = test_video_path() else { return; };
        if !vaapi_available() { return; }
        let dec = match HwDecoder::try_open(path, 320, 240) {
            Some(d) => d,
            None    => return,
        };
        let dur = dec.frame_duration();
        assert!(
            dur.as_millis() > 10 && dur.as_millis() < 200,
            "unexpected frame duration: {:?}", dur,
        );
    }

    #[test]
    fn test_hw_decoder_bgra_buffer_size() {
        let Some(path) = test_video_path() else { return; };
        if !vaapi_available() { return; }
        let mut dec = match HwDecoder::try_open(path, 640, 360) {
            Some(d) => d,
            None    => return,
        };
        let mut buf = vec![0u8; 640 * 360 * 4];
        let ok = dec.next_frame_bgra(&mut buf).unwrap();
        assert!(ok, "expected at least one frame at 640×360");
    }

    #[test]
    fn test_hw_decoder_small_dst_rejected() {
        // Safety guard: next_frame_bgra with dst < target_w*target_h*4 must Err.
        let Some(path) = test_video_path() else { return; };
        if !vaapi_available() { return; }
        let mut dec = match HwDecoder::try_open(path, 320, 240) {
            Some(d) => d,
            None    => return,
        };
        let mut tiny = vec![0u8; 10]; // 10 < 320*240*4 = 307200
        let result = dec.next_frame_bgra(&mut tiny);
        assert!(result.is_err(), "must return Err for undersized dst buffer");
        assert!(
            result.unwrap_err().to_string().contains("dst too small"),
            "error message must mention 'dst too small'"
        );
    }
}
