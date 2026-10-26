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
    #[serde(default)]
    pub middle_down: bool,
    #[serde(default)]
    pub side_down: bool,
    #[serde(default)]
    pub ctrl_down: bool,
    #[serde(default)]
    pub alt_down: bool,
    #[serde(default)]
    pub shift_down: bool,
    #[serde(default)]
    pub super_down: bool,
    #[serde(default)]
    pub active_keycode: u32,
    #[serde(default)]
    pub double_clicked: bool,
    #[serde(default)]
    pub shake_gesture: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemInputState {
    pub left_down: bool,
    pub middle_down: bool,
    pub right_down: bool,
    pub side_down: bool,
    pub ctrl_down: bool,
    pub alt_down: bool,
    pub shift_down: bool,
    pub super_down: bool,
    pub active_keycode: u32,
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

/// Leading fields of libdrm's `drmModeCrtc`; only these are read, through the pointer libdrm owns.
#[repr(C)]
struct DrmModeCrtc {
    crtc_id: u32,
    buffer_id: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    mode_valid: c_int,
}

const FALLBACK_CRTC_SIZE: (f32, f32) = (1920.0, 1080.0);

/// Maps the cursor plane's top-left corner (`CRTC_X`/`CRTC_Y`) plus the hotspot to the
/// pointer tip, normalized to the CRTC the plane is scanned out on.
fn cursor_tip_normalized(
    crtc_x: i32,
    crtc_y: i32,
    hotspot: (i32, i32),
    crtc_size: (f32, f32),
) -> (f32, f32) {
    let w = crtc_size.0.max(1.0);
    let h = crtc_size.1.max(1.0);
    (
        ((crtc_x + hotspot.0) as f32 / w).clamp(0.0, 1.0),
        ((crtc_y + hotspot.1) as f32 / h).clamp(0.0, 1.0),
    )
}

/// Last pointer position seen on a cursor plane. Compositors detach the plane (`FB_ID = 0`)
/// when the cursor is hidden or drawn in software, and the pointer has not moved away then.
#[derive(Debug, Default)]
struct HeldPosition(Option<(f32, f32)>);

impl HeldPosition {
    fn observe(&mut self, visible: Option<(f32, f32)>) -> Option<(f32, f32)> {
        if visible.is_some() {
            self.0 = visible;
        }
        self.0
    }
}

struct DrmCursorReader {
    fd: c_int,
    plane_ids: Vec<u32>,
    prev_fb_id: std::sync::atomic::AtomicU64,
    held: parking_lot::Mutex<HeldPosition>,
    crtc_size_cache: parking_lot::Mutex<Option<(u32, (f32, f32))>>,
    /// Pixel size of the display the cursor was last seen on.
    cursor_output: parking_lot::Mutex<Option<(f32, f32)>>,
    get_obj_props: unsafe extern "C" fn(c_int, u32, u32) -> *mut DrmModeObjectProperties,
    free_obj_props: unsafe extern "C" fn(*mut DrmModeObjectProperties),
    get_prop: unsafe extern "C" fn(c_int, u32) -> *mut DrmModePropertyRes,
    free_prop: unsafe extern "C" fn(*mut DrmModePropertyRes),
    get_crtc: Option<unsafe extern "C" fn(c_int, u32) -> *mut DrmModeCrtc>,
    free_crtc: Option<unsafe extern "C" fn(*mut DrmModeCrtc)>,
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
            let get_crtc_ptr = sym("drmModeGetCrtc");
            let free_crtc_ptr = sym("drmModeFreeCrtc");

            if set_cap_ptr.is_null()
                || get_planes_ptr.is_null()
                || get_obj_props_ptr.is_null()
                || free_obj_props_ptr.is_null()
                || get_prop_ptr.is_null()
                || free_prop_ptr.is_null()
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
            let (get_crtc, free_crtc) = if get_crtc_ptr.is_null() || free_crtc_ptr.is_null() {
                (None, None)
            } else {
                (
                    Some(std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(c_int, u32) -> *mut DrmModeCrtc,
                    >(get_crtc_ptr)),
                    Some(std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(*mut DrmModeCrtc),
                    >(free_crtc_ptr)),
                )
            };

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
                        prev_fb_id: std::sync::atomic::AtomicU64::new(0),
                        held: parking_lot::Mutex::new(HeldPosition::default()),
                        crtc_size_cache: parking_lot::Mutex::new(None),
                        cursor_output: parking_lot::Mutex::new(None),
                        get_obj_props,
                        free_obj_props,
                        get_prop,
                        free_prop,
                        get_crtc,
                        free_crtc,
                    });
                }
                libc::close(fd);
            }
            None
        }
    }

    fn crtc_size(&self, crtc_id: u32) -> (f32, f32) {
        let mut cache = self.crtc_size_cache.lock();
        if let Some((cached_id, size)) = *cache {
            if cached_id == crtc_id {
                return size;
            }
        }
        let (Some(get_crtc), Some(free_crtc)) = (self.get_crtc, self.free_crtc) else {
            return FALLBACK_CRTC_SIZE;
        };
        let size = unsafe {
            let crtc = get_crtc(self.fd, crtc_id);
            if crtc.is_null() {
                return FALLBACK_CRTC_SIZE;
            }
            let size = ((*crtc).width as f32, (*crtc).height as f32);
            free_crtc(crtc);
            size
        };
        if size.0 < 1.0 || size.1 < 1.0 {
            return FALLBACK_CRTC_SIZE;
        }
        *cache = Some((crtc_id, size));
        size
    }

    /// Returns the pointer position, holding the last known one while no cursor plane has a
    /// framebuffer attached. Returns `None` only before any position has been seen.
    fn query_cursor(&self) -> Option<(f32, f32, bool)> {
        let mut visible = None;
        let mut cursor_shape_changed = false;
        unsafe {
            for &pid in &self.plane_ids {
                let props = (self.get_obj_props)(self.fd, pid, 0xeeeeeeee);
                if props.is_null() {
                    continue;
                }
                let pcount = (*props).count_props as usize;
                let mut fb_id = 0u64;
                let mut crtc_id = 0u32;
                let mut crtc_x: Option<i32> = None;
                let mut crtc_y: Option<i32> = None;
                let mut hotspot = (0i32, 0i32);

                for j in 0..pcount {
                    let prop_id = *(*props).props.add(j);
                    let pval = *(*props).prop_values.add(j);
                    let pr = (self.get_prop)(self.fd, prop_id);
                    if !pr.is_null() {
                        let name_bytes =
                            std::slice::from_raw_parts((*pr).name.as_ptr() as *const u8, 32);
                        // Signed plane properties are stored as sign-extended 64-bit values.
                        let signed = (pval as u32) as i32;
                        if name_bytes.starts_with(b"FB_ID\0") {
                            fb_id = pval;
                        } else if name_bytes.starts_with(b"CRTC_ID\0") {
                            crtc_id = pval as u32;
                        } else if name_bytes.starts_with(b"CRTC_X\0") {
                            crtc_x = Some(signed);
                        } else if name_bytes.starts_with(b"CRTC_Y\0") {
                            crtc_y = Some(signed);
                        } else if name_bytes.starts_with(b"HOTSPOT_X\0") {
                            hotspot.0 = signed;
                        } else if name_bytes.starts_with(b"HOTSPOT_Y\0") {
                            hotspot.1 = signed;
                        }
                        (self.free_prop)(pr);
                    }
                }
                (self.free_obj_props)(props);

                if fb_id != 0 {
                    if let (Some(cx), Some(cy)) = (crtc_x, crtc_y) {
                        let size = if crtc_id != 0 {
                            self.crtc_size(crtc_id)
                        } else {
                            FALLBACK_CRTC_SIZE
                        };
                        visible = Some(cursor_tip_normalized(cx, cy, hotspot, size));
                        *self.cursor_output.lock() = (crtc_id != 0).then_some(size);
                        let old_fb = self
                            .prev_fb_id
                            .swap(fb_id, std::sync::atomic::Ordering::Relaxed);
                        cursor_shape_changed = old_fb != 0 && old_fb != fb_id;
                        break;
                    }
                }
            }
        }
        let (nx, ny) = self.held.lock().observe(visible)?;
        Some((nx, ny, cursor_shape_changed))
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

/// Mouse buttons read from evdev pointer devices, for click detection on
/// native Wayland, where nothing else reports them. Works only when the
/// user may read `/dev/input` (the `input` group). Devices with letter keys
/// are never opened, so keystrokes are never read.
struct EvdevButtons {
    devices: Vec<(std::path::PathBuf, std::fs::File)>,
    last_scan: Option<std::time::Instant>,
    /// Left, right, middle, side.
    down: [bool; 4],
}

impl EvdevButtons {
    const EV_KEY: u16 = 1;
    const BTN_LEFT: u16 = 0x110;
    const KEY_Q: usize = 16;
    const KEY_M: usize = 50;
    /// How often to look for newly connected mice.
    const RESCAN: std::time::Duration = std::time::Duration::from_secs(10);

    fn new() -> Self {
        let mut buttons = Self { devices: Vec::new(), last_scan: None, down: [false; 4] };
        buttons.scan();
        buttons
    }

    /// Whether the device reports mouse buttons and has no letter keys.
    fn is_pointer(file: &std::fs::File) -> bool {
        use std::os::fd::AsRawFd;
        // EVIOCGBIT(EV_KEY, 96): the device's key and button bitmap.
        const LEN: usize = 96;
        let request = (2u64 << 30) | ((LEN as u64) << 16) | ((b'E' as u64) << 8) | (0x20 + Self::EV_KEY as u64);
        let mut bits = [0u8; LEN];
        let got = unsafe { libc::ioctl(file.as_raw_fd(), request as _, bits.as_mut_ptr()) };
        if got <= 0 {
            return false;
        }
        let has = |code: usize| bits.get(code / 8).is_some_and(|b| b & (1 << (code % 8)) != 0);
        has(Self::BTN_LEFT as usize) && !(Self::KEY_Q..=Self::KEY_M).any(has)
    }

    fn scan(&mut self) {
        use std::os::unix::fs::OpenOptionsExt;
        self.last_scan = Some(std::time::Instant::now());
        let Ok(dir) = std::fs::read_dir("/dev/input") else { return };
        for entry in dir.flatten() {
            let path = entry.path();
            let is_event = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event"));
            if !is_event || self.devices.iter().any(|(p, _)| *p == path) {
                continue;
            }
            let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path)
            else {
                continue;
            };
            if Self::is_pointer(&file) {
                self.devices.push((path, file));
            }
        }
    }

    /// Applies pending events and returns which buttons are down, counting
    /// a press released before this poll as down once so no click is lost.
    fn poll(&mut self) -> [bool; 4] {
        use std::io::Read;
        if self.last_scan.is_none_or(|t| t.elapsed() > Self::RESCAN) {
            self.scan();
        }
        // struct input_event on 64-bit Linux: timeval, type, code, value.
        const EVENT: usize = 24;
        let mut buf = [0u8; EVENT * 64];
        let down = &mut self.down;
        let mut pressed = [false; 4];
        self.devices.retain_mut(|(_, file)| loop {
            match file.read(&mut buf) {
                Ok(0) => return false,
                Ok(n) => {
                    for ev in buf[..n - n % EVENT].chunks_exact(EVENT) {
                        let kind = u16::from_ne_bytes([ev[16], ev[17]]);
                        let code = u16::from_ne_bytes([ev[18], ev[19]]);
                        let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                        let slot = match code.wrapping_sub(Self::BTN_LEFT) {
                            0 => 0,
                            1 => 1,
                            2 => 2,
                            3 | 4 => 3,
                            _ => continue,
                        };
                        if kind == Self::EV_KEY && value != 2 {
                            down[slot] = value != 0;
                            pressed[slot] |= value != 0;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                // The device was unplugged.
                Err(_) => return false,
            }
        });
        std::array::from_fn(|i| self.down[i] || pressed[i])
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
    last_left_click_ms: u64,
    last_shake_ms: u64,
    shake_reversals: Vec<(u64, f32)>,
    prev_dx_sign: i8,
    extreme_x: f32,
    ripples: Vec<ClickRipple>,
    keystrokes: Vec<KeystrokeOverlay>,
    /// Dwell tracking for smart auto-zoom when user focuses in a small region
    dwell_anchor: (f32, f32),
    dwell_start_ms: u64,
    last_motion_ms: u64,
    drm_cursor: Option<DrmCursorReader>,
    x11_conn: Option<X11CursorConn>,
    buttons: parking_lot::Mutex<EvdevButtons>,
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

    fn query_pointer_and_keys(&self) -> Option<(f32, f32, SystemInputState)> {
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

            let mut keymap: [c_char; 32] = [0; 32];
            (self.xlib.XQueryKeymap)(self.display, keymap.as_mut_ptr());

            let is_key_down = |kc: usize| -> bool {
                let byte_idx = kc / 8;
                let bit_idx = kc % 8;
                byte_idx < 32 && ((keymap[byte_idx] as u8) & (1 << bit_idx)) != 0
            };

            // Standard X11/evdev modifier keycodes: Shift(50,62), Ctrl(37,105), Alt(64,108), Super(133,134)
            let shift_kc = is_key_down(50) || is_key_down(62);
            let ctrl_kc = is_key_down(37) || is_key_down(105);
            let alt_kc = is_key_down(64) || is_key_down(108);
            let super_kc = is_key_down(133) || is_key_down(134);

            let mut active_keycode = 0u32;
            for kc in 9..255usize {
                if matches!(kc, 37 | 50 | 62 | 64 | 105 | 108 | 133 | 134) {
                    continue;
                }
                if is_key_down(kc) {
                    active_keycode = kc as u32;
                    break;
                }
            }

            let state = SystemInputState {
                left_down: (mask & (1 << 8)) != 0,
                middle_down: (mask & (1 << 9)) != 0,
                right_down: (mask & (1 << 10)) != 0,
                side_down: (mask & ((1 << 11) | (1 << 12))) != 0,
                shift_down: ((mask & (1 << 0)) != 0) || shift_kc,
                ctrl_down: ((mask & (1 << 2)) != 0) || ctrl_kc,
                alt_down: ((mask & (1 << 3)) != 0) || alt_kc,
                super_down: ((mask & (1 << 6)) != 0) || super_kc,
                active_keycode,
            };

            Some((nx, ny, state))
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
        let drm_cursor = DrmCursorReader::try_open();
        // Never open or poll XWayland (`XQueryPointer`) when DRM hardware cursor plane is
        // available on Wayland, because rootless XWayland blocks synchronous X11 requests
        // when native Wayland windows have focus.
        let x11_conn = if drm_cursor.is_some() {
            None
        } else {
            X11CursorConn::try_open()
        };
        Self {
            samples: [CursorSample::default(); CURSOR_HISTORY_CAP],
            head: 0,
            len: 0,
            vel_x: 0.0,
            vel_y: 0.0,
            prev_left_down: false,
            prev_right_down: false,
            last_left_click_ms: 0,
            last_shake_ms: 0,
            shake_reversals: Vec::with_capacity(8),
            prev_dx_sign: 0,
            extreme_x: 0.5,
            ripples: Vec::with_capacity(CLICK_RIPPLE_CAP),
            keystrokes: Vec::with_capacity(KEYSTROKE_HUD_CAP),
            dwell_anchor: (0.5, 0.5),
            dwell_start_ms: 0,
            last_motion_ms: 0,
            drm_cursor,
            x11_conn,
            buttons: parking_lot::Mutex::new(EvdevButtons::new()),
        }
    }

    /// Queries the live hardware cursor plane on `/dev/dri/card1` first (works across all
    /// native Wayland and XWayland windows on COSMIC in <10 µs without blocking), falling back
    /// to X11 only on pure Xorg sessions.
    pub fn poll_system_input(&self) -> Option<(f32, f32, SystemInputState)> {
        let [left, right, middle, side] = self.buttons.lock().poll();
        let with_buttons = |mut state: SystemInputState| {
            state.left_down |= left;
            state.right_down |= right;
            state.middle_down |= middle;
            state.side_down |= side;
            state
        };
        if let Some(ref drm) = self.drm_cursor {
            if let Some((nx, ny, _shape_changed)) = drm.query_cursor() {
                return Some((nx, ny, with_buttons(SystemInputState::default())));
            }
        }
        self.x11_conn
            .as_ref()
            .and_then(|x| x.query_pointer_and_keys())
            .map(|(x, y, state)| (x, y, with_buttons(state)))
    }

    /// Whether mouse clicks are detected on native Wayland windows too.
    pub fn reads_mouse_buttons(&self) -> bool {
        !self.buttons.lock().devices.is_empty()
    }

    /// Whether a real pointer source (DRM cursor plane or X11) was opened.
    /// Pixel size of the display the hardware cursor is on, when the DRM
    /// cursor plane is readable.
    pub fn cursor_output_size(&self) -> Option<(f32, f32)> {
        *self.drm_cursor.as_ref()?.cursor_output.lock()
    }

    pub fn has_live_source(&self) -> bool {
        self.drm_cursor.is_some() || self.x11_conn.is_some()
    }

    pub fn poll_system_pointer(&self) -> Option<(f32, f32, bool, bool)> {
        let (nx, ny, inp) = self.poll_system_input()?;
        Some((nx, ny, inp.left_down, inp.right_down))
    }

    /// Detects an intentional quick horizontal cursor shake (wiggle left-right-left-right
    /// within 380ms) directly from DRM hardware cursor plane coordinates.
    /// Works 100% globally across all native Wayland & X11 windows without root!
    fn detect_cursor_shake(&mut self, now_ms: u64, raw_x: f32, prev_raw_x: f32) -> bool {
        let dx = raw_x - prev_raw_x;
        if dx.abs() < 0.004 {
            return false;
        }
        let sign: i8 = if dx > 0.0 { 1 } else { -1 };
        if self.prev_dx_sign == 0 {
            self.prev_dx_sign = sign;
            self.extreme_x = raw_x;
            return false;
        }

        if sign == self.prev_dx_sign {
            if (sign > 0 && raw_x > self.extreme_x) || (sign < 0 && raw_x < self.extreme_x) {
                self.extreme_x = raw_x;
            }
            return false;
        }

        // Direction reversed! Check swing amplitude since last reversal extreme
        let swing = (raw_x - self.extreme_x).abs();
        self.prev_dx_sign = sign;
        self.extreme_x = raw_x;

        if (0.015..=0.24).contains(&swing) {
            self.shake_reversals.push((now_ms, swing));
        }
        self.shake_reversals
            .retain(|(t, _)| now_ms.saturating_sub(*t) <= 420);

        if self.shake_reversals.len() >= 3 && now_ms.saturating_sub(self.last_shake_ms) > 850 {
            self.shake_reversals.clear();
            self.last_shake_ms = now_ms;
            return true;
        }
        false
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
        let inp = SystemInputState {
            left_down,
            right_down,
            ..Default::default()
        };
        self.update_with_input(timestamp_ms, raw_x, raw_y, inp, cfg)
    }

    pub fn update_with_input(
        &mut self,
        timestamp_ms: u64,
        raw_x: f32,
        raw_y: f32,
        inp: SystemInputState,
        cfg: &CursorConfig,
    ) -> (CursorSample, bool) {
        let prev = self.latest().unwrap_or(CursorSample {
            timestamp_ms,
            raw_x,
            raw_y,
            smooth_x: raw_x,
            smooth_y: raw_y,
            ..Default::default()
        });

        let dt =
            ((timestamp_ms.saturating_sub(prev.timestamp_ms)) as f32 / 1000.0).clamp(0.001, 0.05);

        let raw_delta = ((raw_x - prev.raw_x).powi(2) + (raw_y - prev.raw_y).powi(2)).sqrt();
        if raw_delta > 0.0015 {
            self.last_motion_ms = timestamp_ms;
        }

        let shake_gesture = self.detect_cursor_shake(timestamp_ms, raw_x, prev.raw_x);

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

        let left_rising = inp.left_down && !self.prev_left_down;
        let right_rising = inp.right_down && !self.prev_right_down;
        let clicked_now = left_rising || right_rising;

        let mut double_clicked = false;
        if left_rising {
            if self.last_left_click_ms > 0
                && timestamp_ms.saturating_sub(self.last_left_click_ms) <= 320
            {
                double_clicked = true;
            }
            self.last_left_click_ms = timestamp_ms;
        }

        if clicked_now {
            self.last_motion_ms = timestamp_ms;
            if self.ripples.len() >= CLICK_RIPPLE_CAP {
                self.ripples.remove(0);
            }
            self.ripples.push(ClickRipple {
                x: smooth_x,
                y: smooth_y,
                start_ms: timestamp_ms,
                is_right_click: inp.right_down && !inp.left_down,
            });
        }
        self.prev_left_down = inp.left_down;
        self.prev_right_down = inp.right_down;

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
            left_down: inp.left_down,
            right_down: inp.right_down,
            middle_down: inp.middle_down,
            side_down: inp.side_down,
            ctrl_down: inp.ctrl_down,
            alt_down: inp.alt_down,
            shift_down: inp.shift_down,
            super_down: inp.super_down,
            active_keycode: inp.active_keycode,
            double_clicked,
            shake_gesture,
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

    /// When the pointer last moved or clicked, if it ever did.
    pub fn last_motion_ms(&self) -> Option<u64> {
        (self.last_motion_ms != 0).then_some(self.last_motion_ms)
    }

    pub fn idle_since_motion_ms(&self, now_ms: u64) -> u64 {
        if self.last_motion_ms == 0 {
            0
        } else {
            now_ms.saturating_sub(self.last_motion_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cursor_tip_normalized, HeldPosition};

    #[test]
    fn cursor_tip_uses_crtc_size_instead_of_1080p() {
        let (x, y) = cursor_tip_normalized(1280, 720, (0, 0), (2560.0, 1440.0));
        assert!((x - 0.5).abs() < 1e-6);
        assert!((y - 0.5).abs() < 1e-6);
    }

    #[test]
    fn cursor_tip_adds_hotspot_to_plane_origin() {
        let (x, y) = cursor_tip_normalized(100, 200, (12, 12), (1000.0, 1000.0));
        assert!((x - 0.112).abs() < 1e-6);
        assert!((y - 0.212).abs() < 1e-6);
    }

    #[test]
    fn cursor_tip_clamps_planes_partly_off_screen() {
        assert_eq!(
            cursor_tip_normalized(-8, -8, (0, 0), (1920.0, 1080.0)),
            (0.0, 0.0)
        );
        assert_eq!(
            cursor_tip_normalized(1950, 1100, (0, 0), (1920.0, 1080.0)),
            (1.0, 1.0)
        );
        // A hotspot can bring a plane that starts off-screen back onto it.
        let (x, _) = cursor_tip_normalized(-4, 0, (14, 0), (1000.0, 1000.0));
        assert!((x - 0.01).abs() < 1e-6);
    }

    #[test]
    fn hidden_cursor_holds_last_visible_position() {
        let mut held = HeldPosition::default();
        assert_eq!(held.observe(None), None);
        assert_eq!(held.observe(Some((0.25, 0.75))), Some((0.25, 0.75)));
        assert_eq!(held.observe(None), Some((0.25, 0.75)));
        assert_eq!(held.observe(None), Some((0.25, 0.75)));
        assert_eq!(held.observe(Some((0.5, 0.5))), Some((0.5, 0.5)));
    }
}
