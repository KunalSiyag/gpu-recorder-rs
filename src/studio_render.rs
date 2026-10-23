//! Turns a raw capture into the final MP4: the cursor-following zoom and the
//! presentation background are applied in one pass.
//!
//! Decoding, compositing, and encoding run as a three-stage pipeline so the
//! CPU work overlaps both FFmpeg processes. When neither zoom nor background
//! would change a pixel, the capture is only remuxed, which takes seconds.

use crate::backgrounds::{corner_mask, render_canvas, ContentRect};
use crate::encoder::{build_smooth_60fps_camera_frames, RecordedZoomEvent};
use crate::ffmpeg::{self, VideoEncoder};
use crate::settings::{BackgroundSettings, Quality, Region};
use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct RenderJob {
    /// Raw capture written by the capture helper (fragmented MP4).
    pub capture: PathBuf,
    /// Final MP4 path.
    pub output: PathBuf,
    pub events: Vec<RecordedZoomEvent>,
    pub auto_zoom: bool,
    /// Region of the monitor that was captured; cursor telemetry is relative
    /// to the whole monitor.
    pub region: Option<Region>,
    pub background: BackgroundSettings,
    pub quality: Quality,
    pub fps: u32,
}

/// Camera at one output frame: center (normalized) and zoom factor.
type Camera = (f32, f32, f32);

fn camera_track(job: &RenderJob, duration_sec: f64) -> Option<Vec<Camera>> {
    if !job.auto_zoom || job.events.is_empty() {
        return None;
    }
    let events: Vec<RecordedZoomEvent> = job
        .events
        .iter()
        .map(|e| {
            if e.off_capture {
                // The cursor is on another display: hold the wide view.
                return RecordedZoomEvent {
                    center_x: 0.5,
                    center_y: 0.5,
                    cursor_x: 0.5,
                    cursor_y: 0.5,
                    zoom: 1.0,
                    clicked: false,
                    manual_lock: false,
                    ..*e
                };
            }
            let Some(region) = job.region else { return *e };
            let (cx, cy) = region.map_point(e.cursor_x, e.cursor_y);
            let (zx, zy) = region.map_point(e.center_x, e.center_y);
            RecordedZoomEvent {
                cursor_x: cx,
                cursor_y: cy,
                center_x: zx,
                center_y: zy,
                ..*e
            }
        })
        .collect();
    let frames = (duration_sec * 60.0).ceil() as usize + 2;
    let track = build_smooth_60fps_camera_frames(&events, frames);
    track.iter().any(|&(_, _, z)| z > 1.01).then_some(track)
}

/// Renders `job` and returns the path of the finished MP4. `progress` is
/// updated in permille.
pub fn render(job: &RenderJob, progress: &AtomicU32) -> Result<PathBuf> {
    progress.store(0, Ordering::Relaxed);
    // Keep the desktop responsive: this thread and the pool it spawns run at
    // a lower priority (Linux applies nice per thread and threads inherit it).
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    let info = ffmpeg::probe_video(&job.capture)?;
    let track = camera_track(job, info.duration_sec);
    if track.is_none() && !job.background.is_enabled() {
        ffmpeg::remux_to_mp4(&job.capture, &job.output)?;
        progress.store(1000, Ordering::Relaxed);
        return Ok(job.output.clone());
    }

    let (w, h) = (info.width, info.height);
    if w % 2 != 0 || h % 2 != 0 || w < 16 || h < 16 {
        bail!("cannot render a {w}x{h} capture (NV12 needs even sizes)");
    }
    let rect = if job.background.is_enabled() {
        ContentRect::framed(w, h, job.background.padding, job.background.radius)
    } else {
        ContentRect::full(w, h)
    };
    let canvas = render_canvas(&job.background, w, h, &rect)?;
    let compositor = Compositor::new(w, h, rect, canvas);

    let tmp = job.output.with_file_name(format!(
        ".{}.rendering.mp4",
        job.output.file_stem().and_then(|s| s.to_str()).unwrap_or("HyperGSR")
    ));
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("hypergsr-render-{i}"))
        .build()
        .context("cannot start render threads")?;
    let encoder = ffmpeg::capabilities().encoder.clone();
    let video_sec = (info.duration_sec - info.video_start_sec).max(0.0);
    let expected = ((video_sec * job.fps as f64).round() as u64).max(1);
    // Container durations can exceed the video by a few frames (audio tail).
    let slack = (job.fps as u64 / 2).max(3);
    let frames = pool.install(|| {
        run_pipeline(job, &encoder, &compositor, track.as_deref(), &tmp, expected, info.video_start_sec, progress)
    });

    match frames {
        // A decoder that read to the end vouches for the frame count (a crashed
        // capture's header overstates its length by up to a keyframe interval).
        // Otherwise anything short means the decoder stopped early.
        Ok((n, decoder_ok)) if n > 0 && (decoder_ok || n + slack >= expected) => {
            std::fs::rename(&tmp, &job.output)?;
            progress.store(1000, Ordering::Relaxed);
            Ok(job.output.clone())
        }
        other => {
            let _ = std::fs::remove_file(&tmp);
            let reason = match other {
                Ok((n, _)) => format!("only {n} of {expected} frames were rendered"),
                Err(err) => format!("{err:#}"),
            };
            bail!("studio render failed: {reason}")
        }
    }
}

fn lower_priority(cmd: &mut Command) -> &mut Command {
    unsafe {
        cmd.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        })
    }
}

fn run_pipeline(
    job: &RenderJob,
    encoder: &VideoEncoder,
    compositor: &Compositor,
    track: Option<&[Camera]>,
    tmp: &Path,
    expected: u64,
    video_start_sec: f64,
    progress: &AtomicU32,
) -> Result<(u64, bool)> {
    let (w, h) = (compositor.w, compositor.h);
    // Decode on the GPU when one is encoding anyway; FFmpeg falls back to
    // software decoding by itself if the hardware decoder is unavailable.
    let hwaccel: Vec<String> = match encoder {
        VideoEncoder::Nvenc => vec!["-hwaccel".into(), "cuda".into()],
        VideoEncoder::Vaapi { device } => vec![
            "-hwaccel".into(),
            "vaapi".into(),
            "-hwaccel_device".into(),
            device.display().to_string(),
        ],
        VideoEncoder::X264 => Vec::new(),
    };
    let mut dec = lower_priority(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-threads", "0"])
            .args(&hwaccel)
            .arg("-i"),
    )
    .arg(&job.capture)
        .args(["-map", "0:v:0"])
        .args(ffmpeg::capabilities().passthrough_timing())
        .args(["-f", "rawvideo", "-pix_fmt", "nv12", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("cannot start the ffmpeg decoder")?;

    let size = format!("{w}x{h}");
    let fps = job.fps.to_string();
    // Raw frames start at zero; shift them back to where the capture's first
    // frame sits relative to its audio.
    let video_offset = format!("{video_start_sec:.6}");
    let mut enc_cmd = Command::new("ffmpeg");
    enc_cmd
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-itsoffset", &video_offset])
        .args(["-f", "rawvideo", "-pix_fmt", "nv12", "-video_size", &size, "-framerate", &fps, "-i", "pipe:0"])
        .arg("-i")
        .arg(&job.capture)
        .args(["-map", "0:v:0", "-map", "1:a?", "-c:a", "copy"])
        .args(ffmpeg::video_encoder_args(encoder, job.quality, &[]))
        .args(ffmpeg::COLOR_TAGS)
        .args(["-movflags", "+faststart", "-f", "mp4"])
        .arg(tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut enc = lower_priority(&mut enc_cmd)
        .spawn()
        .context("cannot start the ffmpeg encoder")?;

    let mut dec_out = dec.stdout.take().context("decoder has no stdout")?;
    let mut enc_in = enc.stdin.take().context("encoder has no stdin")?;
    // NV12 frames held as u16 so the interleaved chroma plane is aligned.
    let frame_words = w * h * 3 / 4;
    const DEPTH: usize = 4;
    let (free_in_tx, free_in_rx) = bounded::<Vec<u16>>(DEPTH);
    let (full_in_tx, full_in_rx) = bounded::<Vec<u16>>(DEPTH);
    let (free_out_tx, free_out_rx) = bounded::<Vec<u16>>(DEPTH);
    let (full_out_tx, full_out_rx) = bounded::<Vec<u16>>(DEPTH);
    for _ in 0..DEPTH {
        free_in_tx.send(vec![0u16; frame_words]).ok();
        free_out_tx.send(vec![0u16; frame_words]).ok();
    }

    let reached_eof = std::sync::atomic::AtomicBool::new(false);
    let written = std::thread::scope(|s| -> Result<(u64, bool)> {
        let reached_eof = &reached_eof;
        let reader = s.spawn(move || read_frames(&mut dec_out, free_in_rx, full_in_tx, reached_eof));
        let writer = s.spawn(move || write_frames(&mut enc_in, full_out_rx, free_out_tx));

        let mut index = 0u64;
        while let Ok(input) = full_in_rx.recv() {
            let Ok(mut output) = free_out_rx.recv() else { break };
            let t = index as f64 / job.fps as f64;
            let cam = track
                .map(|tr| tr[((t * 60.0).round() as usize).min(tr.len() - 1)])
                .unwrap_or((0.5, 0.5, 1.0));
            compositor.compose(&input, &mut output, cam);
            let _ = free_in_tx.send(input);
            if full_out_tx.send(output).is_err() {
                break;
            }
            index += 1;
            if index % 15 == 0 {
                progress.store(((index * 990) / expected).min(990) as u32, Ordering::Relaxed);
            }
        }
        // Unblock both helpers whichever side stopped first: the writer drains
        // what is queued, and a reader still waiting on the decoder sees EOF.
        drop(full_out_tx);
        drop(full_in_rx);
        drop(free_in_tx);
        let written = writer.join().unwrap_or(Ok(0));
        // Unless the decoder reached its end, stop it so the reader can return.
        let decoder_done = reached_eof.load(Ordering::Acquire);
        if !decoder_done {
            let _ = dec.kill();
        }
        let _ = reader.join();
        Ok((written?.min(index), decoder_done))
    });

    let dec_status = dec.wait();
    let status = enc.wait()?;
    let (written, decoder_done) = written?;
    if !status.success() {
        bail!("the ffmpeg encoder exited with {status}");
    }
    if decoder_done && !dec_status.as_ref().is_ok_and(|s| s.success()) {
        bail!("the ffmpeg decoder failed partway ({dec_status:?})");
    }
    Ok((written, decoder_done))
}

fn as_bytes_mut(px: &mut [u16]) -> &mut [u8] {
    // SAFETY: u8 has no alignment requirement and every byte pattern is a valid u16.
    unsafe { std::slice::from_raw_parts_mut(px.as_mut_ptr().cast::<u8>(), px.len() * 2) }
}

fn as_bytes(px: &[u16]) -> &[u8] {
    // SAFETY: as above, read-only.
    unsafe { std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), px.len() * 2) }
}

/// Sets `reached_eof` when the decoder's output ended cleanly on a frame
/// boundary, as opposed to the pipeline shutting the reader down.
fn read_frames(
    src: &mut impl Read,
    free: Receiver<Vec<u16>>,
    full: Sender<Vec<u16>>,
    reached_eof: &std::sync::atomic::AtomicBool,
) {
    for mut buf in free.iter() {
        let bytes = as_bytes_mut(&mut buf);
        // A zero-byte read before any of the frame is a clean end of stream.
        match src.read(&mut bytes[..1]) {
            Ok(0) => {
                reached_eof.store(true, Ordering::Release);
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
        if src.read_exact(&mut bytes[1..]).is_err() {
            return;
        }
        if full.send(buf).is_err() {
            return;
        }
    }
}

fn write_frames(dst: &mut impl Write, full: Receiver<Vec<u16>>, free: Sender<Vec<u16>>) -> Result<u64> {
    let mut n = 0;
    for buf in full.iter() {
        dst.write_all(as_bytes(&buf)).context("the encoder stopped accepting frames")?;
        n += 1;
        let _ = free.send(buf);
    }
    Ok(n)
}

/// A sample type the compositor can resample: a luma byte, or an NV12
/// chroma pair (U in the low byte, V in the high byte).
trait Sample: Copy + Send + Sync {
    /// Blends toward `b` by `w` (0..=256).
    fn lerp(a: Self, b: Self, w: u32) -> Self;
}

impl Sample for u8 {
    #[inline(always)]
    fn lerp(a: u8, b: u8, w: u32) -> u8 {
        ((a as u32 * (256 - w) + b as u32 * w + 128) >> 8) as u8
    }
}

impl Sample for u16 {
    #[inline(always)]
    fn lerp(a: u16, b: u16, w: u32) -> u16 {
        // Spread both bytes into 16-bit lanes and blend them in one multiply.
        let spread = |x: u16| (x as u32 & 0xFF) | ((x as u32 & 0xFF00) << 8);
        let r = ((spread(a) * (256 - w) + spread(b) * w + 0x0080_0080) >> 8) & 0x00FF_00FF;
        (r | (r >> 8)) as u16
    }
}

#[derive(Clone, Copy)]
struct Tap {
    x0: u32,
    x1: u32,
    w: u32,
}

/// Bilinear source taps for `count` output samples covering `[start, start + span)`
/// of an axis `len` samples long.
fn taps(start: f32, span: f32, count: usize, len: usize) -> Vec<Tap> {
    let step = span / count as f32;
    let max = (len - 1) as f32;
    (0..count)
        .map(|i| {
            let s = (start + (i as f32 + 0.5) * step - 0.5).clamp(0.0, max);
            let x0 = s.floor() as u32;
            let x1 = (x0 + 1).min(len as u32 - 1);
            let w = ((s - x0 as f32) * 256.0).round() as u32;
            Tap { x0, x1, w }
        })
        .collect()
}

/// One image plane plus the geometry the compositor needs for it.
struct Plane<T> {
    w: usize,
    h: usize,
    rect: ContentRect,
    canvas: Vec<T>,
    /// Coverage of the rounded corner, `mask_size` x `mask_size`.
    mask: Vec<u8>,
    mask_size: usize,
}

impl<T: Sample> Plane<T> {
    /// Places the view `(x0, y0, view_w, view_h)` of `src` (in this plane's
    /// sample units) into the content rectangle, over the canvas.
    fn compose(&self, src: &[T], dst: &mut [T], view: (f32, f32, f32, f32)) {
        use rayon::prelude::*;
        let (w, rect, r) = (self.w, self.rect, self.mask_size);
        let xt = taps(view.0, view.2, rect.w, w);
        let yt = taps(view.1, view.3, rect.h, self.h);
        dst.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let canvas_row = &self.canvas[y * w..(y + 1) * w];
            if y < rect.y || y >= rect.y + rect.h {
                row.copy_from_slice(canvas_row);
                return;
            }
            row[..rect.x].copy_from_slice(&canvas_row[..rect.x]);
            row[rect.x + rect.w..].copy_from_slice(&canvas_row[rect.x + rect.w..]);

            let j = y - rect.y;
            let ty = yt[j];
            let r0 = &src[ty.x0 as usize * w..ty.x0 as usize * w + w];
            let r1 = &src[ty.x1 as usize * w..ty.x1 as usize * w + w];
            let out = &mut row[rect.x..rect.x + rect.w];
            if ty.w == 0 {
                for (o, tx) in out.iter_mut().zip(&xt) {
                    *o = T::lerp(r0[tx.x0 as usize], r0[tx.x1 as usize], tx.w);
                }
            } else {
                for (o, tx) in out.iter_mut().zip(&xt) {
                    let top = T::lerp(r0[tx.x0 as usize], r0[tx.x1 as usize], tx.w);
                    let bot = T::lerp(r1[tx.x0 as usize], r1[tx.x1 as usize], tx.w);
                    *o = T::lerp(top, bot, ty.w);
                }
            }

            // Rounded corners: blend the recording into the canvas by coverage.
            let corner_row = if j < r {
                Some(j)
            } else if j >= rect.h - r {
                Some(rect.h - 1 - j)
            } else {
                None
            };
            if let Some(dy) = corner_row {
                let mask = &self.mask[dy * r..(dy + 1) * r];
                for (dx, &cov) in mask.iter().enumerate() {
                    if cov == 255 {
                        continue;
                    }
                    let a = cov as u32 + (cov as u32 >> 7); // 0..=256
                    let right = rect.w - 1 - dx;
                    out[dx] = T::lerp(canvas_row[rect.x + dx], out[dx], a);
                    out[right] = T::lerp(canvas_row[rect.x + right], out[right], a);
                }
            }
        });
    }
}

/// RGB to limited-range BT.601 YCbCr, the matrix the capture encoders use.
fn rgb_to_ycbcr(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (
        16.0 + 0.256_788 * r + 0.504_129 * g + 0.097_906 * b,
        128.0 - 0.148_223 * r - 0.290_993 * g + 0.439_216 * b,
        128.0 + 0.439_216 * r - 0.367_788 * g - 0.071_427 * b,
    )
}

/// Converts an RGB0 canvas to NV12 planes.
fn canvas_to_nv12(rgb0: &[u8], w: usize, h: usize) -> (Vec<u8>, Vec<u16>) {
    let px = |x: usize, y: usize| {
        let o = (y * w + x) * 4;
        (rgb0[o] as f32, rgb0[o + 1] as f32, rgb0[o + 2] as f32)
    };
    let mut luma = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = px(x, y);
            luma[y * w + x] = rgb_to_ycbcr(r, g, b).0.round().clamp(0.0, 255.0) as u8;
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    let mut chroma = vec![0u16; cw * ch];
    for y in 0..ch {
        for x in 0..cw {
            let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = px(2 * x + dx, 2 * y + dy);
                r += p.0 * 0.25;
                g += p.1 * 0.25;
                b += p.2 * 0.25;
            }
            let (_, u, v) = rgb_to_ycbcr(r, g, b);
            let u = u.round().clamp(0.0, 255.0) as u16;
            let v = v.round().clamp(0.0, 255.0) as u16;
            chroma[y * cw + x] = u | (v << 8);
        }
    }
    (luma, chroma)
}

/// Composites NV12 frames: the camera's view of the recording, placed in the
/// content rectangle over the background canvas.
pub struct Compositor {
    w: usize,
    h: usize,
    luma: Plane<u8>,
    chroma: Plane<u16>,
}

impl Compositor {
    /// `canvas_rgb0` is the background from `render_canvas`. Sizes and the
    /// content rectangle must be even, as NV12 requires.
    pub fn new(w: usize, h: usize, rect: ContentRect, canvas_rgb0: Vec<u8>) -> Self {
        assert!(w % 2 == 0 && h % 2 == 0 && rect.x % 2 == 0 && rect.y % 2 == 0);
        assert!(rect.w % 2 == 0 && rect.h % 2 == 0);
        let (canvas_y, canvas_uv) = canvas_to_nv12(&canvas_rgb0, w, h);
        let half = ContentRect {
            x: rect.x / 2,
            y: rect.y / 2,
            w: rect.w / 2,
            h: rect.h / 2,
            radius: rect.radius.div_ceil(2),
        };
        Self {
            w,
            h,
            luma: Plane {
                w,
                h,
                rect,
                canvas: canvas_y,
                mask: corner_mask(rect.radius, rect.radius as f32),
                mask_size: rect.radius,
            },
            chroma: Plane {
                w: w / 2,
                h: h / 2,
                rect: half,
                canvas: canvas_uv,
                mask: corner_mask(half.radius, rect.radius as f32 / 2.0),
                mask_size: half.radius,
            },
        }
    }

    /// Writes one NV12 output frame for the camera `(center_x, center_y, zoom)`.
    pub fn compose(&self, src: &[u16], dst: &mut [u16], (cx, cy, zoom): Camera) {
        let (w, h) = (self.w, self.h);
        let zoom = zoom.clamp(1.0, 4.0);
        if zoom <= 1.001 && self.luma.rect == ContentRect::full(w, h) {
            dst.copy_from_slice(src);
            return;
        }
        let view_w = w as f32 / zoom;
        let view_h = h as f32 / zoom;
        let x0 = (cx * w as f32 - view_w * 0.5).clamp(0.0, w as f32 - view_w);
        let y0 = (cy * h as f32 - view_h * 0.5).clamp(0.0, h as f32 - view_h);

        let split = w * h / 2;
        let (src_y, src_uv) = src.split_at(split);
        let (dst_y, dst_uv) = dst.split_at_mut(split);
        self.luma.compose(as_bytes(src_y), as_bytes_mut(dst_y), (x0, y0, view_w, view_h));
        self.chroma
            .compose(src_uv, dst_uv, (x0 / 2.0, y0 / 2.0, view_w / 2.0, view_h / 2.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid NV12 frame.
    fn frame(w: usize, h: usize, y: u8, u: u8, v: u8) -> Vec<u16> {
        let mut f = vec![0u16; w * h * 3 / 4];
        let split = w * h / 2;
        as_bytes_mut(&mut f[..split]).fill(y);
        f[split..].fill(u as u16 | ((v as u16) << 8));
        f
    }

    fn luma(f: &[u16], w: usize, x: usize, y: usize) -> u8 {
        as_bytes(f)[y * w + x]
    }

    #[test]
    fn lerp_blends_with_rounding() {
        assert_eq!(<u8 as Sample>::lerp(0, 255, 0), 0);
        assert_eq!(<u8 as Sample>::lerp(0, 255, 256), 255);
        assert_eq!(<u8 as Sample>::lerp(100, 200, 128), 150);
        let a = 10u16 | (200 << 8);
        let b = 250u16 | (100 << 8);
        assert_eq!(<u16 as Sample>::lerp(a, b, 0), a);
        assert_eq!(<u16 as Sample>::lerp(a, b, 256), b);
        assert_eq!(<u16 as Sample>::lerp(a, b, 128), 130 | (150 << 8));
    }

    #[test]
    fn identity_frame_is_copied_bit_exact() {
        let (w, h) = (8, 4);
        let mut src = frame(w, h, 0, 0, 0);
        for (i, b) in as_bytes_mut(&mut src).iter_mut().enumerate() {
            *b = i as u8;
        }
        let comp = Compositor::new(w, h, ContentRect::full(w, h), vec![0; w * h * 4]);
        let mut dst = frame(w, h, 0, 0, 0);
        comp.compose(&src, &mut dst, (0.5, 0.5, 1.0));
        assert_eq!(dst, src);
    }

    #[test]
    fn framed_frame_keeps_canvas_outside_and_content_inside() {
        let (w, h) = (40, 20);
        let src = frame(w, h, 200, 90, 160);
        let rect = ContentRect { x: 4, y: 2, w: 32, h: 16, radius: 4 };
        // Mid-grey canvas: Y = 126 for RGB 128.
        let canvas: Vec<u8> = std::iter::repeat([128u8, 128, 128, 255]).take(w * h).flatten().collect();
        let comp = Compositor::new(w, h, rect, canvas);
        let mut dst = frame(w, h, 0, 0, 0);
        comp.compose(&src, &mut dst, (0.5, 0.5, 1.0));
        assert_eq!(luma(&dst, w, 0, 0), 126);
        assert_eq!(luma(&dst, w, 20, 10), 200);
        // The outermost corner pixel of the content is mostly canvas.
        assert!(luma(&dst, w, 4, 2) < 160, "corner {}", luma(&dst, w, 4, 2));
        let uv = dst[w * h / 2 + 5 * (w / 2) + 10];
        assert_eq!(uv, 90 | (160 << 8));
    }

    #[test]
    fn zoom_samples_the_region_around_the_camera_center() {
        let (w, h) = (64, 32);
        let mut src = frame(w, h, 0, 128, 128);
        // Left half dark, right half bright.
        for (i, b) in as_bytes_mut(&mut src[..w * h / 2]).iter_mut().enumerate() {
            *b = if i % w < w / 2 { 16 } else { 235 };
        }
        let comp = Compositor::new(w, h, ContentRect::full(w, h), vec![0; w * h * 4]);
        let mut dst = frame(w, h, 0, 0, 0);
        comp.compose(&src, &mut dst, (0.9, 0.5, 2.0));
        // The view is the bright right half; only the first column may blend
        // with the dark sample just left of it.
        for y in 0..h {
            for x in 0..w {
                let min = if x == 0 { 150 } else { 230 };
                assert!(luma(&dst, w, x, y) >= min, "({x}, {y}) = {}", luma(&dst, w, x, y));
            }
        }
    }

    fn job_with(events: Vec<RecordedZoomEvent>) -> RenderJob {
        RenderJob {
            capture: PathBuf::new(),
            output: PathBuf::new(),
            events,
            auto_zoom: true,
            region: None,
            background: BackgroundSettings::default(),
            quality: Quality::High,
            fps: 60,
        }
    }

    fn clicking(off_capture: bool) -> Vec<RecordedZoomEvent> {
        (0..400)
            .map(|i| RecordedZoomEvent {
                timestamp_sec: i as f32 * 0.02,
                center_x: 0.3,
                center_y: 0.3,
                zoom: 2.0,
                cursor_x: 0.3,
                cursor_y: 0.3,
                clicked: i % 100 == 10,
                manual_lock: false,
                off_capture,
            })
            .collect()
    }

    #[test]
    fn clicks_on_the_recorded_display_zoom_but_other_displays_do_not() {
        assert!(camera_track(&job_with(clicking(false)), 8.0).is_some());
        assert!(camera_track(&job_with(clicking(true)), 8.0).is_none());
    }

    #[test]
    fn canvas_converts_to_limited_range_bt601() {
        let rgb: Vec<u8> = [[255u8, 255, 255, 255], [0, 0, 0, 255]]
            .iter()
            .cycle()
            .take(4)
            .flatten()
            .copied()
            .collect();
        let (y, uv) = canvas_to_nv12(&rgb, 2, 2);
        assert_eq!(y, vec![235, 16, 235, 16]);
        assert_eq!(uv, vec![128 | (128 << 8)]);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore = "benchmark; run with --ignored --nocapture"]
    fn compose_1080p_throughput() {
        let (w, h) = (1920, 1080);
        let src: Vec<u16> = (0..w * h * 3 / 4).map(|i| (i as u32).wrapping_mul(2654435761) as u16).collect();
        let settings = crate::settings::BackgroundSettings {
            kind: crate::settings::BackgroundKind::Preset(0),
            ..Default::default()
        };
        let rect = ContentRect::framed(w, h, 0.06, 0.018);
        let t = std::time::Instant::now();
        let canvas = render_canvas(&settings, w, h, &rect).unwrap();
        println!("canvas render: {:?}", t.elapsed());
        let comp = Compositor::new(w, h, rect, canvas);
        let mut dst = vec![0u16; w * h * 3 / 4];
        for (label, zoom) in [("framed 1.0x", 1.0), ("framed 2.0x", 2.0)] {
            let t = std::time::Instant::now();
            for i in 0..300 {
                comp.compose(&src, &mut dst, (0.4 + i as f32 * 0.0005, 0.5, zoom));
            }
            println!("{label}: {:.0} fps", 300.0 / t.elapsed().as_secs_f64());
        }
    }
}
