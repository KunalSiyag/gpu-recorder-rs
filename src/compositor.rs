use crate::capture::FrameBuffer;
use crate::cursor::{ClickRipple, CursorConfig, CursorSample};
use crate::zoom_engine::CameraViewport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackdropTheme {
    MidnightAurora,
    CyberOcean,
    SunsetEmber,
    PureObsidian,
}

impl BackdropTheme {
    pub fn label(&self) -> &'static str {
        match self {
            BackdropTheme::MidnightAurora => "Midnight Aurora (Indigo / Violet)",
            BackdropTheme::CyberOcean => "Cyber Ocean (Cyan / Deep Blue)",
            BackdropTheme::SunsetEmber => "Sunset Ember (Coral / Magenta)",
            BackdropTheme::PureObsidian => "Pure Obsidian (Minimal Dark)",
        }
    }

    pub fn colors(&self) -> ((u8, u8, u8), (u8, u8, u8)) {
        match self {
            BackdropTheme::MidnightAurora => ((24, 28, 68), (76, 36, 118)),
            BackdropTheme::CyberOcean => ((12, 52, 88), (22, 108, 145)),
            BackdropTheme::SunsetEmber => ((88, 28, 65), (165, 58, 48)),
            BackdropTheme::PureObsidian => ((12, 14, 18), (24, 28, 36)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameStyleConfig {
    pub backdrop: BackdropTheme,
    /// Normalized padding in [0.0, 0.14] around the desktop frame
    pub padding: f32,
    /// Corner radius in pixels
    pub corner_radius: f32,
    pub drop_shadow: bool,
}

impl Default for FrameStyleConfig {
    fn default() -> Self {
        Self {
            backdrop: BackdropTheme::MidnightAurora,
            padding: 0.05,
            corner_radius: 16.0,
            drop_shadow: true,
        }
    }
}

pub struct FrameCompositor;

impl FrameCompositor {
    /// Single-pass frame composition with camera zoom, backdrop bezel, spotlight, trail, ripples, and cursor.
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        src: &FrameBuffer,
        viewport: &CameraViewport,
        cursor: &CursorSample,
        trail: &[CursorSample],
        ripples: &[ClickRipple],
        cursor_cfg: &CursorConfig,
        style_cfg: &FrameStyleConfig,
        now_ms: u64,
        dst: &mut FrameBuffer,
    ) {
        let w = dst.width;
        let h = dst.height;
        let pad_x = (w as f32 * style_cfg.padding.clamp(0.0, 0.15)) as usize;
        let pad_y = (h as f32 * style_cfg.padding.clamp(0.0, 0.15)) as usize;
        let inner_w = w.saturating_sub(pad_x * 2).max(1);
        let inner_h = h.saturating_sub(pad_y * 2).max(1);

        let ((r0, g0, b0), (r1, g1, b1)) = style_cfg.backdrop.colors();
        let radius = style_cfg.corner_radius.max(0.0) as isize;

        // Precompute spotlight cursor position in screen UV space
        let spot_enabled = cursor_cfg.spotlight_enabled;
        let spot_dim = (1.0 - cursor_cfg.spotlight_dim_opacity.clamp(0.0, 0.85)).clamp(0.15, 1.0);
        let spot_r2 = 0.018f32; // squared radius in aspect-corrected UV

        for y in 0..h {
            let row_offset = y * w * 3;
            let in_y = y >= pad_y && y < pad_y + inner_h;
            let v_inner = if in_y {
                (y - pad_y) as f32 / inner_h as f32
            } else {
                0.0
            };
            let sample_v = viewport.uv_min_y + v_inner * viewport.uv_h;
            let bg_t = (y * 256 / h.max(1)) as u16;
            let bg_r = ((r0 as u16 * (256 - bg_t) + r1 as u16 * bg_t) >> 8) as u8;
            let bg_g = ((g0 as u16 * (256 - bg_t) + g1 as u16 * bg_t) >> 8) as u8;
            let bg_b = ((b0 as u16 * (256 - bg_t) + b1 as u16 * bg_t) >> 8) as u8;

            for x in 0..w {
                let idx = row_offset + x * 3;
                let in_x = x >= pad_x && x < pad_x + inner_w;

                if in_x && in_y {
                    // Rounded corner cutout check only near corners (fast O(1) branch)
                    let lx = (x - pad_x) as isize;
                    let ly = (y - pad_y) as isize;
                    let rx = (inner_w as isize - 1) - lx;
                    let ry = (inner_h as isize - 1) - ly;
                    let cx = lx.min(rx);
                    let cy = ly.min(ry);
                    if cx < radius && cy < radius {
                        let dx = radius - cx;
                        let dy = radius - cy;
                        if dx * dx + dy * dy > radius * radius {
                            dst.data[idx] = bg_r;
                            dst.data[idx + 1] = bg_g;
                            dst.data[idx + 2] = bg_b;
                            continue;
                        }
                    }

                    let u_inner = (x - pad_x) as f32 / inner_w as f32;
                    let sample_u = viewport.uv_min_x + u_inner * viewport.uv_w;
                    let (mut r, mut g, mut b) = src.sample_rgb(sample_u, sample_v);

                    if spot_enabled {
                        let du = (sample_u - cursor.smooth_x) * 1.777;
                        let dv = sample_v - cursor.smooth_y;
                        let d2 = du * du + dv * dv;
                        if d2 > spot_r2 {
                            r = (r as f32 * spot_dim) as u8;
                            g = (g as f32 * spot_dim) as u8;
                            b = (b as f32 * spot_dim) as u8;
                        }
                    }

                    dst.data[idx] = r;
                    dst.data[idx + 1] = g;
                    dst.data[idx + 2] = b;
                } else {
                    dst.data[idx] = bg_r;
                    dst.data[idx + 1] = bg_g;
                    dst.data[idx + 2] = bg_b;
                }
            }
        }

        // Helper closure: map screen UV (u, v) -> output canvas pixel (px, py)
        let uv_to_canvas = |u: f32, v: f32| -> Option<(isize, isize)> {
            let rel_u = (u - viewport.uv_min_x) / viewport.uv_w;
            let rel_v = (v - viewport.uv_min_y) / viewport.uv_h;
            if (-0.02..=1.02).contains(&rel_u) && (-0.02..=1.02).contains(&rel_v) {
                let px = pad_x as isize + (rel_u * inner_w as f32) as isize;
                let py = pad_y as isize + (rel_v * inner_h as f32) as isize;
                Some((px, py))
            } else {
                None
            }
        };

        // 2. Draw cursor motion trail
        if cursor_cfg.motion_trail {
            for (i, sample) in trail.iter().enumerate().skip(1) {
                if let Some((tx, ty)) = uv_to_canvas(sample.smooth_x, sample.smooth_y) {
                    let frac = 1.0 - (i as f32 / trail.len().max(1) as f32);
                    let rad = ((4.0 * frac * cursor_cfg.cursor_scale) as isize).max(1);
                    Self::draw_blend_circle(dst, tx, ty, rad, (56, 189, 248), frac * 0.45);
                }
            }
        }

        // 3. Draw expanding click ripple rings
        if cursor_cfg.click_ripples {
            for ripple in ripples {
                let elapsed = now_ms.saturating_sub(ripple.start_ms) as f32;
                let progress = (elapsed / 650.0).clamp(0.0, 1.0);
                if let Some((rx, ry)) = uv_to_canvas(ripple.x, ripple.y) {
                    let max_r = (inner_h as f32 * 0.075 * viewport.zoom.sqrt()) as isize;
                    let ring_r = (6 + (max_r as f32 * progress) as isize).max(6);
                    let color = if ripple.is_right_click {
                        (244, 114, 182)
                    } else {
                        (56, 189, 248)
                    };
                    Self::draw_ring(dst, rx, ry, ring_r, 3, color, (1.0 - progress) * 0.9);
                }
            }
        }

        // 4. Draw smoothed high-contrast cursor pointer + halo
        if let Some((cx, cy)) = uv_to_canvas(cursor.smooth_x, cursor.smooth_y) {
            let c_rad = ((7.0 * cursor_cfg.cursor_scale) as isize).clamp(4, 22);
            Self::draw_blend_circle(dst, cx, cy, c_rad + 5, (56, 189, 248), 0.35);
            Self::draw_blend_circle(dst, cx, cy, c_rad + 2, (255, 255, 255), 0.95);
            let inner_col = if cursor.left_down {
                (16, 185, 129)
            } else if cursor.right_down {
                (244, 114, 182)
            } else {
                (14, 165, 233)
            };
            Self::draw_blend_circle(dst, cx, cy, c_rad, inner_col, 1.0);
        }
    }

    fn draw_blend_circle(
        dst: &mut FrameBuffer,
        cx: isize,
        cy: isize,
        radius: isize,
        color: (u8, u8, u8),
        alpha: f32,
    ) {
        let a = alpha.clamp(0.0, 1.0);
        let inv_a = 1.0 - a;
        let r2 = radius * radius;
        let y_min = (cy - radius).max(0) as usize;
        let y_max = (cy + radius).min(dst.height as isize - 1).max(0) as usize;
        let x_min = (cx - radius).max(0) as usize;
        let x_max = (cx + radius).min(dst.width as isize - 1).max(0) as usize;

        for y in y_min..=y_max {
            let dy = y as isize - cy;
            for x in x_min..=x_max {
                let dx = x as isize - cx;
                if dx * dx + dy * dy <= r2 {
                    let idx = (y * dst.width + x) * 3;
                    dst.data[idx] = (dst.data[idx] as f32 * inv_a + color.0 as f32 * a) as u8;
                    dst.data[idx + 1] =
                        (dst.data[idx + 1] as f32 * inv_a + color.1 as f32 * a) as u8;
                    dst.data[idx + 2] =
                        (dst.data[idx + 2] as f32 * inv_a + color.2 as f32 * a) as u8;
                }
            }
        }
    }

    fn draw_ring(
        dst: &mut FrameBuffer,
        cx: isize,
        cy: isize,
        radius: isize,
        thickness: isize,
        color: (u8, u8, u8),
        alpha: f32,
    ) {
        let a = alpha.clamp(0.0, 1.0);
        let inv_a = 1.0 - a;
        let r_outer2 = (radius + thickness) * (radius + thickness);
        let r_inner2 = (radius - thickness).max(0) * (radius - thickness).max(0);
        let y_min = (cy - radius - thickness).max(0) as usize;
        let y_max = (cy + radius + thickness)
            .min(dst.height as isize - 1)
            .max(0) as usize;
        let x_min = (cx - radius - thickness).max(0) as usize;
        let x_max = (cx + radius + thickness)
            .min(dst.width as isize - 1)
            .max(0) as usize;

        for y in y_min..=y_max {
            let dy = y as isize - cy;
            for x in x_min..=x_max {
                let dx = x as isize - cx;
                let d2 = dx * dx + dy * dy;
                if d2 >= r_inner2 && d2 <= r_outer2 {
                    let idx = (y * dst.width + x) * 3;
                    dst.data[idx] = (dst.data[idx] as f32 * inv_a + color.0 as f32 * a) as u8;
                    dst.data[idx + 1] =
                        (dst.data[idx + 1] as f32 * inv_a + color.1 as f32 * a) as u8;
                    dst.data[idx + 2] =
                        (dst.data[idx + 2] as f32 * inv_a + color.2 as f32 * a) as u8;
                }
            }
        }
    }
}
