//! The webcam bubble. The camera is recorded beside the screen by its own
//! FFmpeg, keeping the kernel's CLOCK_MONOTONIC frame stamps, the clock the
//! capture helper reports video time zero on. The studio render lines the
//! two up and draws the camera as a round bubble in a corner.

use crate::settings::Webcam;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Frame rate the camera is recorded and drawn at.
const FPS: f64 = 30.0;

/// A video capture device.
#[derive(Debug, Clone)]
pub struct Device {
    pub path: PathBuf,
    pub name: String,
}

/// Cameras, in device order. Each camera also exposes metadata nodes; only
/// the first node of a device (`index` 0) captures video.
pub fn devices() -> Vec<Device> {
    let mut found: Vec<Device> = std::fs::read_dir("/sys/class/video4linux")
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .filter(|e| std::fs::read_to_string(e.path().join("index")).is_ok_and(|i| i.trim() == "0"))
                .map(|e| Device {
                    path: Path::new("/dev").join(e.file_name()),
                    name: std::fs::read_to_string(e.path().join("name"))
                        .map(|n| n.trim().to_string())
                        .unwrap_or_default(),
                })
                .filter(|d| d.path.exists())
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// The camera to record: `HYPERGSR_WEBCAM`, or the first one.
pub fn default_device() -> Option<Device> {
    if let Some(path) = std::env::var_os("HYPERGSR_WEBCAM") {
        let path = PathBuf::from(path);
        return Some(Device { name: path.display().to_string(), path });
    }
    devices().into_iter().next()
}

/// CLOCK_MONOTONIC now, in seconds.
fn monotonic_now() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Where a part of the joined video sits on the monotonic clock: joined
/// time `t` in `shift..shift + last` was captured at `zero + (t - shift)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClockSpan {
    pub shift: f64,
    pub zero: f64,
    pub last: f64,
}

/// The recorded camera, ready to render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebcamClip {
    pub path: PathBuf,
    /// CLOCK_MONOTONIC time of the camera's first frame.
    pub start: f64,
    /// How video time maps to the monotonic clock.
    pub clock: Vec<ClockSpan>,
    pub corner: Webcam,
    /// Bubble diameter as a fraction of the output's shorter side.
    pub size: f32,
}

impl WebcamClip {
    /// Index of the camera frame shown at video time `t`, if the camera was
    /// recording then.
    pub fn frame_at(&self, t: f64) -> Option<i64> {
        let span = self.clock.iter().rev().find(|s| t >= s.shift - 1e-6)?;
        if t > span.shift + span.last {
            return None;
        }
        let k = ((span.zero + (t - span.shift) - self.start) * FPS).round() as i64;
        (k >= 0).then_some(k)
    }

    /// Bubble diameter (even) and center for a `w` x `h` output.
    pub fn placement(&self, w: usize, h: usize) -> (usize, (f32, f32)) {
        let short = w.min(h) as f32;
        let d = ((short * self.size.clamp(0.1, 0.4) / 2.0).round() as usize * 2).max(16);
        let margin = (short * 0.04).round();
        let r = d as f32 / 2.0;
        let x = match self.corner {
            Webcam::Left => margin + r,
            _ => w as f32 - margin - r,
        };
        (d, (x, h as f32 - margin - r))
    }
}

/// Whether `device` offers 640x480 MJPEG. Asked once per device, with a
/// timeout, since a wedged camera can block opening forever.
fn offers_small_mjpeg(device: &Path) -> bool {
    static KNOWN: std::sync::Mutex<Vec<(PathBuf, bool)>> = std::sync::Mutex::new(Vec::new());
    if let Some(&(_, mjpeg)) = KNOWN.lock().ok().and_then(|k| k.iter().find(|(d, _)| d == device).cloned()).as_ref() {
        return mjpeg;
    }
    let probe = Command::new("ffmpeg")
        .args(["-hide_banner", "-f", "v4l2", "-list_formats", "compressed", "-i"])
        .arg(device)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut probe) = probe else { return false };
    let deadline = Instant::now() + Duration::from_secs(2);
    while probe.try_wait().ok().flatten().is_none() {
        if Instant::now() > deadline {
            let _ = probe.kill();
            let _ = probe.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut text = String::new();
    if let Some(mut err) = probe.stderr.take() {
        let _ = err.read_to_string(&mut text);
    }
    let mjpeg = text.lines().any(|l| l.contains("mjpeg") && l.contains("640x480"));
    if let Ok(mut known) = KNOWN.lock() {
        known.push((device.to_path_buf(), mjpeg));
    }
    mjpeg
}

/// Learns the default camera's formats ahead of the first recording.
pub fn warm_up() {
    if let Some(device) = default_device() {
        offers_small_mjpeg(&device.path);
    }
}

/// Records a camera to a file while the screen is recorded.
pub struct WebcamRecorder {
    /// Taken when stopped; a recorder dropped while running kills it.
    child: Option<Child>,
    path: PathBuf,
    /// CLOCK_MONOTONIC when FFmpeg was started, to sanity-check its stamps.
    spawned: f64,
}

impl WebcamRecorder {
    pub fn start(device: &Path, output: &Path) -> Result<Self> {
        // A small MJPEG stream keeps USB bandwidth and decoding cheap; other
        // cameras record in their default mode.
        let mjpeg = offers_small_mjpeg(device);
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "v4l2"]);
        if mjpeg {
            cmd.args(["-input_format", "mjpeg", "-video_size", "640x480", "-framerate", "30"]);
        }
        cmd.arg("-i")
            .arg(device)
            .args(["-map", "0:v:0", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "24"])
            .args(["-pix_fmt", "yuv420p", "-g", "60"])
            // Keep the driver's monotonic stamps.
            .args(["-copyts", "-f", "matroska"])
            .arg(output)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // The camera must never outlive the recorder, even if it is killed.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let spawned = monotonic_now();
        let child = cmd.spawn().context("cannot start FFmpeg for the webcam")?;
        Ok(Self { child: Some(child), path: output.to_path_buf(), spawned })
    }

    /// Stops recording; returns the file and when its first frame was taken.
    pub fn finish(mut self) -> Option<(PathBuf, f64)> {
        if let Some(mut child) = self.child.take() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"q");
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait().ok().flatten().is_none() {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // A camera that sent no frames leaves nothing worth keeping.
        let Some(start) = first_stamp(&self.path) else {
            let _ = std::fs::remove_file(&self.path);
            return None;
        };
        // A camera stamping another clock is placed by when it started.
        let start = if (start - self.spawned).abs() < 15.0 { start } else { self.spawned + 0.5 };
        Some((std::mem::take(&mut self.path), start))
    }

    /// Stops recording and deletes the file.
    pub fn discard(self) {
        if let Some((path, _)) = self.finish() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for WebcamRecorder {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The first frame's timestamp, from FFmpeg's `start:`.
fn first_stamp(path: &Path) -> Option<f64> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-copyts", "-i"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .ok()?;
    parse_start(&String::from_utf8_lossy(&out.stderr))
}

fn parse_start(banner: &str) -> Option<f64> {
    let rest = &banner[banner.find("start: ")? + "start: ".len()..];
    rest.split(',').next()?.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Decodes the camera as square `d` x `d` RGBA frames at [`FPS`], mirrored.
pub struct WebcamFeed {
    child: Child,
    out: ChildStdout,
    d: usize,
    frame: Vec<u8>,
    /// Index of the frame in `frame`, -1 before the first.
    index: i64,
    ended: bool,
}

impl WebcamFeed {
    pub fn open(clip: &WebcamClip, d: usize) -> Result<Self> {
        if d < 2 || !d.is_multiple_of(2) {
            bail!("bad bubble size {d}");
        }
        let filter = format!(
            "fps={FPS},crop='min(iw,ih)':'min(iw,ih)',scale={d}:{d}:flags=bicubic,hflip,format=rgba"
        );
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&clip.path)
            .args(["-map", "0:v:0", "-vf", &filter, "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .context("cannot decode the webcam")?;
        let out = child.stdout.take().context("webcam decoder has no stdout")?;
        Ok(Self { child, out, d, frame: vec![0; d * d * 4], index: -1, ended: false })
    }

    /// Frame `k`, reading forward as needed. Frames before the current one
    /// are gone; the current one is returned instead.
    pub fn frame_at(&mut self, k: i64) -> Option<&[u8]> {
        while self.index < k && !self.ended {
            if self.out.read_exact(&mut self.frame).is_err() {
                self.ended = true;
                break;
            }
            self.index += 1;
        }
        // Past the end, the bubble goes away.
        if self.index < 0 || (self.ended && k > self.index + FPS as i64 / 2) {
            return None;
        }
        Some(&self.frame)
    }

    pub fn size(&self) -> usize {
        self.d
    }
}

impl Drop for WebcamFeed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn coverage(edge: f32) -> f32 {
    (edge + 0.5).clamp(0.0, 1.0)
}

/// Draws a `d` x `d` RGBA camera frame as a round bubble centered at
/// `center` of an NV12 frame, with a light ring and a soft shadow.
pub fn draw_bubble_nv12(frame: &mut [u8], w: usize, h: usize, rgba: &[u8], d: usize, center: (f32, f32)) {
    let r = d as f32 / 2.0;
    let ring = (d as f32 * 0.022).max(2.0);
    let outer = r + ring;
    let blur = d as f32 * 0.09;
    let drop = d as f32 * 0.03;
    let (cx, cy) = center;
    let left = cx - r;
    let top = cy - r;
    let reach = outer + blur + drop;
    let x0 = ((cx - reach).floor().max(0.0) as usize) & !1;
    let y0 = ((cy - reach).floor().max(0.0) as usize) & !1;
    let x1 = ((cx + reach).ceil() as usize).min(w) & !1;
    let y1 = ((cy + reach).ceil() as usize).min(h) & !1;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // (shadow, ring, image) coverage at a point.
    let layers = |px: f32, py: f32| {
        let dist = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
        let shadow_dist = ((px - cx).powi(2) + (py - cy - drop).powi(2)).sqrt();
        let shadow = 0.32 * ((outer + blur - shadow_dist) / blur).clamp(0.0, 1.0);
        (shadow, coverage(outer - dist), coverage(r - dist))
    };
    let sample = |px: f32, py: f32| -> [f32; 3] {
        let u = ((px - left).floor().max(0.0) as usize).min(d - 1);
        let v = ((py - top).floor().max(0.0) as usize).min(d - 1);
        let o = (v * d + u) * 4;
        [rgba[o] as f32, rgba[o + 1] as f32, rgba[o + 2] as f32]
    };
    let ring_rgb = [244.0f32, 244.0, 248.0];
    let (luma, chroma) = frame.split_at_mut(w * h);
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (shadow, ring_cov, img_cov) = layers(px, py);
            if shadow == 0.0 && ring_cov == 0.0 {
                continue;
            }
            let o = y * w + x;
            let mut v = luma[o] as f32;
            v += (16.0 - v) * shadow * (1.0 - ring_cov);
            let ring_y = crate::studio_render::rgb_to_ycbcr(ring_rgb[0], ring_rgb[1], ring_rgb[2]).0;
            v += (ring_y - v) * ring_cov;
            if img_cov > 0.0 {
                let [r8, g8, b8] = sample(px, py);
                v += (crate::studio_render::rgb_to_ycbcr(r8, g8, b8).0 - v) * img_cov;
            }
            luma[o] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    let cw = w / 2;
    for y in (y0 / 2)..(y1 / 2) {
        for x in (x0 / 2)..(x1 / 2) {
            let (px, py) = (2.0 * x as f32 + 1.0, 2.0 * y as f32 + 1.0);
            let (shadow, ring_cov, img_cov) = layers(px, py);
            if shadow == 0.0 && ring_cov == 0.0 {
                continue;
            }
            let o = (y * cw + x) * 2;
            let (mut u, mut vv) = (chroma[o] as f32, chroma[o + 1] as f32);
            // The shadow darkens toward neutral; the ring is neutral too.
            let neutral = shadow * (1.0 - ring_cov) + ring_cov;
            u += (128.0 - u) * neutral.min(1.0);
            vv += (128.0 - vv) * neutral.min(1.0);
            if img_cov > 0.0 {
                let mut acc = [0.0f32; 3];
                for (dx, dy) in [(-0.5, -0.5), (0.5, -0.5), (-0.5, 0.5), (0.5, 0.5)] {
                    let c = sample(px + dx, py + dy);
                    for i in 0..3 {
                        acc[i] += c[i] * 0.25;
                    }
                }
                let (_, cu, cv) = crate::studio_render::rgb_to_ycbcr(acc[0], acc[1], acc[2]);
                u += (cu - u) * img_cov;
                vv += (cv - vv) * img_cov;
            }
            chroma[o] = u.round().clamp(0.0, 255.0) as u8;
            chroma[o + 1] = vv.round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(clock: Vec<ClockSpan>) -> WebcamClip {
        WebcamClip { path: PathBuf::new(), start: 100.0, clock, corner: Webcam::Right, size: 0.2 }
    }

    #[test]
    fn video_time_maps_to_camera_frames_across_parts() {
        // Part 1 began at monotonic 101 s; part 2 (joined at 10 s) at 130 s.
        let c = clip(vec![
            ClockSpan { shift: 0.0, zero: 101.0, last: 10.0 },
            ClockSpan { shift: 10.0, zero: 130.0, last: f64::INFINITY },
        ]);
        assert_eq!(c.frame_at(0.0), Some(30));
        assert_eq!(c.frame_at(1.0), Some(60));
        assert_eq!(c.frame_at(10.5), Some(915));
        // A camera that started after the screen shows nothing at first.
        let late = WebcamClip { start: 102.0, ..clip(vec![ClockSpan { shift: 0.0, zero: 101.0, last: 5.0 }]) };
        assert_eq!(late.frame_at(0.5), None);
        assert_eq!(late.frame_at(1.5), Some(15));
        assert_eq!(late.frame_at(6.0), None);
    }

    #[test]
    fn a_camera_that_records_nothing_leaves_no_file() {
        let out = std::env::temp_dir().join(format!("hypergsr-cam-{}.webcam.mkv", std::process::id()));
        let missing = Path::new("/dev/hypergsr-no-such-camera");
        let cam = WebcamRecorder::start(missing, &out).expect("FFmpeg starts");
        assert!(cam.finish().is_none());
        assert!(!out.exists());
        // Dropped while running: stopped, and its file removed.
        let cam = WebcamRecorder::start(missing, &out).expect("FFmpeg starts");
        drop(cam);
        assert!(!out.exists());
    }

    #[test]
    fn reads_the_first_stamp() {
        let banner = "Input #0, matroska,webm, from 'a.mkv':\n  Duration: 00:00:02.03, start: 12345.678000, bitrate: 10 kb/s\n";
        assert_eq!(parse_start(banner), Some(12345.678));
    }

    #[test]
    fn the_bubble_is_round_with_a_ring_and_stays_in_frame() {
        let (w, h) = (64, 48);
        let mut frame = vec![16u8; w * h * 3 / 2];
        frame[w * h..].fill(128);
        let d = 20;
        let rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], d * d).flatten().collect();
        let c = clip(Vec::new());
        let (_, center) = WebcamClip { size: 20.0 / 48.0, ..c }.placement(w, h);
        draw_bubble_nv12(&mut frame, w, h, &rgba, d, center);
        let at = |x: f32, y: f32| frame[y as usize * w + x as usize];
        // Red center: BT.601 luma of pure red is about 82.
        assert!((at(center.0, center.1) as i32 - 82).abs() <= 2, "{}", at(center.0, center.1));
        // Light ring just outside the image.
        assert!(at(center.0 + 10.6, center.1) > 150);
        // Corners of the bubble's box stay background.
        assert_eq!(at(center.0 - 9.5, center.1 - 9.5), 16);
        // Red chroma at the center block.
        let o = w * h + ((center.1 as usize / 2) * (w / 2) + center.0 as usize / 2) * 2;
        assert!(frame[o + 1] > 200, "Cr {}", frame[o + 1]);
        // Placed in the bottom-right corner, inside the frame.
        assert!(center.0 + 10.0 < w as f32 && center.1 + 10.0 < h as f32 && center.0 > w as f32 / 2.0);
    }
}
