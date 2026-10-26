//! The cursor recorded beside the video when the portal sends it as
//! metadata instead of drawing it into the frames. The studio render draws
//! it back on a smoothed path, at the chosen size, over the zoomed frame.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One cursor image in straight-alpha RGBA.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub w: usize,
    pub h: usize,
    pub hot_x: f32,
    pub hot_y: f32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Video time in seconds.
    pub t: f64,
    /// Hotspot position in stream pixels.
    pub x: f32,
    pub y: f32,
    /// 1-based index into `shapes`; 0 when the cursor is hidden.
    pub shape: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CursorTrack {
    /// Size of the stream the positions are in, when known.
    pub stream_size: Option<(u32, u32)>,
    pub shapes: Vec<Shape>,
    pub samples: Vec<Sample>,
}

/// Where the capture helper writes the cursor of `capture`.
pub fn track_path(capture: &Path) -> PathBuf {
    capture.with_extension("cursor")
}

impl CursorTrack {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        Self::parse(&text)
    }

    /// Parses the helper's track (see `pw_portal_capture.c`). A track cut
    /// short by a crash keeps its complete lines.
    pub fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some("HGSR-CURSOR 1") {
            bail!("not a cursor track");
        }
        let mut track = CursorTrack::default();
        // Helper shape id -> index + 1 in `shapes`.
        let mut ids = std::collections::HashMap::new();
        let mut raw: Vec<(u64, f32, f32, usize)> = Vec::new();
        let mut zero_ns = None;
        for line in lines {
            let mut f = line.split_ascii_whitespace();
            match f.next() {
                Some("S") => {
                    let mut num = || f.next().and_then(|v| v.parse::<i64>().ok());
                    let (Some(id), Some(w), Some(h), Some(hx), Some(hy)) = (num(), num(), num(), num(), num()) else {
                        continue;
                    };
                    let (w, h) = (w as usize, h as usize);
                    let Some(rgba) = f.next().and_then(decode_hex) else { continue };
                    if w == 0 || h == 0 || w > 256 || h > 256 || rgba.len() != w * h * 4 {
                        continue;
                    }
                    track.shapes.push(Shape { w, h, hot_x: hx as f32, hot_y: hy as f32, rgba });
                    ids.insert(id, track.shapes.len());
                }
                Some("P") => {
                    let (Some(ns), Some(x), Some(y), Some(id)) = (
                        f.next().and_then(|v| v.parse::<u64>().ok()),
                        f.next().and_then(|v| v.parse::<f32>().ok()),
                        f.next().and_then(|v| v.parse::<f32>().ok()),
                        f.next().and_then(|v| v.parse::<i64>().ok()),
                    ) else {
                        continue;
                    };
                    let shape = if id == 0 { 0 } else { ids.get(&id).copied().unwrap_or(0) };
                    raw.push((ns, x, y, shape));
                }
                Some("T") if zero_ns.is_none() => {
                    zero_ns = f.next().and_then(|v| v.parse::<u64>().ok());
                    let mut num = || f.next().and_then(|v| v.parse::<u32>().ok());
                    track.stream_size = num().zip(num());
                }
                _ => {}
            }
        }
        let Some(zero) = zero_ns else { bail!("the capture never started") };
        // Samples before the first frame only set where the cursor starts.
        let first = raw.iter().rposition(|s| s.0 <= zero).unwrap_or(0);
        track.samples = raw[first..]
            .iter()
            .map(|&(ns, x, y, shape)| Sample {
                t: (ns.saturating_sub(zero)) as f64 / 1e9,
                x,
                y,
                shape,
            })
            .collect();
        Ok(track)
    }

    /// Whether the cursor was ever visible.
    pub fn is_useful(&self) -> bool {
        self.samples.iter().any(|s| s.shape != 0)
    }

    /// Hotspot position at video time `t`, or `None` while the cursor is
    /// hidden or off the recorded stream.
    pub fn position_at(&self, t: f64) -> Option<(f32, f32)> {
        let i = self.samples.partition_point(|s| s.t <= t).checked_sub(1)?;
        let s = self.samples[i];
        (s.shape != 0).then_some((s.x, s.y))
    }

    /// Appends `other`, moved by `shift` seconds and cut after `last`
    /// seconds of its own time (the joined video's timeline).
    pub fn append_shifted(&mut self, other: CursorTrack, shift: f64, last: f64) {
        let base = self.shapes.len();
        self.stream_size = self.stream_size.or(other.stream_size);
        self.shapes.extend(other.shapes);
        // The previous part's cursor ends where its video ends.
        if let Some(end) = self.samples.last().copied() {
            if end.shape != 0 && shift > end.t {
                self.samples.push(Sample { t: shift, shape: 0, ..end });
            }
        }
        self.samples.extend(other.samples.into_iter().filter(|s| s.t <= last).map(|s| Sample {
            t: s.t + shift,
            shape: if s.shape == 0 { 0 } else { s.shape + base },
            ..s
        }));
    }

    /// Cursor at each of `frames` output frames: position in capture pixels
    /// (the stream minus `crop` offset) and 1-based shape, smoothed so it
    /// glides instead of jittering.
    pub fn frames(&self, fps: u32, frames: usize, crop: (f32, f32)) -> Vec<Option<(f32, f32, usize)>> {
        let mut out = Vec::with_capacity(frames);
        let mut next = 0;
        let mut current: Option<Sample> = None;
        for i in 0..frames {
            let t = i as f64 / fps as f64;
            while next < self.samples.len() && self.samples[next].t <= t {
                current = Some(self.samples[next]);
                next += 1;
            }
            out.push(current.filter(|s| s.shape != 0).map(|s| (s.x - crop.0, s.y - crop.1, s.shape)));
        }
        smooth(&mut out, fps);
        out
    }
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    bytes.chunks_exact(2).map(|p| Some(digit(p[0])? << 4 | digit(p[1])?)).collect()
}

/// Gaussian smoothing of the path, without lag since the whole recording is
/// known. Runs of visible frames are smoothed separately, and so are the
/// sides of a jump (a warp, or the cursor crossing to another display).
fn smooth(path: &mut [Option<(f32, f32, usize)>], fps: u32) {
    const SIGMA_SEC: f32 = 0.05;
    const JUMP_PX: f32 = 600.0;
    let sigma = SIGMA_SEC * fps as f32;
    let radius = (sigma * 3.0).ceil() as usize;
    let weights: Vec<f32> = (0..=radius).map(|d| (-(d as f32).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
    let raw: Vec<_> = path.to_vec();
    let mut start = 0;
    while start < raw.len() {
        if raw[start].is_none() {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < raw.len() {
            let (Some(a), Some(b)) = (raw[end - 1], raw[end]) else { break };
            if (b.0 - a.0).abs().max((b.1 - a.1).abs()) > JUMP_PX {
                break;
            }
            end += 1;
        }
        for i in start..end {
            let lo = i.saturating_sub(radius).max(start);
            let hi = (i + radius + 1).min(end);
            let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
            for (j, p) in raw.iter().enumerate().take(hi).skip(lo) {
                let (x, y, _) = p.expect("visible run");
                let w = weights[i.abs_diff(j)];
                sx += x * w;
                sy += y * w;
                sw += w;
            }
            if let Some(p) = path[i].as_mut() {
                p.0 = sx / sw;
                p.1 = sy / sw;
            }
        }
        start = end;
    }
}

/// A shape prepared for blending into NV12: premultiplied limited-range
/// BT.601 Y, Cb, Cr and alpha per texel, scaled 0..1 for alpha.
pub struct PreparedShape {
    w: usize,
    h: usize,
    hot_x: f32,
    hot_y: f32,
    texels: Vec<[f32; 4]>,
}

impl PreparedShape {
    pub fn new(shape: &Shape) -> Self {
        let texels = shape
            .rgba
            .chunks_exact(4)
            .map(|p| {
                let a = p[3] as f32 / 255.0;
                let (r, g, b) = (p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a);
                [
                    16.0 * a + 0.256_788 * r + 0.504_129 * g + 0.097_906 * b,
                    128.0 * a - 0.148_223 * r - 0.290_993 * g + 0.439_216 * b,
                    128.0 * a + 0.439_216 * r - 0.367_788 * g - 0.071_427 * b,
                    a,
                ]
            })
            .collect();
        Self { w: shape.w, h: shape.h, hot_x: shape.hot_x, hot_y: shape.hot_y, texels }
    }

    /// Bilinear sample at texel coordinates; transparent outside.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |xi: f32, yi: f32| -> [f32; 4] {
            if xi < 0.0 || yi < 0.0 || xi >= self.w as f32 || yi >= self.h as f32 {
                [0.0; 4]
            } else {
                self.texels[yi as usize * self.w + xi as usize]
            }
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            top + (bot - top) * fy
        })
    }
}

/// Where a capture pixel lands in the output frame.
#[derive(Clone, Copy)]
pub struct Placement {
    /// Content rectangle in the output (x, y, w, h).
    pub rect: (usize, usize, usize, usize),
    /// Part of the capture shown in the rectangle (x0, y0, w, h).
    pub view: (f32, f32, f32, f32),
}

/// Draws `shape` with its hotspot at capture pixel `(x, y)` into the NV12
/// frame (`w` x `h`, chroma pairs after the luma), `scale` times its size
/// on top of the zoom. Drawing is clipped to the content rectangle.
pub fn draw_nv12(frame: &mut [u8], w: usize, h: usize, place: Placement, shape: &PreparedShape, (x, y): (f32, f32), scale: f32) {
    let (rx, ry, rw, rh) = place.rect;
    let (vx, vy, vw, _) = place.view;
    let k = rw as f32 / vw.max(1.0);
    let size = scale * k;
    let ox = rx as f32 + (x - vx) * k - shape.hot_x * size;
    let oy = ry as f32 + (y - vy) * k - shape.hot_y * size;
    // Even-aligned box so each chroma sample sees its whole 2x2 block.
    let x0 = (ox.floor().max(rx as f32) as usize) & !1;
    let y0 = (oy.floor().max(ry as f32) as usize) & !1;
    let x1 = ((ox + shape.w as f32 * size).ceil().min((rx + rw) as f32).max(0.0) as usize + 1) & !1;
    let y1 = ((oy + shape.h as f32 * size).ceil().min((ry + rh) as f32).max(0.0) as usize + 1) & !1;
    let (x1, y1) = (x1.min(w), y1.min(h));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (luma, chroma) = frame.split_at_mut(w * h);
    let inside = |px: usize, py: usize| px >= rx && px < rx + rw && py >= ry && py < ry + rh;
    for cy in (y0..y1).step_by(2) {
        for cx in (x0..x1).step_by(2) {
            let (mut u, mut v, mut a) = (0.0, 0.0, 0.0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (px, py) = (cx + dx, cy + dy);
                if !inside(px, py) {
                    continue;
                }
                let s = shape.sample((px as f32 + 0.5 - ox) / size, (py as f32 + 0.5 - oy) / size);
                if s[3] <= 0.0 {
                    continue;
                }
                let l = &mut luma[py * w + px];
                *l = (*l as f32 * (1.0 - s[3]) + s[0]).round().clamp(0.0, 255.0) as u8;
                u += s[1];
                v += s[2];
                a += s[3];
            }
            if a <= 0.0 {
                continue;
            }
            let (u, v, a) = (u / 4.0, v / 4.0, a / 4.0);
            let o = (cy / 2) * w + (cx / 2) * 2;
            chroma[o] = (chroma[o] as f32 * (1.0 - a) + u).round().clamp(0.0, 255.0) as u8;
            chroma[o + 1] = (chroma[o + 1] as f32 * (1.0 - a) + v).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// How long a click ripple lasts, in seconds.
pub const RIPPLE_SEC: f64 = 0.45;

/// Draws a click ripple centred on capture pixel `(x, y)`, `progress`
/// (0..1) through its animation: an indigo ring that grows and fades over
/// a soft disc, sized with the zoom.
pub fn draw_ripple_nv12(frame: &mut [u8], w: usize, h: usize, place: Placement, (x, y): (f32, f32), progress: f32) {
    // Indigo (99, 102, 241) in limited-range BT.601.
    const COLOR: (f32, f32, f32) = (106.6, 199.4, 132.2);
    let (rx, ry, rw, rh) = place.rect;
    let (vx, vy, vw, _) = place.view;
    let k = rw as f32 / vw.max(1.0);
    let p = progress.clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - p).powi(3);
    let radius = (8.0 + 22.0 * ease) * k;
    let half_ring = 1.6 * k.max(1.0);
    let ring_alpha = 0.9 * (1.0 - p).powi(2);
    let disc_alpha = 0.22 * (1.0 - p);
    let cx = rx as f32 + (x - vx) * k;
    let cy = ry as f32 + (y - vy) * k;
    let reach = radius + half_ring + 1.0;
    let x0 = ((cx - reach).floor().max(rx as f32) as usize) & !1;
    let y0 = ((cy - reach).floor().max(ry as f32) as usize) & !1;
    let x1 = (((cx + reach).ceil().min((rx + rw) as f32).max(0.0) as usize) + 1).min(w) & !1;
    let y1 = (((cy + reach).ceil().min((ry + rh) as f32).max(0.0) as usize) + 1).min(h) & !1;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let alpha = |px: usize, py: usize| -> f32 {
        if px < rx || px >= rx + rw || py < ry || py >= ry + rh {
            return 0.0;
        }
        let d = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
        let ring = (half_ring - (d - radius).abs() + 0.5).clamp(0.0, 1.0);
        let disc = (radius - d + 0.5).clamp(0.0, 1.0);
        (ring * ring_alpha + disc * disc_alpha).min(1.0)
    };
    let (luma, chroma) = frame.split_at_mut(w * h);
    for by in (y0..y1).step_by(2) {
        for bx in (x0..x1).step_by(2) {
            let mut sum = 0.0;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let a = alpha(bx + dx, by + dy);
                if a > 0.0 {
                    let l = &mut luma[(by + dy) * w + bx + dx];
                    *l = (*l as f32 + (COLOR.0 - *l as f32) * a).round() as u8;
                    sum += a;
                }
            }
            if sum > 0.0 {
                let a = sum / 4.0;
                let o = (by / 2) * w + (bx / 2) * 2;
                chroma[o] = (chroma[o] as f32 + (COLOR.1 - chroma[o] as f32) * a).round() as u8;
                chroma[o + 1] = (chroma[o + 1] as f32 + (COLOR.2 - chroma[o + 1] as f32) * a).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: &str = "HGSR-CURSOR 1\n\
        S 1 2 1 0 0 ff0000ff00ff00ff\n\
        P 500 10 20 1\n\
        P 900 11 21 1\n\
        T 1000 1920 1080\n\
        P 1500000000 300 400 1\n\
        P 2000000000 300 400 0\n\
        P 2500000000 30 40 1\n\
        P 3000000000 5 5";

    #[test]
    fn parses_shapes_and_times_samples_from_the_first_frame() {
        let t = CursorTrack::parse(TRACK).unwrap();
        assert_eq!(t.shapes.len(), 1);
        assert_eq!(t.stream_size, Some((1920, 1080)));
        assert_eq!(t.shapes[0].rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
        // The sample just before the first frame is where the cursor starts;
        // the cut-off last line is dropped.
        assert_eq!(t.samples.len(), 4);
        assert_eq!((t.samples[0].t, t.samples[0].x), (0.0, 11.0));
        assert!((t.samples[1].t - 1.5).abs() < 1e-6);
        assert_eq!(t.samples[2].shape, 0);
        assert!(t.is_useful());
    }

    #[test]
    fn tracks_without_a_first_frame_are_rejected() {
        assert!(CursorTrack::parse("HGSR-CURSOR 1\nP 1 1 1 0\n").is_err());
        assert!(CursorTrack::parse("garbage").is_err());
    }

    #[test]
    fn frames_hold_positions_hide_and_apply_the_crop() {
        let t = CursorTrack::parse(TRACK).unwrap();
        let f = t.frames(10, 30, (10.0, 20.0));
        assert_eq!(f[0].map(|p| p.2), Some(1));
        assert!(f[21].is_none(), "hidden at 2.1 s");
        let (x, y, _) = f[29].unwrap();
        assert!((x - 20.0).abs() < 1e-3 && (y - 20.0).abs() < 1e-3, "({x}, {y})");
    }

    #[test]
    fn smoothing_glides_but_keeps_jumps() {
        let mut path: Vec<_> = (0..60).map(|i| Some((if i < 30 { 0.0 } else { 100.0 }, 0.0, 1))).collect();
        smooth(&mut path, 60);
        let x = |i: usize| path[i].unwrap().0;
        assert!(x(29) > 1.0 && x(29) < 50.0 && x(30) > 50.0 && x(30) < 99.0);
        assert!(x(0) < 1e-3 && (x(59) - 100.0).abs() < 1e-3);

        let mut warp: Vec<_> = (0..20).map(|i| Some((if i < 10 { 0.0 } else { 1500.0 }, 0.0, 1))).collect();
        smooth(&mut warp, 60);
        assert_eq!(warp[9].unwrap().0, 0.0);
        assert_eq!(warp[10].unwrap().0, 1500.0);
    }

    #[test]
    fn parts_join_on_one_timeline() {
        let mut a = CursorTrack::parse(TRACK).unwrap();
        let b = CursorTrack::parse(TRACK).unwrap();
        a.append_shifted(b, 10.0, 2.0);
        assert_eq!(a.shapes.len(), 2);
        let tail: Vec<_> = a.samples.iter().filter(|s| s.t >= 10.0).collect();
        // The first part's cursor is hidden at the seam, then the second
        // part's samples follow, cut after its own 2 s.
        assert_eq!(tail[0].shape, 0);
        assert_eq!(tail[1].shape, 2);
        assert!(tail.iter().all(|s| s.t <= 12.0));
    }

    #[test]
    fn ripples_tint_their_ring_and_stay_inside_the_content() {
        let (w, h) = (64, 64);
        let mut frame = vec![200u8; w * h];
        frame.extend(std::iter::repeat_n(128u8, w * h / 2));
        let place = Placement { rect: (0, 0, 40, 64), view: (0.0, 0.0, 40.0, 64.0) };
        draw_ripple_nv12(&mut frame, w, h, place, (20.0, 32.0), 0.0);
        // On the ring (radius 8), toward indigo's darker luma.
        assert!(frame[32 * w + 28] < 140, "{}", frame[32 * w + 28]);
        assert_eq!(frame[2 * w + 2], 200);
        // A ripple at the edge is clipped to the content rectangle.
        draw_ripple_nv12(&mut frame, w, h, place, (39.0, 32.0), 0.0);
        assert_eq!(frame[32 * w + 41], 200);
        // A finished ripple draws nothing.
        let before = frame.clone();
        draw_ripple_nv12(&mut frame, w, h, place, (20.0, 10.0), 1.0);
        assert_eq!(frame, before);
    }

    #[test]
    fn drawing_blends_inside_the_content_only() {
        let (w, h) = (8, 4);
        let mut frame = vec![16u8; w * h];
        frame.extend(std::iter::repeat_n(128u8, w * h / 2));
        let white = Shape { w: 2, h: 2, hot_x: 0.0, hot_y: 0.0, rgba: vec![255; 16] };
        let place = Placement { rect: (0, 0, 4, 4), view: (0.0, 0.0, 4.0, 4.0) };
        draw_nv12(&mut frame, w, h, place, &PreparedShape::new(&white), (2.0, 2.0), 1.0);
        assert_eq!(frame[2 * w + 2], 235);
        assert_eq!(frame[3 * w + 3], 235);
        assert_eq!(frame[w + 1], 16);
        // Clipped at the rectangle's right edge.
        draw_nv12(&mut frame, w, h, place, &PreparedShape::new(&white), (3.0, 0.0), 1.0);
        assert_eq!(frame[3], 235);
        assert_eq!(frame[4], 16);
    }
}
