//! Presentation backgrounds: the canvas the recording is framed on, with a
//! soft drop shadow, plus the rounded-corner coverage mask for the recording.
//!
//! Everything here is rendered once per export; the per-frame compositor in
//! `studio_render` only copies and blends the results.

use crate::settings::{BackgroundKind, BackgroundSettings};
use anyhow::{Context, Result};

/// A soft color field: a linear gradient plus a few blurred color glows.
pub struct Preset {
    pub name: &'static str,
    /// Gradient angle in degrees (0 = left to right, 90 = top to bottom).
    pub angle: f32,
    pub stops: &'static [(f32, [u8; 3])],
    /// `(x, y, radius, color, strength)`, positions and radius relative to the
    /// canvas width.
    pub glows: &'static [(f32, f32, f32, [u8; 3], f32)],
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Aurora",
        angle: 120.0,
        stops: &[(0.0, [14, 22, 66]), (0.55, [42, 32, 118]), (1.0, [12, 94, 120])],
        glows: &[
            (0.12, 0.15, 0.55, [56, 189, 248], 0.55),
            (0.88, 0.85, 0.60, [217, 70, 239], 0.50),
        ],
    },
    Preset {
        name: "Sunset",
        angle: 135.0,
        stops: &[(0.0, [255, 138, 76]), (0.5, [236, 72, 120]), (1.0, [109, 40, 217])],
        glows: &[
            (0.15, 0.10, 0.50, [255, 200, 120], 0.55),
            (0.90, 0.95, 0.55, [76, 29, 149], 0.45),
        ],
    },
    Preset {
        name: "Ocean",
        angle: 160.0,
        stops: &[(0.0, [8, 47, 73]), (0.6, [3, 105, 161]), (1.0, [6, 182, 212])],
        glows: &[
            (0.80, 0.18, 0.55, [125, 211, 252], 0.45),
            (0.10, 0.90, 0.50, [15, 23, 42], 0.55),
        ],
    },
    Preset {
        name: "Midnight",
        angle: 110.0,
        stops: &[(0.0, [9, 9, 15]), (1.0, [24, 24, 40])],
        glows: &[
            (0.50, -0.05, 0.75, [99, 102, 241], 0.45),
            (0.95, 1.00, 0.45, [139, 92, 246], 0.25),
        ],
    },
    Preset {
        name: "Peach",
        angle: 140.0,
        stops: &[(0.0, [255, 228, 214]), (0.5, [253, 205, 225]), (1.0, [214, 204, 255])],
        glows: &[
            (0.15, 0.85, 0.50, [255, 186, 160], 0.50),
            (0.85, 0.15, 0.50, [196, 214, 255], 0.50),
        ],
    },
    Preset {
        name: "Forest",
        angle: 150.0,
        stops: &[(0.0, [6, 78, 59]), (0.55, [4, 120, 87]), (1.0, [20, 83, 45])],
        glows: &[
            (0.85, 0.20, 0.55, [110, 231, 183], 0.40),
            (0.10, 0.95, 0.50, [2, 44, 34], 0.55),
        ],
    },
    Preset {
        name: "Graphite",
        angle: 90.0,
        stops: &[(0.0, [64, 64, 70]), (1.0, [23, 23, 27])],
        glows: &[(0.5, 0.0, 0.70, [120, 120, 132], 0.35)],
    },
];

/// Where the recording sits on the canvas, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub radius: usize,
}

impl ContentRect {
    pub fn full(w: usize, h: usize) -> Self {
        Self { x: 0, y: 0, w, h, radius: 0 }
    }

    /// Insets the recording by `padding` on every side, keeping its aspect
    /// ratio, with sizes kept even so chroma planes stay aligned.
    pub fn framed(canvas_w: usize, canvas_h: usize, padding: f32, radius: f32) -> Self {
        let scale = (1.0 - 2.0 * padding.clamp(0.0, 0.2)) as f64;
        let w = (((canvas_w as f64 * scale) / 2.0).round() as usize * 2).clamp(2, canvas_w & !1);
        let h = (((canvas_h as f64 * scale) / 2.0).round() as usize * 2).clamp(2, canvas_h & !1);
        // Even offsets keep the 4:2:0 chroma grid aligned with the content.
        let x = ((canvas_w - w) / 2) & !1;
        let y = ((canvas_h - h) / 2) & !1;
        let radius = ((w.min(h) as f32 * radius.clamp(0.0, 0.08)).round() as usize).min(w.min(h) / 2);
        Self { x, y, w, h, radius }
    }

    /// Fits a `src_w` x `src_h` recording, keeping its shape, into a canvas
    /// of another shape, inset by `padding` on every side.
    pub fn fit(canvas_w: usize, canvas_h: usize, src_w: usize, src_h: usize, padding: f32, radius: f32) -> Self {
        let inset = (1.0 - 2.0 * padding.clamp(0.0, 0.2)) as f64;
        let scale = (canvas_w as f64 * inset / src_w.max(1) as f64).min(canvas_h as f64 * inset / src_h.max(1) as f64);
        let even = |v: f64, max: usize| (((v / 2.0).round() as usize) * 2).clamp(2, max & !1);
        let w = even(src_w as f64 * scale, canvas_w);
        let h = even(src_h as f64 * scale, canvas_h);
        let x = ((canvas_w - w) / 2) & !1;
        let y = ((canvas_h - h) / 2) & !1;
        let radius = ((w.min(h) as f32 * radius.clamp(0.0, 0.08)).round() as usize).min(w.min(h) / 2);
        Self { x, y, w, h, radius }
    }
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92 * 255.0
    } else {
        (1.055 * c.powf(1.0 / 2.4) - 0.055) * 255.0
    }
}

/// Cheap per-pixel hash in [0, 1) used to dither gradients so the encoder
/// does not turn them into visible bands.
fn hash01(x: u32, y: u32, c: u32) -> f32 {
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ c.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65536.0
}

fn lin(c: [u8; 3]) -> [f32; 3] {
    [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2])]
}

/// Linear-light color of `preset` at normalized canvas position `(u, v)`;
/// `aspect` is height / width.
fn preset_color(preset: &Preset, stops: &[(f32, [f32; 3])], glows: &[(f32, f32, f32, [f32; 3], f32)], u: f32, v: f32, aspect: f32) -> [f32; 3] {
    let (s, c) = preset.angle.to_radians().sin_cos();
    // Project onto the gradient axis and normalize to 0..1 across the canvas.
    let extent = c.abs() + s.abs() * aspect;
    let t = (((u - 0.5) * c + (v - 0.5) * aspect * s) / extent.max(1e-3) + 0.5).clamp(0.0, 1.0);
    let mut col = stops[stops.len() - 1].1;
    for pair in stops.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t <= t1 {
            let k = ((t - t0) / (t1 - t0).max(1e-4)).clamp(0.0, 1.0);
            let k = k * k * (3.0 - 2.0 * k);
            col = [c0[0] + (c1[0] - c0[0]) * k, c0[1] + (c1[1] - c0[1]) * k, c0[2] + (c1[2] - c0[2]) * k];
            break;
        }
    }
    for &(gx, gy, r, gc, strength) in glows {
        let dx = u - gx;
        let dy = (v - gy) * aspect;
        let k = (-(dx * dx + dy * dy) / (r * r * 0.5)).exp() * strength;
        for i in 0..3 {
            col[i] += (gc[i] - col[i]) * k;
        }
    }
    col
}

fn render_preset(preset: &Preset, w: usize, h: usize, out: &mut [u8]) {
    use rayon::prelude::*;
    let stops: Vec<(f32, [f32; 3])> = preset.stops.iter().map(|&(t, c)| (t, lin(c))).collect();
    let glows: Vec<(f32, f32, f32, [f32; 3], f32)> =
        preset.glows.iter().map(|&(x, y, r, c, s)| (x, y, r, lin(c), s)).collect();
    let aspect = h as f32 / w.max(1) as f32;
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let v = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let col = preset_color(preset, &stops, &glows, u, v, aspect);
            let px = &mut row[x * 4..x * 4 + 4];
            for c in 0..3 {
                let dither = hash01(x as u32, y as u32, c as u32) - 0.5;
                px[c] = (linear_to_srgb(col[c]) + dither).round().clamp(0.0, 255.0) as u8;
            }
            px[3] = 255;
        }
    });
}

/// Scales `img` to cover the canvas, cropping the overflow from the center.
fn render_image(path: &std::path::Path, w: usize, h: usize, out: &mut [u8]) -> Result<()> {
    let img = image::open(path)
        .with_context(|| format!("cannot open background image {}", path.display()))?
        .to_rgb8();
    let (iw, ih) = (img.width() as f64, img.height() as f64);
    let scale = (w as f64 / iw).max(h as f64 / ih);
    let crop_w = ((w as f64 / scale).round() as u32).clamp(1, img.width());
    let crop_h = ((h as f64 / scale).round() as u32).clamp(1, img.height());
    let cx = (img.width() - crop_w) / 2;
    let cy = (img.height() - crop_h) / 2;
    let cropped = image::imageops::crop_imm(&img, cx, cy, crop_w, crop_h).to_image();
    let resized =
        image::imageops::resize(&cropped, w as u32, h as u32, image::imageops::FilterType::CatmullRom);
    for (dst, src) in out.chunks_exact_mut(4).zip(resized.pixels()) {
        dst[..3].copy_from_slice(&src.0);
        dst[3] = 255;
    }
    Ok(())
}

/// Abramowitz-Stegun 7.1.26 approximation, accurate to ~1.5e-7.
fn erf(x: f32) -> f32 {
    let sign = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_7) * t - 0.284_496_74) * t + 0.254_829_6)
            * t
            * (-x * x).exp();
    sign * y
}

/// Coverage of a Gaussian-blurred box along one axis.
fn blurred_span(p: f32, lo: f32, hi: f32, sigma: f32) -> f32 {
    let k = std::f32::consts::FRAC_1_SQRT_2 / sigma.max(0.5);
    0.5 * (erf((p - lo) * k) - erf((p - hi) * k))
}

fn apply_shadow(rect: &ContentRect, w: usize, out: &mut [u8]) {
    use rayon::prelude::*;
    let short = rect.w.min(rect.h) as f32;
    let sigma = short * 0.035;
    let offset_y = short * 0.018;
    let opacity = 0.55;
    let (x0, x1) = (rect.x as f32, (rect.x + rect.w) as f32);
    let (y0, y1) = (rect.y as f32 + offset_y, (rect.y + rect.h) as f32 + offset_y);
    let xs: Vec<f32> = (0..w).map(|x| blurred_span(x as f32 + 0.5, x0, x1, sigma)).collect();
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let fy = blurred_span(y as f32 + 0.5, y0, y1, sigma);
        if fy < 1.0 / 512.0 {
            return;
        }
        for (x, fx) in xs.iter().enumerate() {
            let a = opacity * fx * fy;
            if a < 1.0 / 512.0 {
                continue;
            }
            let px = &mut row[x * 4..x * 4 + 3];
            for c in px {
                *c = (*c as f32 * (1.0 - a)).round() as u8;
            }
        }
    });
}

/// Renders the full canvas (RGB0) behind the recording.
pub fn render_canvas(settings: &BackgroundSettings, w: usize, h: usize, rect: &ContentRect) -> Result<Vec<u8>> {
    let mut out = vec![0u8; w * h * 4];
    match &settings.kind {
        BackgroundKind::None => {}
        BackgroundKind::Preset(i) => render_preset(&PRESETS[(*i).min(PRESETS.len() - 1)], w, h, &mut out),
        BackgroundKind::Image(path) => {
            if let Err(err) = render_image(path, w, h, &mut out) {
                eprintln!("[HyperGSR] {err:#}; using the default background");
                render_preset(&PRESETS[0], w, h, &mut out);
            }
        }
    }
    if settings.shadow && settings.is_enabled() {
        apply_shadow(rect, w, &mut out);
    }
    Ok(out)
}

/// Anti-aliased coverage (0..=255) of one rounded corner of `radius` pixels,
/// as a `size` x `size` table indexed `[dy * size + dx]`, with (0, 0) at the
/// outermost corner pixel.
pub fn corner_mask(size: usize, radius: f32) -> Vec<u8> {
    let r = radius;
    let mut mask = vec![255u8; size * size];
    for dy in 0..size {
        for dx in 0..size {
            // 4x4 supersampling of the quarter circle centered at (r, r).
            let mut hits = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = dx as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = dy as f32 + (sy as f32 + 0.5) / 4.0;
                    let (ddx, ddy) = (r - px, r - py);
                    if ddx * ddx + ddy * ddy <= r * r {
                        hits += 1;
                    }
                }
            }
            mask[dy * size + dx] = ((hits * 255 + 8) / 16) as u8;
        }
    }
    mask
}

/// Approximate sRGB colors of a preset for UI swatches: `(top-left, bottom-right)`.
pub fn swatch_colors(preset: &Preset) -> ([f64; 3], [f64; 3]) {
    let first = preset.stops[0].1;
    let last = preset.stops[preset.stops.len() - 1].1;
    let f = |c: [u8; 3]| [c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0];
    (f(first), f(last))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_recording_fits_a_vertical_canvas() {
        let r = ContentRect::fit(1080, 1920, 1920, 1080, 0.0, 0.0);
        assert_eq!((r.x, r.w), (0, 1080));
        assert_eq!(r.h, 608);
        assert_eq!(r.y, (1920 - 608) / 2);
        let padded = ContentRect::fit(1080, 1080, 1920, 1080, 0.05, 0.02);
        assert!(padded.w <= 972 && padded.x >= 54 && padded.x % 2 == 0 && padded.y % 2 == 0);
        // Same shape: matches the plain inset.
        assert_eq!(ContentRect::fit(1920, 1080, 1920, 1080, 0.06, 0.0), ContentRect::framed(1920, 1080, 0.06, 0.0));
    }

    #[test]
    fn framed_rect_is_centered_even_and_keeps_aspect() {
        let r = ContentRect::framed(1920, 1080, 0.06, 0.02);
        assert_eq!(r.w % 2, 0);
        assert_eq!(r.h % 2, 0);
        assert_eq!(r.x % 2, 0);
        assert_eq!(r.y % 2, 0);
        assert!((r.x * 2 + r.w).abs_diff(1920) <= 2);
        assert!((r.y * 2 + r.h).abs_diff(1080) <= 2);
        let aspect = r.w as f32 / r.h as f32;
        assert!((aspect - 16.0 / 9.0).abs() < 0.01);
        assert!(r.radius > 0 && r.radius < 40);
    }

    #[test]
    fn corner_mask_is_opaque_inside_and_clear_at_the_tip() {
        let m = corner_mask(16, 16.0);
        assert_eq!(m[0], 0);
        assert_eq!(m[15 * 16 + 15], 255);
        assert!(m[8 * 16 + 1] < m[8 * 16 + 8]);
    }

    #[test]
    fn presets_render_opaque_dithered_pixels() {
        let settings = BackgroundSettings {
            kind: BackgroundKind::Preset(0),
            ..Default::default()
        };
        let rect = ContentRect::framed(64, 36, 0.1, 0.02);
        let canvas = render_canvas(&settings, 64, 36, &rect).unwrap();
        assert_eq!(canvas.len(), 64 * 36 * 4);
        assert!(canvas.chunks(4).all(|p| p[3] == 255));
        assert!(canvas.chunks(4).any(|p| p[0] != canvas[0] || p[2] != canvas[2]));
    }

    #[test]
    fn erf_matches_reference_values() {
        assert!(erf(0.0).abs() < 1e-6);
        assert!((erf(1.0) - 0.842_700_8).abs() < 1e-5);
        assert!((erf(-2.0) + 0.995_322_3).abs() < 1e-5);
    }
}
