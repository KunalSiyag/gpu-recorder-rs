use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::ptr;
use x11_dl::xlib::Xlib;

/// Fixed-capacity O(1) ring buffer to prevent unbounded allocations or long iterations.
pub const CURSOR_HISTORY_CAP: usize = 256;
pub const CLICK_RIPPLE_CAP: usize = 16;
pub const KEYSTROKE_HUD_CAP: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct CursorSample {
    pub timestamp_ms: u64,
    /// Normalized X in [0.0, 1.0]
    pub raw_x: f32,
    /// Normalized Y in [0.0, 1.0]
    pub raw_y: f32,
    /// Spring-smoothed X in [0.0, 1.0]
    pub smooth_x: f32,
    /// Spring-smoothed Y in [0.0, 1.0]
    pub smooth_y: f32,
    /// Normalized velocity magnitude (screen widths per second)
    pub speed: f32,
    pub left_down: bool,
    pub right_down: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClickRipple {
    pub x: f32,
    pub y: f32,
    pub start_ms: u64,
    pub is_right_click: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeystrokeOverlay {
    pub label: String,
    pub start_ms: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorConfig {
    pub smoothing_enabled: bool,
    /// Spring stiffness for cursor smoothing (higher = snappier, lower = smoother glide)
    pub smoothing_stiffness: f32,
    pub cursor_scale: f32,
    pub click_ripples: bool,
    pub spotlight_enabled: bool,
    pub spotlight_dim_opacity: f32,
    pub motion_trail: bool,
    pub trail_length: usize,
    pub show_keystrokes: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            smoothing_enabled: true,
            smoothing_stiffness: 165.0,
            cursor_scale: 1.35,
            click_ripples: true,
            spotlight_enabled: false,
            spotlight_dim_opacity: 0.45,
            motion_trail: true,
            trail_length: 14,
            show_keystrokes: true,
        }
    }
}

/// Hardware DRM/KMS Cursor Plane reader (`libdrm.so.2`).
/// Reads `CRTC_X`, `CRTC_Y`, and `FB_ID` directly from the GPU's hardware cursor plane
/// so global cursor tracking works natively on Wayland (COSMIC / Smithay / Sway / Hyprland)
/// across all Wayland and X11 windows with < 3 microseconds overhead.
#[repr(C)]
struct DrmModePlaneRes {
    count_planes: u32,
    planes: *mut u32,
}

#[repr(C)]
struct DrmModeObjectProperties {
    count_props: u32,
    props: *mut u32,
    prop_values: *mut u64,
}

#[repr(C)]
struct DrmModePropertyRes {
    prop_id: u32,
    flags: u32,
    name: [c_char; 32],
    count_values: c_int,
    values: *mut u64,
    count_enums: c_int,
    enums: *mut c_void,
    count_blobs: c_int,
    blob_ids: *mut u32,
}

struct DrmCursorReader {
    fd: c_int,
    plane_ids: Vec<u32>,
    screen_w: f32,
    screen_h: f32,
    prev_fb_id: std::sync::atomic::AtomicU64,
    get_obj_props: unsafe extern "C" fn(c_int, u32, u32) -> *mut DrmModeObjectProperties,
    free_obj_props: unsafe extern "C" fn(*mut DrmModeObjectProperties),
    get_prop: unsafe extern "C" fn(c_int, u32) -> *mut DrmModePropertyRes,
    free_prop: unsafe extern "C" fn(*mut DrmModePropertyRes),
}

unsafe impl Send for DrmCursorReader {}
unsafe impl Sync for DrmCursorReader {}

impl DrmCursorReader {
    fn try_open() -> Option<Self> {
        unsafe {
            let lib_name = CString::new("libdrm.so.2").ok()?;
            let handle = libc::dlopen(lib_name.as_ptr(), libc::RTLD_LAZY);
            if handle.is_null() {
                return None;
            }

            let sym = |name: &str| -> *mut c_void {
                let cs = CString::new(name).unwrap();
                libc::dlsym(handle, cs.as_ptr())
            };

            let set_cap_ptr = sym("drmSetClientCap");
            let get_planes_ptr = sym("drmModeGetPlaneResources");
            let free_planes_ptr = sym("drmModeFreePlaneResources");
            let get_obj_props_ptr = sym("drmModeObjectGetProperties");
            let free_obj_props_ptr = sym("drmModeFreeObjectProperties");
            let get_prop_ptr = sym("drmModeGetProperty");
            let free_prop_ptr = sym("drmModeFreeProperty");

            if set_cap_ptr.is_null()
                || get_planes_ptr.is_null()
                || get_obj_props_ptr.is_null()
                || free_obj_props_ptr.is_null()
                || get_prop_ptr.is_null()
            {
                return None;
            }

            type SetCapFn = unsafe extern "C" fn(c_int, u64, u64) -> c_int;
            type GetPlanesFn = unsafe extern "C" fn(c_int) -> *mut DrmModePlaneRes;
            type FreePlanesFn = unsafe extern "C" fn(*mut DrmModePlaneRes);

            let set_cap: SetCapFn = std::mem::transmute(set_cap_ptr);
            let get_planes: GetPlanesFn = std::mem::transmute(get_planes_ptr);
            let free_planes: FreePlanesFn = std::mem::transmute(free_planes_ptr);
            let get_obj_props: unsafe extern "C" fn(c_int, u32, u32) -> *mut DrmModeObjectProperties =
                std::mem::transmute(get_obj_props_ptr);
            let free_obj_props: unsafe extern "C" fn(*mut DrmModeObjectProperties) =
                std::mem::transmute(free_obj_props_ptr);
            let get_prop: unsafe extern "C" fn(c_int, u32) -> *mut DrmModePropertyRes =
                std::mem::transmute(get_prop_ptr);
            let free_prop: unsafe extern "C" fn(*mut DrmModePropertyRes) =
                std::mem::transmute(free_prop_ptr);

            for card_path in ["/dev/dri/card1", "/dev/dri/card0"] {
                let cpath = CString::new(card_path).ok()?;
                let fd = libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
                if fd < 0 {
                    continue;
                }
                // DRM_CLIENT_CAP_UNIVERSAL_PLANES = 2, DRM_CLIENT_CAP_ATOMIC = 3
                let _ = set_cap(fd, 2, 1);
                let _ = set_cap(fd, 3, 1);

                let pres = get_planes(fd);
                if pres.is_null() {
                    libc::close(fd);
                    continue;
                }

                let mut cursor_planes = Vec::new();
                let count = (*pres).count_planes as usize;
                for i in 0..count {
                    let pid = *(*pres).planes.add(i);
                    let props = get_obj_props(fd, pid, 0xeeeeeeee);
                    if props.is_null() {
                        continue;
                    }
                    let pcount = (*props).count_props as usize;
                    let mut is_cursor = false;
                    for j in 0..pcount {
                        let prop_id = *(*props).props.add(j);
                        let pval = *(*props).prop_values.add(j);
                        let pr = get_prop(fd, prop_id);
                        if !pr.is_null() {
                            let name_bytes = std::slice::from_raw_parts(
                                (*pr).name.as_ptr() as *const u8,
                                32,
                            );
                            if name_bytes.starts_with(b"type\0") && pval == 2 {
                                is_cursor = true;
                            }
                            if !free_prop_ptr.is_null() {
                                free_prop(pr);
                            }
                        }
                    }
                    free_obj_props(props);
                    if is_cursor {
                        cursor_planes.push(pid);
                    }
                }
                if !free_planes_ptr.is_null() {
                    free_planes(pres);
                }

                if !cursor_planes.is_empty() {
                    return Some(Self {
                        fd,
                        plane_ids: cursor_planes,
                        screen_w: 1920.0,
                        screen_h: 1080.0,
                        prev_fb_id: std::sync::atomic::AtomicU64::new(0),
                        get_obj_props,
                        free_obj_props,
                        get_prop,
                        free_prop,
                    });
                }
                libc::close(fd);
            }
            None
        }
    }

    fn query_cursor(&self) -> Option<(f32, f32, bool)> {
        unsafe {
            for &pid in &self.plane_ids {
                let props = (self.get_obj_props)(self.fd, pid, 0xeeeeeeee);
                if props.is_null() {
                    continue;
                }
                let pcount = (*props).count_props as usize;
                let mut fb_id = 0u64;
                let mut crtc_x: Option<i32> = None;
                let mut crtc_y: Option<i32> = None;

                for j in 0..pcount {
                    let prop_id = *(*props).props.add(j);
                    let pval = *(*props).prop_values.add(j);
                    let pr = (self.get_prop)(self.fd, prop_id);
                    if !pr.is_null() {
                        let name_bytes =
                            std::slice::from_raw_parts((*pr).name.as_ptr() as *const u8, 32);
                        if name_bytes.starts_with(b"FB_ID\0") {
                            fb_id = pval;
                        } else if name_bytes.starts_with(b"CRTC_X\0") {
                            crtc_x = Some((pval as u32) as i32);
                        } else if name_bytes.starts_with(b"CRTC_Y\0") {
                            crtc_y = Some((pval as u32) as i32);
                        }
                        (self.free_prop)(pr);
                    }
                }
                (self.free_obj_props)(props);

                if fb_id != 0 {
                    if let (Some(cx), Some(cy)) = (crtc_x, crtc_y) {
                        let nx = (cx as f32 / self.screen_w).clamp(0.0, 1.0);
                        let ny = (cy as f32 / self.screen_h).clamp(0.0, 1.0);
                        let old_fb = self
                            .prev_fb_id
                            .swap(fb_id, std::sync::atomic::Ordering::Relaxed);
                        let cursor_shape_changed = old_fb != 0 && old_fb != fb_id;
                        return Some((nx, ny, cursor_shape_changed));
                    }
                }
            }
            None
        }
    }
}

impl Drop for DrmCursorReader {
    fn drop(&mut self) {
        unsafe {
            if self.fd >= 0 {
                libc::close(self.fd);
            }
        }
    }
}

pub struct CursorTracker {
    samples: [CursorSample; CURSOR_HISTORY_CAP],
    head: usize,
    len: usize,
    vel_x: f32,
    vel_y: f32,
    prev_left_down: bool,
    prev_right_down: bool,
    ripples: Vec<ClickRipple>,
    keystrokes: Vec<KeystrokeOverlay>,
    /// Dwell tracking for smart auto-zoom when user focuses in a small region
    dwell_anchor: (f32, f32),
    dwell_start_ms: u64,
    last_motion_ms: u64,
    drm_cursor: Option<DrmCursorReader>,
    x11_conn: Option<X11CursorConn>,
}

struct X11CursorConn {
    xlib: Xlib,
    display: *mut x11_dl::xlib::Display,
    root: x11_dl::xlib::Window,
    screen_w: f32,
    screen_h: f32,
}

unsafe impl Send for X11CursorConn {}

impl X11CursorConn {
    fn try_open() -> Option<Self> {
        unsafe {
            let xlib = Xlib::open().ok()?;
            let display = (xlib.XOpenDisplay)(ptr::null());
            if display.is_null() {
                return None;
            }
            let screen = (xlib.XDefaultScreen)(display);
            let root = (xlib.XRootWindow)(display, screen);
            let w = (xlib.XDisplayWidth)(display, screen).max(1) as f32;
            let h = (xlib.XDisplayHeight)(display, screen).max(1) as f32;
            Some(Self {
                xlib,
                display,
                root,
                screen_w: w,
                screen_h: h,
            })
        }
    }

    fn query_pointer(&self) -> Option<(f32, f32, bool, bool)> {
        unsafe {
            let mut root_ret = 0;
            let mut child_ret = 0;
            let mut root_x = 0;
            let mut root_y = 0;
            let mut win_x = 0;
            let mut win_y = 0;
            let mut mask: c_uint = 0;
            let ok = (self.xlib.XQueryPointer)(
                self.display,
                self.root,
                &mut root_ret,
                &mut child_ret,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            );
            if ok == 0 {
                return None;
            }
            let nx = (root_x as f32 / self.screen_w).clamp(0.0, 1.0);
            let ny = (root_y as f32 / self.screen_h).clamp(0.0, 1.0);
            let left = (mask & (1 << 8)) != 0;
            let right = (mask & (1 << 10)) != 0;
            Some((nx, ny, left, right))
        }
    }
}

impl Drop for X11CursorConn {
    fn drop(&mut self) {
        unsafe {
            if !self.display.is_null() {
                (self.xlib.XCloseDisplay)(self.display);
            }
        }
    }
}

impl CursorTracker {
    pub fn new() -> Self {
        Self {
            samples: [CursorSample::default(); CURSOR_HISTORY_CAP],
            head: 0,
            len: 0,
            vel_x: 0.0,
            vel_y: 0.0,
            prev_left_down: false,
            prev_right_down: false,
            ripples: Vec::with_capacity(CLICK_RIPPLE_CAP),
            keystrokes: Vec::with_capacity(KEYSTROKE_HUD_CAP),
            dwell_anchor: (0.5, 0.5),
            dwell_start_ms: 0,
            last_motion_ms: 0,
            drm_cursor: DrmCursorReader::try_open(),
            x11_conn: X11CursorConn::try_open(),
        }
    }

    /// Queries the live hardware cursor plane on `/dev/dri/card1` first (works across all
    /// native Wayland and XWayland windows on COSMIC), falling back to X11 `XQueryPointer`.
    pub fn poll_system_pointer(&self) -> Option<(f32, f32, bool, bool)> {
        let x11_buttons = self
            .x11_conn
            .as_ref()
            .and_then(|x| x.query_pointer())
            .map(|(_, _, l, r)| (l, r))
            .unwrap_or((false, false));

        if let Some(ref drm) = self.drm_cursor {
            if let Some((nx, ny, shape_changed)) = drm.query_cursor() {
                // Treat cursor bitmap change (e.g. pointer hand / I-beam / active click feedback)
                // or X11 button mask as an interaction trigger.
                return Some((nx, ny, x11_buttons.0 || shape_changed, x11_buttons.1));
            }
        }
        self.x11_conn.as_ref()?.query_pointer()
    }

    /// Push a new cursor observation and step the critically-damped 2D spring filter in O(1) time.
    pub fn update(
        &mut self,
        timestamp_ms: u64,
        raw_x: f32,
        raw_y: f32,
        left_down: bool,
        right_down: bool,
        cfg: &CursorConfig,
    ) -> (CursorSample, bool) {
        let prev = self.latest().unwrap_or(CursorSample {
            timestamp_ms,
            raw_x,
            raw_y,
            smooth_x: raw_x,
            smooth_y: raw_y,
            speed: 0.0,
            left_down: false,
            right_down: false,
        });

        let dt =
            ((timestamp_ms.saturating_sub(prev.timestamp_ms)) as f32 / 1000.0).clamp(0.001, 0.05);

        let raw_delta = ((raw_x - prev.raw_x).powi(2) + (raw_y - prev.raw_y).powi(2)).sqrt();
        if raw_delta > 0.0015 {
            self.last_motion_ms = timestamp_ms;
        }

        let (smooth_x, smooth_y) = if cfg.smoothing_enabled {
            let k = cfg.smoothing_stiffness.clamp(20.0, 600.0);
            let c = 2.0 * k.sqrt();
            let ax = -k * (prev.smooth_x - raw_x) - c * self.vel_x;
            let ay = -k * (prev.smooth_y - raw_y) - c * self.vel_y;
            self.vel_x += ax * dt;
            self.vel_y += ay * dt;
            (
                (prev.smooth_x + self.vel_x * dt).clamp(0.0, 1.0),
                (prev.smooth_y + self.vel_y * dt).clamp(0.0, 1.0),
            )
        } else {
            self.vel_x = (raw_x - prev.raw_x) / dt;
            self.vel_y = (raw_y - prev.raw_y) / dt;
            (raw_x.clamp(0.0, 1.0), raw_y.clamp(0.0, 1.0))
        };

        let speed = (self.vel_x * self.vel_x + self.vel_y * self.vel_y).sqrt();

        let clicked_now =
            (left_down && !self.prev_left_down) || (right_down && !self.prev_right_down);
        if clicked_now {
            self.last_motion_ms = timestamp_ms;
            if self.ripples.len() >= CLICK_RIPPLE_CAP {
                self.ripples.remove(0);
            }
            self.ripples.push(ClickRipple {
                x: smooth_x,
                y: smooth_y,
                start_ms: timestamp_ms,
                is_right_click: right_down && !left_down,
            });
        }
        self.prev_left_down = left_down;
        self.prev_right_down = right_down;

        self.ripples
            .retain(|r| timestamp_ms.saturating_sub(r.start_ms) < 650);
        self.keystrokes
            .retain(|k| timestamp_ms.saturating_sub(k.start_ms) < k.duration_ms);

        let dist_from_anchor = ((smooth_x - self.dwell_anchor.0).powi(2)
            + (smooth_y - self.dwell_anchor.1).powi(2))
        .sqrt();
        if dist_from_anchor > 0.055 {
            self.dwell_anchor = (smooth_x, smooth_y);
            self.dwell_start_ms = timestamp_ms;
        }

        let sample = CursorSample {
            timestamp_ms,
            raw_x,
            raw_y,
            smooth_x,
            smooth_y,
            speed,
            left_down,
            right_down,
        };

        self.samples[self.head] = sample;
        self.head = (self.head + 1) % CURSOR_HISTORY_CAP;
        if self.len < CURSOR_HISTORY_CAP {
            self.len += 1;
        }

        (sample, clicked_now)
    }

    pub fn push_keystroke(&mut self, label: impl Into<String>, timestamp_ms: u64) {
        if self.keystrokes.len() >= KEYSTROKE_HUD_CAP {
            self.keystrokes.remove(0);
        }
        self.keystrokes.push(KeystrokeOverlay {
            label: label.into(),
            start_ms: timestamp_ms,
            duration_ms: 1600,
        });
    }

    pub fn latest(&self) -> Option<CursorSample> {
        if self.len == 0 {
            return None;
        }
        let idx = (self.head + CURSOR_HISTORY_CAP - 1) % CURSOR_HISTORY_CAP;
        Some(self.samples[idx])
    }

    pub fn recent_trail(&self, count: usize) -> Vec<CursorSample> {
        let n = count.min(self.len).min(32);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let idx = (self.head + CURSOR_HISTORY_CAP - 1 - i) % CURSOR_HISTORY_CAP;
            out.push(self.samples[idx]);
        }
        out
    }

    pub fn active_ripples(&self) -> &[ClickRipple] {
        &self.ripples
    }

    pub fn active_keystrokes(&self) -> &[KeystrokeOverlay] {
        &self.keystrokes
    }

    pub fn dwell_duration_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.dwell_start_ms)
    }

    pub fn idle_since_motion_ms(&self, now_ms: u64) -> u64 {
        if self.last_motion_ms == 0 {
            0
        } else {
            now_ms.saturating_sub(self.last_motion_ms)
        }
    }
}
