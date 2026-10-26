use serde::{Deserialize, Serialize};
use std::ptr;
use x11_dl::xlib::{Xlib, ZPixmap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureSourceMode {
    AutoHybrid,
    X11Display,
    StudioWorkspace,
}

impl CaptureSourceMode {
    pub fn label(&self) -> &'static str {
        match self {
            CaptureSourceMode::AutoHybrid => "Auto (X11/Wayland + Studio Fallback)",
            CaptureSourceMode::X11Display => "X11 / XWayland Root Display (:1)",
            CaptureSourceMode::StudioWorkspace => "Interactive Studio Desktop Canvas",
        }
    }
}

/// Pre-allocated RGB24 frame buffer to avoid per-frame heap allocations.
#[derive(Clone)]
pub struct FrameBuffer {
    pub width: usize,
    pub height: usize,
    /// Packed RGB24 bytes (`width * height * 3`)
    pub data: Vec<u8>,
}

impl FrameBuffer {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0u8; width * height * 3],
        }
    }

    pub fn resize_if_needed(&mut self, width: usize, height: usize) {
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.data.resize(width * height * 3, 0);
        }
    }

    #[inline(always)]
    pub fn sample_rgb(&self, u: f32, v: f32) -> (u8, u8, u8) {
        let x = ((u.clamp(0.0, 0.9999) * self.width as f32) as usize).min(self.width - 1);
        let y = ((v.clamp(0.0, 0.9999) * self.height as f32) as usize).min(self.height - 1);
        let idx = (y * self.width + x) * 3;
        (self.data[idx], self.data[idx + 1], self.data[idx + 2])
    }

    #[inline(always)]
    pub fn set_rgb(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        if x < self.width && y < self.height {
            let idx = (y * self.width + x) * 3;
            self.data[idx] = r;
            self.data[idx + 1] = g;
            self.data[idx + 2] = b;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill_rect(
        &mut self,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
        r: u8,
        g: u8,
        b: u8,
    ) {
        let x_end = (x0 + w).min(self.width);
        let y_end = (y0 + h).min(self.height);
        for y in y0.min(self.height)..y_end {
            let row_start = y * self.width * 3;
            for x in x0.min(self.width)..x_end {
                let idx = row_start + x * 3;
                self.data[idx] = r;
                self.data[idx + 1] = g;
                self.data[idx + 2] = b;
            }
        }
    }
}

pub struct ScreenCapturer {
    x11: Option<X11ScreenCapturer>,
    cached_workspace: FrameBuffer,
    workspace_initialized: bool,
}

struct X11ScreenCapturer {
    xlib: Xlib,
    display: *mut x11_dl::xlib::Display,
    root: x11_dl::xlib::Window,
    src_w: usize,
    src_h: usize,
}

unsafe impl Send for X11ScreenCapturer {}

impl X11ScreenCapturer {
    fn try_open() -> Option<Self> {
        unsafe {
            let xlib = Xlib::open().ok()?;
            let display = (xlib.XOpenDisplay)(ptr::null());
            if display.is_null() {
                return None;
            }
            let screen = (xlib.XDefaultScreen)(display);
            let root = (xlib.XRootWindow)(display, screen);
            let src_w = (xlib.XDisplayWidth)(display, screen).max(1) as usize;
            let src_h = (xlib.XDisplayHeight)(display, screen).max(1) as usize;
            Some(Self {
                xlib,
                display,
                root,
                src_w,
                src_h,
            })
        }
    }

    fn capture_into(&self, dst: &mut FrameBuffer) -> bool {
        unsafe {
            let img = (self.xlib.XGetImage)(
                self.display,
                self.root,
                0,
                0,
                self.src_w as u32,
                self.src_h as u32,
                !0,
                ZPixmap,
            );
            if img.is_null() {
                return false;
            }
            let img_ref = &*img;
            if img_ref.data.is_null() || img_ref.bits_per_pixel < 24 {
                (self.xlib.XDestroyImage)(img);
                return false;
            }

            let bpp = (img_ref.bits_per_pixel / 8) as usize;
            let stride = img_ref.bytes_per_line as usize;
            let raw = std::slice::from_raw_parts(
                img_ref.data as *const u8,
                stride * self.src_h,
            );

            // Fast O(1) 16-point probe to check if XWayland root window is blank/all-black
            let mut non_zero_samples = 0usize;
            for i in 1..=16 {
                let sx = (self.src_w * i / 17).min(self.src_w - 1);
                let sy = (self.src_h * i / 17).min(self.src_h - 1);
                let off = sy * stride + sx * bpp;
                if off + 2 < raw.len() && (raw[off] | raw[off + 1] | raw[off + 2]) > 8 {
                    non_zero_samples += 1;
                }
            }

            if non_zero_samples == 0 {
                (self.xlib.XDestroyImage)(img);
                return false;
            }

            let dst_w = dst.width;
            let dst_h = dst.height;
            for y in 0..dst_h {
                let sy = y * self.src_h / dst_h;
                let src_row = sy * stride;
                let dst_row = y * dst_w * 3;
                for x in 0..dst_w {
                    let sx = x * self.src_w / dst_w;
                    let s_idx = src_row + sx * bpp;
                    let d_idx = dst_row + x * 3;
                    dst.data[d_idx] = raw[s_idx + 2];
                    dst.data[d_idx + 1] = raw[s_idx + 1];
                    dst.data[d_idx + 2] = raw[s_idx];
                }
            }

            (self.xlib.XDestroyImage)(img);
            true
        }
    }
}

impl Drop for X11ScreenCapturer {
    fn drop(&mut self) {
        unsafe {
            if !self.display.is_null() {
                (self.xlib.XCloseDisplay)(self.display);
            }
        }
    }
}

impl ScreenCapturer {
    pub fn new(_width: usize, _height: usize) -> Self {
        Self {
            x11: X11ScreenCapturer::try_open(),
            cached_workspace: FrameBuffer::new(0, 0),
            workspace_initialized: false,
        }
    }

    /// Pre-renders the static base of the developer desktop once so per-frame updates take <0.2ms.
    fn bake_studio_workspace(&mut self, width: usize, height: usize) {
        self.cached_workspace.resize_if_needed(width, height);
        let buf = &mut self.cached_workspace;

        // 1. Dark desktop wallpaper with subtle grid
        for y in 0..height {
            let v = y as f32 / height as f32;
            let r = (14.0 + v * 10.0) as u8;
            let g = (18.0 + v * 14.0) as u8;
            let b = (30.0 + v * 22.0) as u8;
            let row = y * width * 3;
            let is_grid_y = y % 40 == 0;
            for x in 0..width {
                let idx = row + x * 3;
                if is_grid_y || x % 40 == 0 {
                    buf.data[idx] = r.saturating_add(7);
                    buf.data[idx + 1] = g.saturating_add(9);
                    buf.data[idx + 2] = b.saturating_add(14);
                } else {
                    buf.data[idx] = r;
                    buf.data[idx + 1] = g;
                    buf.data[idx + 2] = b;
                }
            }
        }

        // Top system status bar
        buf.fill_rect(0, 0, width, height / 28, 10, 13, 22);

        // 2. Left window: Rust + GLSL Code Editor (IDE mockup)
        let ed_x = width * 4 / 100;
        let ed_y = height * 7 / 100;
        let ed_w = width * 54 / 100;
        let ed_h = height * 62 / 100;
        buf.fill_rect(ed_x, ed_y, ed_w, ed_h, 20, 24, 36);
        buf.fill_rect(ed_x, ed_y, ed_w, height / 26, 28, 34, 50);
        // Window traffic lights
        buf.fill_rect(ed_x + 12, ed_y + 10, 10, 10, 255, 95, 86);
        buf.fill_rect(ed_x + 28, ed_y + 10, 10, 10, 255, 189, 46);
        buf.fill_rect(ed_x + 44, ed_y + 10, 10, 10, 39, 201, 63);

        // Simulated syntax-highlighted code lines inside the editor
        let line_colors: [(u8, u8, u8, usize, usize); 14] = [
            (198, 120, 221, 0, 28),
            (97, 175, 239, 16, 48),
            (152, 195, 121, 32, 62),
            (229, 192, 123, 32, 54),
            (86, 182, 194, 48, 72),
            (224, 108, 117, 48, 42),
            (97, 175, 239, 32, 66),
            (198, 120, 221, 16, 38),
            (152, 195, 121, 0, 52),
            (229, 192, 123, 24, 70),
            (86, 182, 194, 40, 58),
            (97, 175, 239, 40, 46),
            (198, 120, 221, 24, 34),
            (152, 195, 121, 0, 22),
        ];
        for (i, &(cr, cg, cb, indent, len_pct)) in line_colors.iter().enumerate() {
            let ly = ed_y + height / 18 + i * (ed_h / 18);
            // Gutter number bar
            buf.fill_rect(ed_x + 12, ly, 14, 6, 75, 85, 110);
            // Code token bar
            let lx = ed_x + 42 + indent;
            let lw = (ed_w * len_pct / 100).min(ed_w.saturating_sub(indent + 56));
            buf.fill_rect(lx, ly, lw, 7, cr, cg, cb);
        }

        // 3. Right top window: GPU Encoder & NVENC/VAAPI Telemetry Panel
        let gp_x = width * 61 / 100;
        let gp_y = height * 7 / 100;
        let gp_w = width * 35 / 100;
        let gp_h = height * 40 / 100;
        buf.fill_rect(gp_x, gp_y, gp_w, gp_h, 19, 25, 38);
        buf.fill_rect(gp_x, gp_y, gp_w, height / 26, 27, 36, 54);
        // Metric cards
        buf.fill_rect(gp_x + 16, gp_y + 42, gp_w - 32, gp_h / 4, 28, 38, 58);
        buf.fill_rect(gp_x + 24, gp_y + 54, (gp_w - 48) * 78 / 100, 10, 56, 189, 248);
        buf.fill_rect(gp_x + 16, gp_y + 48 + gp_h / 4, gp_w - 32, gp_h / 4, 28, 38, 58);
        buf.fill_rect(gp_x + 24, gp_y + 60 + gp_h / 4, (gp_w - 48) * 92 / 100, 10, 52, 211, 153);

        // 4. Bottom window: Zoom Keyframe Timeline & Audio Waveform Deck
        let tl_x = width * 4 / 100;
        let tl_y = height * 72 / 100;
        let tl_w = width * 92 / 100;
        let tl_h = height * 22 / 100;
        buf.fill_rect(tl_x, tl_y, tl_w, tl_h, 18, 22, 34);
        buf.fill_rect(tl_x, tl_y, tl_w, height / 32, 26, 32, 48);
        // Keyframe blocks on the timeline
        buf.fill_rect(tl_x + tl_w * 8 / 100, tl_y + tl_h * 35 / 100, tl_w * 18 / 100, tl_h / 3, 99, 102, 241);
        buf.fill_rect(tl_x + tl_w * 36 / 100, tl_y + tl_h * 35 / 100, tl_w * 22 / 100, tl_h / 3, 14, 165, 233);
        buf.fill_rect(tl_x + tl_w * 66 / 100, tl_y + tl_h * 35 / 100, tl_w * 19 / 100, tl_h / 3, 16, 185, 129);

        self.workspace_initialized = true;
    }

    pub fn capture_frame(&mut self, mode: CaptureSourceMode, now_ms: u64, dst: &mut FrameBuffer) {
        if !self.workspace_initialized
            || self.cached_workspace.width != dst.width
            || self.cached_workspace.height != dst.height
        {
            self.bake_studio_workspace(dst.width, dst.height);
        }

        match mode {
            CaptureSourceMode::X11Display => {
                if let Some(ref x11) = self.x11 {
                    if x11.capture_into(dst) {
                        return;
                    }
                }
            }
            CaptureSourceMode::AutoHybrid => {
                if let Some(ref x11) = self.x11 {
                    if x11.capture_into(dst) {
                        return;
                    }
                }
            }
            CaptureSourceMode::StudioWorkspace => {}
        }

        // Fast memcpy from pre-baked workspace + animated playhead bar
        dst.data.copy_from_slice(&self.cached_workspace.data);
        let w = dst.width;
        let h = dst.height;
        let tl_x = w * 4 / 100;
        let tl_y = h * 72 / 100;
        let tl_w = w * 92 / 100;
        let tl_h = h * 22 / 100;
        let playhead_x = tl_x + ((now_ms % 12000) as usize * tl_w / 12000);
        dst.fill_rect(playhead_x, tl_y + 4, 3, tl_h.saturating_sub(8), 250, 204, 21);
    }
}
