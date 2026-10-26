//! Turns a raw capture into the final video: the cursor-following zoom, the
//! presentation background, cuts, and the output shape are applied in one
//! pass.
//!
//! Decoding, compositing, and encoding run as a three-stage pipeline so the
//! CPU work overlaps both FFmpeg processes. When nothing would change a
//! pixel, the capture is only remuxed, which takes seconds.

use crate::backgrounds::{corner_mask, render_canvas, ContentRect};
use crate::cursor_track::{self, CursorTrack, Placement, PreparedShape};
use crate::encoder::{build_smooth_60fps_camera_frames, RecordedZoomEvent};
use crate::ffmpeg::{self, VideoEncoder};
use crate::settings::{Aspect, BackgroundKind, BackgroundSettings, ExportFormat, Quality, Region};
use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
    /// The cursor recorded beside a capture that has none drawn in; it is
    /// drawn on a smoothed path, `cursor_scale` times its size.
    pub cursor: Option<CursorTrack>,
    pub cursor_scale: f32,
    /// Show a ripple where each recorded click happened.
    pub click_ripples: bool,
    /// Video-time ranges `(start, end)` left out of the output: a countdown,
    /// pauses, a trim. `end` may be infinite.
    pub cuts: Vec<(f64, f64)>,
    pub aspect: Aspect,
    pub format: ExportFormat,
    /// The webcam, drawn as a bubble.
    #[serde(default)]
    pub webcam: Option<crate::webcam::WebcamClip>,
}

/// The parts of a `duration`-second video that `cuts` keep, in order.
pub fn kept_ranges(cuts: &[(f64, f64)], duration: f64) -> Vec<(f64, f64)> {
    let mut cuts: Vec<(f64, f64)> = cuts
        .iter()
        .map(|&(a, b)| (a.max(0.0), b.min(duration)))
        .filter(|(a, b)| b > a)
        .collect();
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut kept = Vec::new();
    let mut at = 0.0;
    for (a, b) in cuts {
        if a > at {
            kept.push((at, a));
        }
        at = at.max(b);
    }
    if duration > at {
        kept.push((at, duration));
    }
    kept
}

/// Audio filter that keeps what `kept` keeps (video time; the video's first
/// frame sits at `video_start` in the file) and closes the gaps.
fn audio_select(kept: &[(f64, f64)], video_start: f64) -> String {
    let terms: Vec<String> = kept
        .iter()
        .map(|(a, b)| format!("between(t,{:.4},{:.4})", a + video_start, b + video_start))
        .collect();
    format!("aselect='{}',asetpts=N/SR/TB", terms.join("+"))
}

/// Cursor at each output frame, ready to draw.
struct CursorFrames {
    shapes: Vec<PreparedShape>,
    /// Capture-pixel hotspot and 1-based shape per frame.
    path: Vec<Option<(f32, f32, usize)>>,
    scale: f32,
}

/// Where the capture's top-left corner sits in the stream the cursor track
/// is measured in: the capture is cropped the way `Region::crop_filter`
/// rounds it.
fn cursor_crop(job: &RenderJob, track: &CursorTrack, capture: (usize, usize)) -> (f32, f32) {
    job.region.map_or((0.0, 0.0), |r| {
        let (sw, sh) = track.stream_size.map_or(
            (capture.0 as f32 / r.w, capture.1 as f32 / r.h),
            |(w, h)| (w as f32, h as f32),
        );
        ((sw * r.x).trunc(), (sh * r.y).trunc())
    })
}

impl CursorFrames {
    fn new(job: &RenderJob, info_w: usize, info_h: usize, frames: usize) -> Option<Self> {
        let track = job.cursor.as_ref()?;
        let crop = cursor_crop(job, track, (info_w, info_h));
        Some(Self {
            shapes: track.shapes.iter().map(PreparedShape::new).collect(),
            path: track.frames(job.fps, frames, crop),
            scale: job.cursor_scale.clamp(1.0, 2.0),
        })
    }

    fn draw(&self, index: u64, frame: &mut [u16], compositor: &Compositor, cam: Camera) {
        let Some(&Some((x, y, shape))) = self.path.get(index as usize) else { return };
        let Some(shape) = self.shapes.get(shape - 1) else { return };
        let place = compositor.placement(cam);
        cursor_track::draw_nv12(as_bytes_mut(frame), compositor.w, compositor.h, place, shape, (x, y), self.scale);
    }
}

/// Video time and capture-pixel position of each click to show a ripple
/// for. With a cursor track, the ripple sits on the drawn cursor.
fn click_points(job: &RenderJob, capture: (usize, usize), cursor: Option<&CursorFrames>) -> Vec<(f64, f32, f32)> {
    if !job.click_ripples {
        return Vec::new();
    }
    job.events
        .iter()
        .filter(|e| e.clicked && !e.off_capture)
        .filter_map(|e| {
            let t = f64::from(e.timestamp_sec);
            let (x, y) = match cursor {
                Some(c) => {
                    let (x, y, _) = (*c.path.get((t * job.fps as f64).round() as usize)?)?;
                    (x, y)
                }
                None => {
                    let (x, y) = job.region.map_or((e.cursor_x, e.cursor_y), |r| r.map_point(e.cursor_x, e.cursor_y));
                    (x * capture.0 as f32, y * capture.1 as f32)
                }
            };
            Some((t, x, y))
        })
        .collect()
}

/// Camera at one output frame: center (normalized) and zoom factor.
type Camera = (f32, f32, f32);

/// The camera path for a capture of `capture` pixels. With a cursor track,
/// the zoom follows the track, which is measured on the recorded stream
/// itself (a window, or the right display); clicks and manual zoom still
/// come from the live telemetry.
fn camera_track(job: &RenderJob, duration_sec: f64, capture: (usize, usize)) -> Option<Vec<Camera>> {
    if !job.auto_zoom || job.events.is_empty() {
        return None;
    }
    let on_stream = |e: &RecordedZoomEvent| -> RecordedZoomEvent {
        let Some(track) = &job.cursor else { return *e };
        let crop = cursor_crop(job, track, capture);
        match track.position_at(f64::from(e.timestamp_sec)) {
            Some((x, y)) => {
                let x = ((x - crop.0) / capture.0 as f32).clamp(0.0, 1.0);
                let y = ((y - crop.1) / capture.1 as f32).clamp(0.0, 1.0);
                RecordedZoomEvent { cursor_x: x, cursor_y: y, center_x: x, center_y: y, off_capture: false, ..*e }
            }
            // The cursor is not over the recording.
            None => RecordedZoomEvent { off_capture: true, ..*e },
        }
    };
    let events: Vec<RecordedZoomEvent> = job
        .events
        .iter()
        .map(on_stream)
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
                    ..e
                };
            }
            let Some(region) = job.region.filter(|_| job.cursor.is_none()) else { return e };
            let (cx, cy) = region.map_point(e.cursor_x, e.cursor_y);
            let (zx, zy) = region.map_point(e.center_x, e.center_y);
            RecordedZoomEvent {
                cursor_x: cx,
                cursor_y: cy,
                center_x: zx,
                center_y: zy,
                ..e
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
    let track = camera_track(job, info.duration_sec, (info.width, info.height));
    let video_sec = (info.duration_sec - info.video_start_sec).max(0.0);
    let expected = ((video_sec * job.fps as f64).round() as u64).max(1);
    // Container durations can exceed the video by a few frames (audio tail).
    let slack = (job.fps as u64 / 2).max(3);
    let cursor = CursorFrames::new(job, info.width, info.height, expected as usize + slack as usize * 4);
    let clicks = click_points(job, (info.width, info.height), cursor.as_ref());
    let (src_w, src_h) = (info.width, info.height);
    let (w, h) = job.aspect.output_size((src_w, src_h));
    let reshaped = (w, h) != (src_w, src_h);
    let kept = if job.cuts.is_empty() {
        vec![(0.0, f64::INFINITY)]
    } else {
        kept_ranges(&job.cuts, video_sec + 1.0)
    };
    if kept.is_empty() {
        bail!("nothing is left after the cuts");
    }
    let plain = track.is_none()
        && !job.background.is_enabled()
        && cursor.is_none()
        && clicks.is_empty()
        && job.webcam.is_none();
    if plain && !reshaped && job.cuts.is_empty() && job.format == ExportFormat::Mp4 {
        ffmpeg::remux_to_mp4(&job.capture, &job.output)?;
        progress.store(1000, Ordering::Relaxed);
        return Ok(job.output.clone());
    }

    if src_w % 2 != 0 || src_h % 2 != 0 || src_w < 16 || src_h < 16 {
        bail!("cannot render a {src_w}x{src_h} capture (NV12 needs even sizes)");
    }
    // A new shape without a background still needs something around the
    // recording: a quiet dark canvas, edge to edge.
    let background = match (&job.background.kind, reshaped) {
        (BackgroundKind::None, true) => BackgroundSettings {
            kind: BackgroundKind::Preset(3),
            padding: 0.0,
            radius: 0.0,
            shadow: false,
        },
        _ => job.background.clone(),
    };
    let rect = if background.is_enabled() {
        ContentRect::fit(w, h, src_w, src_h, background.padding, background.radius)
    } else {
        ContentRect::full(w, h)
    };
    let canvas = render_canvas(&background, w, h, &rect)?;
    let compositor = Compositor::new(w, h, (src_w, src_h), rect, canvas);

    let fps = job.fps as f64;
    let first_frame = (kept[0].0 * fps).floor() as u64;
    let end = kept.last().map_or(f64::INFINITY, |r| r.1);
    let last_frame = if end.is_finite() { ((end * fps).ceil() as u64).min(expected) } else { expected };
    let window = DecodeWindow {
        first_frame,
        frames: last_frame.saturating_sub(first_frame).max(1),
        bounded: end.is_finite(),
        kept: if job.cuts.is_empty() { None } else { Some(kept) },
    };

    let tmp = job.output.with_file_name(format!(
        ".{}.rendering.{}",
        job.output.file_stem().and_then(|s| s.to_str()).unwrap_or("HyperGSR"),
        job.format.extension()
    ));
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("hypergsr-render-{i}"))
        .build()
        .context("cannot start render threads")?;
    let encoder = ffmpeg::capabilities().encoder.clone();
    // A webcam that cannot be decoded leaves the video without its bubble.
    let mut webcam = job.webcam.as_ref().and_then(|clip| {
        let (d, center) = clip.placement(w, h);
        crate::webcam::WebcamFeed::open(clip, d)
            .map_err(|err| eprintln!("[HyperGSR] {err:#}; saving without the webcam"))
            .ok()
            .map(|feed| (clip, feed, center))
    });
    let frames = pool.install(|| {
        let effects = Effects {
            compositor: &compositor,
            camera: track.as_deref(),
            cursor: cursor.as_ref(),
            clicks: &clicks,
            webcam: webcam.as_mut().map(|(clip, feed, center)| (*clip, feed, *center)),
        };
        run_pipeline(job, &encoder, effects, &tmp, &window, info.video_start_sec, progress)
    });

    match frames {
        // A decoder that read to the end vouches for the frame count (a crashed
        // capture's header overstates its length by up to a keyframe interval).
        // Otherwise anything short means the decoder stopped early.
        Ok((n, decoder_ok)) if n > 0 && (decoder_ok || n + slack >= window.frames) => {
            std::fs::rename(&tmp, &job.output)?;
            progress.store(1000, Ordering::Relaxed);
            Ok(job.output.clone())
        }
        other => {
            let _ = std::fs::remove_file(&tmp);
            let reason = match other {
                Ok((n, _)) => format!("only {n} of {} frames were rendered", window.frames),
                Err(err) => format!("{err:#}"),
            };
            bail!("studio render failed: {reason}")
        }
    }
}

/// Which input frames the render reads and which it keeps.
struct DecodeWindow {
    /// Index of the first frame decoded; earlier ones are all cut.
    first_frame: u64,
    /// How many frames are decoded from there.
    frames: u64,
    /// Stop after `frames`; otherwise the decoder reads to the end, which
    /// the container's duration may understate.
    bounded: bool,
    /// Video-time ranges to keep, or `None` to keep every frame.
    kept: Option<Vec<(f64, f64)>>,
}

impl DecodeWindow {
    fn keeps(&self, t: f64) -> bool {
        self.kept.as_ref().is_none_or(|k| k.iter().any(|&(a, b)| t >= a && t < b))
    }
}

/// Encoder arguments for `format` (video, then audio), given raw NV12 input.
fn export_args(format: ExportFormat, encoder: &VideoEncoder, quality: Quality, audio_filter: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let push = |args: &mut Vec<String>, items: &[&str]| args.extend(items.iter().map(|s| s.to_string()));
    match format {
        ExportFormat::Mp4 => {
            push(&mut args, &["-map", "0:v:0", "-map", "1:a?"]);
            if let Some(filter) = audio_filter {
                push(&mut args, &["-af", filter, "-c:a", "aac", "-b:a", "192k"]);
            } else {
                push(&mut args, &["-c:a", "copy"]);
            }
            args.extend(ffmpeg::video_encoder_args(encoder, quality, &[]));
            push(&mut args, ffmpeg::COLOR_TAGS);
            push(&mut args, &["-movflags", "+faststart", "-f", "mp4"]);
        }
        ExportFormat::WebM => {
            push(&mut args, &["-map", "0:v:0", "-map", "1:a?"]);
            if let Some(filter) = audio_filter {
                push(&mut args, &["-af", filter]);
            }
            let crf = match quality {
                Quality::Standard => "36",
                Quality::High => "30",
            };
            push(&mut args, &["-c:a", "libopus", "-b:a", "128k"]);
            push(&mut args, &["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "7", "-row-mt", "1"]);
            push(&mut args, &["-crf", crf, "-b:v", "0", "-pix_fmt", "yuv420p"]);
            push(&mut args, ffmpeg::COLOR_TAGS);
            push(&mut args, &["-f", "webm"]);
        }
        ExportFormat::Gif => {
            push(&mut args, &["-map", "0:v:0", "-an"]);
            push(&mut args, &[
                "-vf",
                "fps=15,scale='min(960,iw)':-2:flags=lanczos,split[a][b];\
                 [a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4",
            ]);
            push(&mut args, &["-loop", "0", "-f", "gif"]);
        }
    }
    args
}

fn lower_priority(cmd: &mut Command) -> &mut Command {
    unsafe {
        cmd.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        })
    }
}

/// What the render pass applies to each frame.
struct Effects<'a> {
    compositor: &'a Compositor,
    camera: Option<&'a [Camera]>,
    cursor: Option<&'a CursorFrames>,
    /// From [`click_points`], in time order.
    clicks: &'a [(f64, f32, f32)],
    /// The webcam, its decoder, and where the bubble's center goes.
    webcam: Option<(&'a crate::webcam::WebcamClip, &'a mut crate::webcam::WebcamFeed, (f32, f32))>,
}

fn run_pipeline(
    job: &RenderJob,
    encoder: &VideoEncoder,
    mut effects: Effects<'_>,
    tmp: &Path,
    window: &DecodeWindow,
    video_start_sec: f64,
    progress: &AtomicU32,
) -> Result<(u64, bool)> {
    let compositor = effects.compositor;
    let (w, h) = (compositor.w, compositor.h);
    let (src_w, src_h) = compositor.src;
    let fps_f = job.fps as f64;
    // Start half a frame early so the first frame decoded is `first_frame`.
    let seek = if window.first_frame > 0 {
        vec![
            "-ss".to_string(),
            format!("{:.6}", video_start_sec + (window.first_frame as f64 - 0.5) / fps_f),
        ]
    } else {
        Vec::new()
    };
    let span = if window.bounded {
        vec!["-t".to_string(), format!("{:.6}", window.frames as f64 / fps_f)]
    } else {
        Vec::new()
    };
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
            .args(&seek)
            .args(&span)
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
    // With cuts, the audio is cut to match and starts with the first kept
    // frame. Otherwise raw frames start at zero and are shifted back to where
    // the capture's first frame sits relative to its audio.
    let audio_filter = window.kept.as_ref().map(|kept| audio_select(kept, video_start_sec));
    let video_offset = if audio_filter.is_some() { 0.0 } else { video_start_sec };
    let video_offset = format!("{video_offset:.6}");
    let mut enc_cmd = Command::new("ffmpeg");
    enc_cmd
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-itsoffset", &video_offset])
        .args(["-f", "rawvideo", "-pix_fmt", "nv12", "-video_size", &size, "-framerate", &fps, "-i", "pipe:0"])
        .arg("-i")
        .arg(&job.capture)
        .args(export_args(job.format, encoder, job.quality, audio_filter.as_deref()))
        .arg(tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut enc = lower_priority(&mut enc_cmd)
        .spawn()
        .context("cannot start the ffmpeg encoder")?;

    let mut dec_out = dec.stdout.take().context("decoder has no stdout")?;
    let mut enc_in = enc.stdin.take().context("encoder has no stdin")?;
    // NV12 frames held as u16 so the interleaved chroma plane is aligned.
    let in_words = src_w * src_h * 3 / 4;
    let out_words = w * h * 3 / 4;
    const DEPTH: usize = 4;
    let (free_in_tx, free_in_rx) = bounded::<Vec<u16>>(DEPTH);
    let (full_in_tx, full_in_rx) = bounded::<Vec<u16>>(DEPTH);
    let (free_out_tx, free_out_rx) = bounded::<Vec<u16>>(DEPTH);
    let (full_out_tx, full_out_rx) = bounded::<Vec<u16>>(DEPTH);
    for _ in 0..DEPTH {
        free_in_tx.send(vec![0u16; in_words]).ok();
        free_out_tx.send(vec![0u16; out_words]).ok();
    }

    let reached_eof = std::sync::atomic::AtomicBool::new(false);
    let written = std::thread::scope(|s| -> Result<(u64, bool)> {
        let reached_eof = &reached_eof;
        let reader = s.spawn(move || read_frames(&mut dec_out, free_in_rx, full_in_tx, reached_eof));
        let writer = s.spawn(move || write_frames(&mut enc_in, full_out_rx, free_out_tx));

        // `index` counts decoded frames; `frame` is the input frame number.
        let mut index = 0u64;
        while let Ok(input) = full_in_rx.recv() {
            let frame = window.first_frame + index;
            let t = frame as f64 / fps_f;
            index += 1;
            if !window.keeps(t) {
                let _ = free_in_tx.send(input);
                continue;
            }
            let Ok(mut output) = free_out_rx.recv() else { break };
            let cam = effects
                .camera
                .map(|tr| tr[((t * 60.0).round() as usize).min(tr.len() - 1)])
                .unwrap_or((0.5, 0.5, 1.0));
            compositor.compose(&input, &mut output, cam);
            let started = effects.clicks.partition_point(|c| c.0 <= t);
            for &(tc, x, y) in effects.clicks[..started].iter().rev() {
                if t - tc >= cursor_track::RIPPLE_SEC {
                    break;
                }
                let place = compositor.placement(cam);
                let progress = ((t - tc) / cursor_track::RIPPLE_SEC) as f32;
                cursor_track::draw_ripple_nv12(as_bytes_mut(&mut output), w, h, place, (x, y), progress);
            }
            if let Some(cursor) = effects.cursor {
                cursor.draw(frame, &mut output, compositor, cam);
            }
            if let Some((clip, feed, center)) = effects.webcam.as_mut() {
                let d = feed.size();
                if let Some(px) = clip.frame_at(t).and_then(|k| feed.frame_at(k)) {
                    crate::webcam::draw_bubble_nv12(as_bytes_mut(&mut output), w, h, px, d, *center);
                }
            }
            let _ = free_in_tx.send(input);
            if full_out_tx.send(output).is_err() {
                break;
            }
            if index % 15 == 0 {
                progress.store(((index * 990) / window.frames).min(990) as u32, Ordering::Relaxed);
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
        written?;
        Ok((index, decoder_done))
    });

    let dec_status = dec.wait();
    let status = enc.wait()?;
    let (decoded, decoder_done) = written?;
    if !status.success() {
        bail!("the ffmpeg encoder exited with {status}");
    }
    if decoder_done && !dec_status.as_ref().is_ok_and(|s| s.success()) {
        bail!("the ffmpeg decoder failed partway ({dec_status:?})");
    }
    Ok((decoded, decoder_done))
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
    /// Size of the recording's plane.
    src_w: usize,
    src_h: usize,
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
        let sw = self.src_w;
        let xt = taps(view.0, view.2, rect.w, sw);
        let yt = taps(view.1, view.3, rect.h, self.src_h);
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
            let r0 = &src[ty.x0 as usize * sw..ty.x0 as usize * sw + sw];
            let r1 = &src[ty.x1 as usize * sw..ty.x1 as usize * sw + sw];
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
pub(crate) fn rgb_to_ycbcr(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
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
    /// Size of the recording, which may differ from the output's.
    src: (usize, usize),
    luma: Plane<u8>,
    chroma: Plane<u16>,
}

impl Compositor {
    /// Places a `src`-sized recording in `rect` of a `w` x `h` output;
    /// `canvas_rgb0` is the background from `render_canvas`. Sizes and the
    /// content rectangle must be even, as NV12 requires.
    pub fn new(w: usize, h: usize, src: (usize, usize), rect: ContentRect, canvas_rgb0: Vec<u8>) -> Self {
        assert!(w % 2 == 0 && h % 2 == 0 && rect.x % 2 == 0 && rect.y % 2 == 0);
        assert!(src.0 % 2 == 0 && src.1 % 2 == 0);
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
            src,
            luma: Plane {
                w,
                src_w: src.0,
                src_h: src.1,
                rect,
                canvas: canvas_y,
                mask: corner_mask(rect.radius, rect.radius as f32),
                mask_size: rect.radius,
            },
            chroma: Plane {
                w: w / 2,
                src_w: src.0 / 2,
                src_h: src.1 / 2,
                rect: half,
                canvas: canvas_uv,
                mask: corner_mask(half.radius, rect.radius as f32 / 2.0),
                mask_size: half.radius,
            },
        }
    }

    /// Part of the capture `(x0, y0, w, h)` the camera shows.
    fn view(&self, (cx, cy, zoom): Camera) -> (f32, f32, f32, f32) {
        let (w, h) = (self.src.0 as f32, self.src.1 as f32);
        let zoom = zoom.clamp(1.0, 4.0);
        let (view_w, view_h) = (w / zoom, h / zoom);
        let x0 = (cx * w - view_w * 0.5).clamp(0.0, w - view_w);
        let y0 = (cy * h - view_h * 0.5).clamp(0.0, h - view_h);
        (x0, y0, view_w, view_h)
    }

    /// Where capture pixels land in the output for the camera.
    fn placement(&self, cam: Camera) -> Placement {
        let r = self.luma.rect;
        Placement { rect: (r.x, r.y, r.w, r.h), view: self.view(cam) }
    }

    /// Writes one NV12 output frame for the camera `(center_x, center_y, zoom)`.
    pub fn compose(&self, src: &[u16], dst: &mut [u16], cam: Camera) {
        let (w, h) = (self.w, self.h);
        if cam.2 <= 1.001 && self.src == (w, h) && self.luma.rect == ContentRect::full(w, h) {
            dst.copy_from_slice(src);
            return;
        }
        let (x0, y0, view_w, view_h) = self.view(cam);

        let (src_y, src_uv) = src.split_at(self.src.0 * self.src.1 / 2);
        let (dst_y, dst_uv) = dst.split_at_mut(w * h / 2);
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
        let comp = Compositor::new(w, h, (w, h), ContentRect::full(w, h), vec![0; w * h * 4]);
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
        let comp = Compositor::new(w, h, (w, h), rect, canvas);
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
        let comp = Compositor::new(w, h, (w, h), ContentRect::full(w, h), vec![0; w * h * 4]);
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
            cursor: None,
            cursor_scale: 1.0,
            click_ripples: true,
            cuts: Vec::new(),
            aspect: Aspect::Original,
            format: ExportFormat::Mp4,
            webcam: None,
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
        assert!(camera_track(&job_with(clicking(false)), 8.0, (1920, 1080)).is_some());
        assert!(camera_track(&job_with(clicking(true)), 8.0, (1920, 1080)).is_none());
    }

    fn track_at(x: f32, y: f32, visible: bool) -> CursorTrack {
        let shape = crate::cursor_track::Shape { w: 1, h: 1, hot_x: 0.0, hot_y: 0.0, rgba: vec![0, 0, 0, 255] };
        CursorTrack {
            stream_size: Some((1920, 1080)),
            shapes: vec![shape],
            samples: vec![crate::cursor_track::Sample { t: 0.0, x, y, shape: usize::from(visible) }],
        }
    }

    #[test]
    fn a_cursor_track_steers_the_zoom_instead_of_screen_telemetry() {
        // Screen telemetry says (0.3, 0.3); the recorded stream's own cursor
        // is at (0.75, 0.75).
        let mut job = job_with(clicking(false));
        job.cursor = Some(track_at(1440.0, 810.0, true));
        let track = camera_track(&job, 8.0, (1920, 1080)).expect("zooms");
        let (cx, cy, z) = track[3 * 60];
        assert!(z > 1.3 && cx > 0.6 && cy > 0.6, "camera {cx} {cy} {z}");

        // A cursor that is never over the recording leaves it wide.
        job.cursor = Some(track_at(1440.0, 810.0, false));
        assert!(camera_track(&job, 8.0, (1920, 1080)).is_none());
    }

    #[test]
    fn cuts_leave_the_ranges_between_them() {
        assert_eq!(kept_ranges(&[], 10.0), vec![(0.0, 10.0)]);
        // Countdown, a pause, and a trim after 8 s; overlaps merge.
        let kept = kept_ranges(&[(0.0, 3.0), (5.0, 6.0), (5.5, 6.5), (8.0, f64::INFINITY)], 10.0);
        assert_eq!(kept, vec![(3.0, 5.0), (6.5, 8.0)]);
        assert!(kept_ranges(&[(0.0, f64::INFINITY)], 10.0).is_empty());
        let filter = audio_select(&kept, 0.25);
        assert_eq!(filter, "aselect='between(t,3.2500,5.2500)+between(t,6.7500,8.2500)',asetpts=N/SR/TB");
    }

    #[test]
    fn a_wide_recording_is_letterboxed_into_a_vertical_frame() {
        let (sw, sh) = (32, 16);
        let src = frame(sw, sh, 200, 90, 160);
        let (w, h) = (16, 32);
        let rect = ContentRect::fit(w, h, sw, sh, 0.0, 0.0);
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (0, 12, 16, 8));
        let canvas: Vec<u8> = std::iter::repeat([0u8, 0, 0, 255]).take(w * h).flatten().collect();
        let comp = Compositor::new(w, h, (sw, sh), rect, canvas);
        let mut dst = frame(w, h, 0, 0, 0);
        comp.compose(&src, &mut dst, (0.5, 0.5, 1.0));
        assert_eq!(luma(&dst, w, 8, 2), 16, "canvas above");
        assert_eq!(luma(&dst, w, 8, 16), 200, "recording in the middle");
        assert_eq!(luma(&dst, w, 8, 30), 16, "canvas below");
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
        let comp = Compositor::new(w, h, (w, h), rect, canvas);
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
