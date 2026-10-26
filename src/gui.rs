use crate::island_position::{self, IslandPos, DEFAULT_TOP_MARGIN};
use crate::state::SharedStudioState;
use crate::zoom_engine::{AutoZoomMode, ComboActionMode, ZoomComboBinding};
use glib::object::ObjectType;
use glib::ControlFlow;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, DrawingArea,
    EventControllerKey, EventControllerMotion, EventControllerScroll, EventControllerScrollFlags,
    GestureClick, GestureDrag, Image, Label, Orientation,
};
use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const ISLAND_WINDOW_TITLE: &str = "HyperGSR Dynamic Island";

fn asset_path(name: &str) -> PathBuf {
    let cwd_asset = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("assets")
        .join(name);
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
    {
        // npm bundle, system package (/usr/share/hypergsr), then a source checkout.
        for base in [
            exe_dir.join("assets"),
            exe_dir.join("../share/hypergsr/assets"),
            exe_dir.join("../../assets"),
        ] {
            let candidate = base.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    if cwd_asset.exists() {
        return cwd_asset;
    }
    crate::bundled::icon(name).unwrap_or(cwd_asset)
}

/// Makes the bundled Inter font available to this process through
/// fontconfig, before GTK creates its font map.
fn register_ui_font() {
    let Some(path) = crate::bundled::ui_font() else {
        return;
    };
    let Ok(path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return;
    };
    unsafe {
        let lib = CString::new("libfontconfig.so.1").unwrap();
        let handle = libc::dlopen(lib.as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            return;
        }
        let name = CString::new("FcConfigAppFontAddFile").unwrap();
        let sym = libc::dlsym(handle, name.as_ptr());
        if sym.is_null() {
            return;
        }
        type AddFontFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
        let add: AddFontFn = std::mem::transmute(sym);
        // A null config means the current one, which Pango uses.
        add(std::ptr::null_mut(), path.as_ptr());
    }
}

pub fn launch_gtk_ui(state: SharedStudioState, _web_port: u16) {
    register_ui_font();
    let app = Application::builder()
        .application_id(crate::shortcuts::APP_ID)
        .build();

    app.connect_activate(move |app| {
        // Launching HyperGSR again shows the island that is already open.
        if let Some(window) = app.windows().into_iter().find(|w| w.title().as_deref() == Some(ISLAND_WINDOW_TITLE)) {
            window.present();
            return;
        }
        build_dynamic_island(app, state.clone());
    });

    let empty_args: Vec<String> = vec![];
    app.run_with_args(&empty_args);
}

const LAYER_EDGE_LEFT: c_int = 0;
const LAYER_EDGE_TOP: c_int = 2;

/// Runtime handle to the dlopen'ed gtk4-layer-shell calls that position the island.
#[derive(Clone, Copy)]
struct LayerShell {
    window: *mut c_void,
    set_anchor: unsafe extern "C" fn(*mut c_void, c_int, c_int),
    set_margin: unsafe extern "C" fn(*mut c_void, c_int, c_int),
}

impl LayerShell {
    /// `None` is the default spot: centered under the top edge.
    fn place(&self, pos: Option<IslandPos>) {
        unsafe {
            match pos {
                Some(p) => {
                    (self.set_anchor)(self.window, LAYER_EDGE_LEFT, 1);
                    (self.set_anchor)(self.window, LAYER_EDGE_TOP, 1);
                    (self.set_margin)(self.window, LAYER_EDGE_LEFT, p.x);
                    (self.set_margin)(self.window, LAYER_EDGE_TOP, p.y);
                }
                None => {
                    (self.set_anchor)(self.window, LAYER_EDGE_LEFT, 0);
                    (self.set_margin)(self.window, LAYER_EDGE_LEFT, 0);
                    (self.set_anchor)(self.window, LAYER_EDGE_TOP, 1);
                    (self.set_margin)(self.window, LAYER_EDGE_TOP, DEFAULT_TOP_MARGIN);
                }
            }
        }
    }
}

/// gtk4-layer-shell entry points, resolved once at runtime so the recorder
/// still starts on desktops without the library.
pub(crate) struct LayerApi {
    pub init_for_window: unsafe extern "C" fn(*mut c_void),
    pub set_layer: unsafe extern "C" fn(*mut c_void, c_int),
    pub set_anchor: unsafe extern "C" fn(*mut c_void, c_int, c_int),
    pub set_margin: unsafe extern "C" fn(*mut c_void, c_int, c_int),
    pub set_exclusive_zone: unsafe extern "C" fn(*mut c_void, c_int),
    pub set_keyboard_mode: unsafe extern "C" fn(*mut c_void, c_int),
    pub set_namespace: unsafe extern "C" fn(*mut c_void, *const c_char),
}

// SAFETY: plain C function pointers into a library that stays loaded.
unsafe impl Send for LayerApi {}
unsafe impl Sync for LayerApi {}

/// `None` when gtk4-layer-shell is missing or the compositor lacks
/// `zwlr_layer_shell_v1` (GNOME, X11).
pub(crate) fn layer_api() -> Option<&'static LayerApi> {
    static API: std::sync::OnceLock<Option<LayerApi>> = std::sync::OnceLock::new();
    API.get_or_init(|| unsafe {
        let wayland = gdk4::Display::default()
            .is_some_and(|d| d.type_().name() == "GdkWaylandDisplay");
        if !wayland {
            return None;
        }
        let path = CString::new("/usr/local/lib/x86_64-linux-gnu/libgtk4-layer-shell.so.0").unwrap();
        let mut handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
        if handle.is_null() {
            let fallback = CString::new("libgtk4-layer-shell.so.0").unwrap();
            handle = libc::dlopen(fallback.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
        }
        if handle.is_null() {
            return None;
        }
        let sym = |name: &str| -> *mut c_void {
            let cs = CString::new(name).unwrap();
            libc::dlsym(handle, cs.as_ptr())
        };
        let names = [
            "gtk_layer_is_supported",
            "gtk_layer_init_for_window",
            "gtk_layer_set_layer",
            "gtk_layer_set_anchor",
            "gtk_layer_set_margin",
            "gtk_layer_set_exclusive_zone",
            "gtk_layer_set_keyboard_mode",
            "gtk_layer_set_namespace",
        ];
        if names.iter().any(|name| sym(name).is_null()) {
            return None;
        }
        type IsSupportedFn = unsafe extern "C" fn() -> c_int;
        let is_supported: IsSupportedFn = std::mem::transmute(sym("gtk_layer_is_supported"));
        if is_supported() == 0 {
            return None;
        }
        // Each symbol is cast to the field's function type, as declared above.
        #[allow(clippy::missing_transmute_annotations)]
        Some(LayerApi {
            init_for_window: std::mem::transmute(sym("gtk_layer_init_for_window")),
            set_layer: std::mem::transmute(sym("gtk_layer_set_layer")),
            set_anchor: std::mem::transmute(sym("gtk_layer_set_anchor")),
            set_margin: std::mem::transmute(sym("gtk_layer_set_margin")),
            set_exclusive_zone: std::mem::transmute(sym("gtk_layer_set_exclusive_zone")),
            set_keyboard_mode: std::mem::transmute(sym("gtk_layer_set_keyboard_mode")),
            set_namespace: std::mem::transmute(sym("gtk_layer_set_namespace")),
        })
    })
    .as_ref()
}

fn init_wayland_layer_shell(window: &ApplicationWindow) -> Option<LayerShell> {
    let api = layer_api()?;
    unsafe {
        let win_ptr = window.as_ptr() as *mut c_void;
        (api.init_for_window)(win_ptr);
        let ns = CString::new("hypergsr-dynamic-island").unwrap();
        (api.set_namespace)(win_ptr, ns.as_ptr());

        (api.set_layer)(win_ptr, 3); // OVERLAY: above every window, fullscreen ones included
        (api.set_anchor)(win_ptr, 1, 0);
        (api.set_anchor)(win_ptr, 3, 0);
        (api.set_exclusive_zone)(win_ptr, -1);
        (api.set_keyboard_mode)(win_ptr, 2); // ON_DEMAND
        let layer = LayerShell {
            window: win_ptr,
            set_anchor: api.set_anchor,
            set_margin: api.set_margin,
        };
        layer.place(None);
        Some(layer)
    }
}

fn monitor_size(window: &ApplicationWindow) -> Option<(i32, i32)> {
    let surface = window.surface()?;
    let monitor = WidgetExt::display(window).monitor_at_surface(&surface)?;
    let geometry = monitor.geometry();
    Some((geometry.width(), geometry.height()))
}

/// Logical height of the island's monitor, or of the first monitor before
/// the surface has been placed on one.
fn screen_height(window: &ApplicationWindow) -> i32 {
    monitor_size(window)
        .map(|(_, h)| h)
        .or_else(|| {
            let monitors = WidgetExt::display(window).monitors();
            let first = monitors.item(0)?.downcast::<gdk4::Monitor>().ok()?;
            Some(first.geometry().height())
        })
        .unwrap_or(1080)
}

fn island_size(window: &ApplicationWindow) -> (i32, i32) {
    (window.width().max(1), window.height().max(1))
}

/// Lets the user drag the island by `handle`. Layer-shell surfaces cannot be
/// moved by the compositor, so the drag rewrites the surface margins instead;
/// other shells get a regular compositor move. Returns a callback that pulls
/// the island back on screen after it changes size.
fn install_island_drag(
    window: &ApplicationWindow,
    handle: &GtkBox,
    layer: Option<LayerShell>,
) -> Rc<dyn Fn()> {
    handle.set_cursor_from_name(Some("grab"));
    handle.set_tooltip_text(Some("Drag to move · Double-click to re-center"));
    let drag = GestureDrag::new();
    drag.set_button(gdk4::BUTTON_PRIMARY);

    let Some(layer) = layer else {
        let win = window.clone();
        let handle_ref = handle.clone();
        drag.connect_drag_begin(move |gesture, x, y| {
            let Some(toplevel) = win
                .surface()
                .and_then(|s| s.dynamic_cast::<gdk4::Toplevel>().ok())
            else {
                return;
            };
            let Some(device) = gesture.device() else {
                return;
            };
            let point = handle_ref
                .compute_point(&win, &gtk4::graphene::Point::new(x as f32, y as f32))
                .unwrap_or_else(|| gtk4::graphene::Point::new(x as f32, y as f32));
            let (sx, sy) = win.surface_transform();
            toplevel.begin_move(
                &device,
                gesture.current_button() as i32,
                point.x() as f64 + sx,
                point.y() as f64 + sy,
                gesture.current_event_time(),
            );
            gesture.reset();
        });
        handle.add_controller(drag);
        return Rc::new(|| {});
    };

    let pos: Rc<Cell<Option<IslandPos>>> = Rc::new(Cell::new(island_position::load()));
    layer.place(pos.get());

    // Drag offsets are relative to where the compositor last put the surface,
    // which lags the margins we request by one frame.
    let applied = Rc::new(Cell::new(IslandPos { x: 0, y: 0 }));
    let requested = Rc::new(Cell::new(IslandPos { x: 0, y: 0 }));
    {
        let applied = applied.clone();
        let requested = requested.clone();
        window.connect_realize(move |win| {
            if let Some(clock) = win.frame_clock() {
                let applied = applied.clone();
                let requested = requested.clone();
                clock.connect_after_paint(move |_| applied.set(requested.get()));
            }
        });
    }

    let keep_on_screen: Rc<dyn Fn()> = {
        let win = window.clone();
        let pos = pos.clone();
        let applied = applied.clone();
        let requested = requested.clone();
        Rc::new(move || {
            let (Some(current), Some(monitor)) = (pos.get(), monitor_size(&win)) else {
                return;
            };
            let clamped = island_position::clamp(current, island_size(&win), monitor);
            if clamped != current {
                pos.set(Some(clamped));
                applied.set(clamped);
                requested.set(clamped);
                layer.place(Some(clamped));
            }
        })
    };
    {
        let keep_on_screen = keep_on_screen.clone();
        window.connect_map(move |_| {
            let keep_on_screen = keep_on_screen.clone();
            glib::idle_add_local_once(move || keep_on_screen());
        });
    }

    {
        let win = window.clone();
        let pos = pos.clone();
        let applied = applied.clone();
        let requested = requested.clone();
        let handle_ref = handle.clone();
        drag.connect_drag_begin(move |_, _, _| {
            let current = pos.get().unwrap_or_else(|| {
                let (width, _) = island_size(&win);
                let monitor_width = monitor_size(&win).map_or(width, |m| m.0);
                island_position::centered(width, monitor_width)
            });
            pos.set(Some(current));
            applied.set(current);
            requested.set(current);
            layer.place(Some(current));
            handle_ref.set_cursor_from_name(Some("grabbing"));
        });
    }
    {
        let win = window.clone();
        let pos = pos.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let base = applied.get();
            let mut target = IslandPos {
                x: base.x + dx.round() as i32,
                y: base.y + dy.round() as i32,
            };
            if let Some(monitor) = monitor_size(&win) {
                target = island_position::clamp(target, island_size(&win), monitor);
            }
            if target != requested.get() {
                requested.set(target);
                pos.set(Some(target));
                layer.place(Some(target));
            }
        });
    }
    {
        let pos = pos.clone();
        let handle_ref = handle.clone();
        drag.connect_drag_end(move |_, _, _| {
            handle_ref.set_cursor_from_name(Some("grab"));
            if let Some(p) = pos.get() {
                if let Err(err) = island_position::save(p) {
                    eprintln!("[HyperGSR] Could not save island position: {err}");
                }
            }
        });
    }
    handle.add_controller(drag);

    let recenter = GestureClick::new();
    recenter.set_button(gdk4::BUTTON_PRIMARY);
    recenter.connect_pressed(move |_, n_press, _, _| {
        if n_press == 2 {
            pos.set(None);
            layer.place(None);
            island_position::forget();
        }
    });
    handle.add_controller(recenter);

    keep_on_screen
}

fn load_dynamic_island_css() {
    let provider = CssProvider::new();
    provider.load_from_data(
        r#"
        window,
        window.background,
        window.csd,
        window.solid-csd {
            background-color: transparent;
            background-image: none;
            background: transparent;
            border: none;
            box-shadow: none;
            outline: none;
            color: #f4f4f6;
            font-family: "Inter Variable", "Inter", system-ui, sans-serif;
            font-feature-settings: "cv11", "ss03";
        }

        /* =================================================================
           OBSIDIAN DARK MODE (Default — Screen Studio / Apple Dynamic Island)
           ================================================================= */
        .island-shell {
            background-color: rgba(8, 8, 12, 0.98);
            border: 1px solid rgba(255, 255, 255, 0.14);
            border-radius: 22px;
            padding: 5px 8px;
            margin: 0px;
        }
        .island-shell-rec {
            background-color: rgba(12, 6, 9, 0.98);
            border: 1px solid rgba(244, 63, 94, 0.55);
            border-radius: 22px;
            padding: 5px 8px;
            margin: 0px;
        }

        .island-top-row {
            padding: 0px;
            margin: 0px;
        }

        .brand-pill {
            background-color: rgba(255, 255, 255, 0.055);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 999px;
            padding: 4px 10px 4px 8px;
        }

        .island-title {
            font-size: 11.5px;
            font-weight: 800;
            color: #f8fafc;
            letter-spacing: -0.2px;
        }
        .island-timer-rec {
            font-family: "Inter Variable", "Inter", sans-serif;
            font-feature-settings: "tnum", "cv11";
            font-size: 11.5px;
            font-weight: 800;
            color: #fb7185;
        }

        button {
            background-image: none;
            box-shadow: none;
            outline: none;
            border-radius: 999px;
            padding: 4px 10px;
            min-height: 24px;
            min-width: 0px;
            font-size: 11px;
            font-weight: 700;
            border: 1px solid rgba(255, 255, 255, 0.08);
            background-color: rgba(255, 255, 255, 0.06);
            color: #e4e4e7;
        }
        button:hover {
            background-color: rgba(255, 255, 255, 0.12);
            border-color: rgba(255, 255, 255, 0.18);
            color: #ffffff;
        }

        button.lens-pill {
            background-color: rgba(99, 102, 241, 0.14);
            border: 1px solid rgba(129, 140, 248, 0.30);
            color: #c7d2fe;
            font-family: "Inter Variable", "Inter", sans-serif;
            font-feature-settings: "tnum", "cv11";
            font-size: 10.5px;
            font-weight: 700;
            padding: 4px 10px;
        }
        button.lens-pill-zoomed {
            background-color: rgba(16, 185, 129, 0.18);
            border: 1px solid rgba(52, 211, 153, 0.48);
            color: #6ee7b7;
            font-family: "Inter Variable", "Inter", sans-serif;
            font-feature-settings: "tnum", "cv11";
            font-size: 10.5px;
            font-weight: 800;
            padding: 4px 10px;
        }

        button.shutter-rec {
            background-color: #e11d48;
            border: 1px solid rgba(251, 113, 133, 0.55);
            color: #ffffff;
            font-weight: 800;
            padding: 4px 12px;
        }
        button.shutter-rec:hover {
            background-color: #f43f5e;
        }

        button.shutter-stop {
            background-color: rgba(225, 29, 72, 0.24);
            border: 1px solid #fb7185;
            color: #fecdd3;
            font-weight: 800;
            padding: 4px 12px;
        }

        button.theme-pill {
            background-color: rgba(255, 255, 255, 0.06);
            border: 1px solid rgba(255, 255, 255, 0.10);
            padding: 4px 8px;
        }

        button.studio-pill {
            background-color: rgba(255, 255, 255, 0.06);
            border: 1px solid rgba(255, 255, 255, 0.10);
            color: #d4d4d8;
            padding: 4px 10px;
        }
        button.studio-pill-open {
            background-color: rgba(99, 102, 241, 0.24);
            border: 1px solid rgba(129, 140, 248, 0.45);
            color: #e0e7ff;
            padding: 4px 10px;
        }

        button.close-pill {
            background-color: transparent;
            border: 1px solid transparent;
            color: #71717a;
            padding: 4px 7px;
            font-size: 11px;
        }
        button.close-pill:hover {
            background-color: rgba(244, 63, 94, 0.18);
            color: #fda4af;
        }

        /* Compact Vertical Studio Drawer (matches top bar width — zero dead space) */
        .island-drawer {
            border-top: 1px solid rgba(255, 255, 255, 0.08);
            margin-top: 6px;
            padding: 8px 2px 2px 2px;
        }

        .bento-card {
            background-color: rgba(17, 17, 24, 0.96);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 14px;
            padding: 9px 10px;
        }

        .section-cap {
            font-size: 9.5px;
            font-weight: 800;
            letter-spacing: 0.7px;
            color: #94a3b8;
        }

        .coord-badge {
            font-family: "Inter Variable", "Inter", sans-serif;
            font-feature-settings: "tnum", "cv11";
            font-size: 9.5px;
            font-weight: 700;
            color: #34d399;
        }

        .seg-track {
            background-color: rgba(5, 5, 8, 0.90);
            border: 1px solid rgba(255, 255, 255, 0.07);
            border-radius: 10px;
            padding: 2px;
        }
        button.seg-btn {
            background-color: transparent;
            border: 1px solid transparent;
            border-radius: 8px;
            color: #94a3b8;
            font-size: 10.5px;
            font-weight: 700;
            padding: 3px 7px;
            min-height: 20px;
        }
        button.seg-btn:hover {
            color: #f1f5f9;
            background-color: rgba(255, 255, 255, 0.04);
        }
        button.seg-btn-active {
            background-color: #4f46e5;
            border: 1px solid rgba(165, 180, 252, 0.40);
            border-radius: 8px;
            color: #ffffff;
            font-size: 10.5px;
            font-weight: 800;
            padding: 3px 7px;
            min-height: 20px;
        }

        button.toggle-chip {
            background-color: rgba(255, 255, 255, 0.04);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 9px;
            color: #94a3b8;
            font-size: 10.5px;
            font-weight: 700;
            padding: 4px 8px;
        }
        button.toggle-chip-on {
            background-color: rgba(16, 185, 129, 0.14);
            border: 1px solid rgba(52, 211, 153, 0.36);
            border-radius: 9px;
            color: #6ee7b7;
            font-size: 10.5px;
            font-weight: 800;
            padding: 4px 8px;
        }

        button.action-chip {
            background-color: rgba(99, 102, 241, 0.16);
            border: 1px solid rgba(129, 140, 248, 0.32);
            border-radius: 9px;
            color: #c7d2fe;
            font-size: 10.5px;
            font-weight: 700;
            padding: 4px 10px;
        }
        button.action-chip:hover {
            background-color: rgba(99, 102, 241, 0.28);
            color: #ffffff;
        }

        .status-strip {
            border-top: 1px solid rgba(255, 255, 255, 0.07);
            padding-top: 6px;
            margin-top: 2px;
            font-family: "Inter Variable", "Inter", sans-serif;
            font-feature-settings: "tnum", "cv11";
            font-size: 9.5px;
            color: #94a3b8;
        }

        /* =================================================================
           PEARL FROSTED LIGHT MODE (`.theme-light`)
           ================================================================= */
        .theme-light.island-shell {
            background-color: rgba(248, 250, 252, 0.98);
            border: 1px solid rgba(15, 23, 42, 0.14);
            color: #0f172a;
        }
        .theme-light.island-shell-rec {
            background-color: rgba(255, 241, 242, 0.98);
            border: 1px solid rgba(225, 29, 72, 0.45);
            color: #0f172a;
        }
        .theme-light .brand-pill {
            background-color: rgba(15, 23, 42, 0.05);
            border: 1px solid rgba(15, 23, 42, 0.09);
        }
        .theme-light .island-title {
            color: #0f172a;
        }
        .theme-light .island-timer-rec {
            color: #e11d48;
        }
        .theme-light button {
            background-color: rgba(15, 23, 42, 0.05);
            border: 1px solid rgba(15, 23, 42, 0.10);
            color: #1e293b;
        }
        .theme-light button:hover {
            background-color: rgba(15, 23, 42, 0.10);
            color: #0f172a;
        }
        .theme-light button.lens-pill {
            background-color: rgba(79, 70, 229, 0.10);
            border: 1px solid rgba(79, 70, 229, 0.25);
            color: #4338ca;
        }
        .theme-light button.lens-pill-zoomed {
            background-color: rgba(16, 185, 129, 0.15);
            border: 1px solid rgba(5, 150, 105, 0.40);
            color: #065f46;
        }
        .theme-light button.shutter-rec {
            background-color: #e11d48;
            border: 1px solid #be123c;
            color: #ffffff;
        }
        .theme-light button.shutter-stop {
            background-color: rgba(225, 29, 72, 0.15);
            border: 1px solid #e11d48;
            color: #be123c;
        }
        .theme-light button.studio-pill-open {
            background-color: rgba(79, 70, 229, 0.14);
            border: 1px solid rgba(79, 70, 229, 0.35);
            color: #3730a3;
        }
        .theme-light .island-drawer {
            border-top: 1px solid rgba(15, 23, 42, 0.08);
        }
        .theme-light .bento-card {
            background-color: #ffffff;
            border: 1px solid rgba(15, 23, 42, 0.09);
        }
        .theme-light .section-cap {
            color: #64748b;
        }
        .theme-light .coord-badge {
            color: #059669;
        }
        .theme-light .seg-track {
            background-color: #e2e8f0;
            border: 1px solid rgba(15, 23, 42, 0.08);
        }
        .theme-light button.seg-btn {
            color: #475569;
        }
        .theme-light button.seg-btn-active {
            background-color: #4f46e5;
            color: #ffffff;
        }
        .theme-light button.toggle-chip {
            background-color: #f1f5f9;
            border: 1px solid #cbd5e1;
            color: #64748b;
        }
        .theme-light button.toggle-chip-on {
            background-color: rgba(16, 185, 129, 0.14);
            border: 1px solid rgba(5, 150, 105, 0.38);
            color: #065f46;
        }
        .theme-light button.action-chip {
            background-color: rgba(79, 70, 229, 0.12);
            border: 1px solid rgba(79, 70, 229, 0.28);
            color: #4338ca;
        }
        .theme-light .status-strip {
            border-top: 1px solid rgba(15, 23, 42, 0.08);
            color: #475569;
        }
        "#,
    );
    if let Some(display) = gdk4::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_USER,
        );
        let polish = CssProvider::new();
        polish.load_from_data(ISLAND_POLISH_CSS);
        gtk4::style_context_add_provider_for_display(
            &display,
            &polish,
            gtk4::STYLE_PROVIDER_PRIORITY_USER + 2,
        );
    }
}

/// Finish layer over the base theme: lighter Inter weights, motion, soft
/// top-lit surfaces. No outer shadows: they would widen the island's input
/// area and block clicks on whatever is behind it.
const ISLAND_POLISH_CSS: &str = r#"
button {
    font-weight: 560;
    letter-spacing: -0.1px;
    transition: background-color 120ms ease-out, border-color 120ms ease-out, color 120ms ease-out;
}
button image { opacity: 0.92; }
button.seg-btn image, button.toggle-chip image { opacity: 0.85; }
button.seg-btn:hover image { opacity: 1; }

.island-shell, .island-shell-rec {
    background-image: linear-gradient(180deg, rgba(255, 255, 255, 0.05), rgba(255, 255, 255, 0) 46px);
    border-color: rgba(255, 255, 255, 0.11);
}
.island-shell-rec { border-color: rgba(244, 63, 94, 0.6); }
.island-title { font-weight: 650; letter-spacing: -0.15px; }
.island-timer-rec { font-weight: 650; letter-spacing: 0.2px; }
.island-countdown { color: #fbbf24; }
.theme-light .island-countdown { color: #b45309; }
button.pause-pill { padding: 4px 8px; }
button.pause-pill-on {
    padding: 4px 8px;
    background-color: rgba(251, 191, 36, 0.18);
    border-color: rgba(251, 191, 36, 0.6);
    color: #fde68a;
}
.theme-light button.pause-pill-on {
    background-color: rgba(245, 158, 11, 0.14);
    border-color: rgba(217, 119, 6, 0.55);
    color: #92400e;
}
.section-cap { font-size: 9px; font-weight: 650; letter-spacing: 0.9px; }
.panel-hint { font-weight: 500; }

.bento-card {
    background-image: linear-gradient(180deg, rgba(255, 255, 255, 0.028), rgba(255, 255, 255, 0) 60px);
    border-color: rgba(255, 255, 255, 0.07);
    border-radius: 13px;
}
.seg-track { border-radius: 10px; }
button.seg-btn { font-weight: 560; border-radius: 8px; }
button.seg-btn-active {
    font-weight: 620;
    background-image: linear-gradient(180deg, #6d70f5, #4f46e5);
    border-color: rgba(199, 210, 254, 0.35);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.18);
}
button.shutter-rec {
    font-weight: 620;
    background-image: linear-gradient(180deg, #f43f5e, #e11d48);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.22);
}
button.shutter-rec:hover { background-image: linear-gradient(180deg, #fb5a76, #e8264f); }
button.shutter-stop { font-weight: 620; padding: 4px 10px; }
button.action-chip, button.toggle-chip, button.toggle-chip-on { font-weight: 580; }
button.lens-pill, button.lens-pill-zoomed { font-weight: 600; }
button.close-pill { padding: 4px 6px; }
button.close-pill image { opacity: 0.6; }
button.close-pill:hover image { opacity: 1; }
.status-strip { font-weight: 500; }

/* Legibility: larger type and brighter secondary text on the dark theme. */
button { font-size: 12px; min-height: 26px; }
button.seg-btn, button.seg-btn-active { font-size: 12px; min-height: 26px; padding: 3px 8px; }
button.seg-btn { color: #c3c9d4; }
button.seg-btn:hover { color: #ffffff; }
button.action-chip, button.toggle-chip, button.toggle-chip-on { font-size: 12px; padding: 5px 10px; }
button.toggle-chip { color: #c3c9d4; }
button.action-chip { color: #dbe1ff; }
button.lens-pill, button.lens-pill-zoomed { font-size: 11.5px; }
.island-title, .island-timer-rec { font-size: 12.5px; }
.section-cap { font-size: 10.5px; color: #aab3c0; }
.panel-hint { font-size: 11.5px; color: #b8bfca; }
.coord-badge { font-size: 11px; }
.status-strip { font-size: 11.5px; color: #b8bfca; }
.bento-card { padding: 11px 12px; }

.theme-light button.seg-btn { color: #3f4b5c; }
.theme-light button.toggle-chip { color: #475569; }
.theme-light button.action-chip { color: #3730a3; }
.theme-light .section-cap { color: #5b6677; }
.theme-light .panel-hint, .theme-light .status-strip { color: #4b5563; }

.theme-light.island-shell, .theme-light.island-shell-rec {
    background-image: linear-gradient(180deg, rgba(255, 255, 255, 0.9), rgba(255, 255, 255, 0) 46px);
    border-color: rgba(15, 23, 42, 0.12);
}
.theme-light.island-shell-rec { border-color: rgba(225, 29, 72, 0.5); }
.theme-light .bento-card {
    background-image: none;
    border-color: rgba(15, 23, 42, 0.08);
}
.theme-light button.seg-btn {
    background-color: transparent;
    border-color: transparent;
}
.theme-light button.seg-btn:hover { background-color: rgba(15, 23, 42, 0.06); }
.theme-light button.seg-btn-active {
    background-image: linear-gradient(180deg, #6366f1, #4f46e5);
    border-color: rgba(67, 56, 202, 0.5);
}
"#;

/// A symbolic icon from the bundled set (`assets/icons`), recolored by GTK
/// to the widget's text color.
pub(crate) fn icon(name: &str, size: i32) -> Image {
    let image = Image::from_icon_name(&format!("hgsr-{name}-symbolic"));
    image.set_pixel_size(size);
    image
}

/// A button showing an icon followed by `label` (or just the icon).
pub(crate) fn icon_button(name: &str, label: &str) -> Button {
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.set_halign(Align::Center);
    row.append(&icon(name, 15));
    if !label.is_empty() {
        row.append(&Label::new(Some(label)));
    }
    let button = Button::new();
    button.set_child(Some(&row));
    button
}

/// Sets a button's text, keeping the icon of an `icon_button`.
pub(crate) fn set_btn_text(button: &Button, text: &str) {
    let label = button.child().and_then(|row| {
        let mut child = row.first_child();
        while let Some(widget) = child {
            if let Ok(label) = widget.clone().downcast::<Label>() {
                return Some(label);
            }
            child = widget.next_sibling();
        }
        None
    });
    match label {
        Some(label) => label.set_text(text),
        None => button.set_label(text),
    }
}

/// The newest saved recording in `dir`.
fn latest_recording(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            name.starts_with("HyperGSR_") && name.ends_with(".mp4") && !name.ends_with(".capture.mp4")
        })
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

/// Renders `widget` (inside `window`) to a PNG, for screenshots in tests
/// and documentation.
pub(crate) fn save_widget_png(widget: &impl IsA<gtk4::Widget>, window: &impl IsA<gtk4::Native>, path: &str) {
    let paintable = gtk4::WidgetPaintable::new(Some(widget));
    let w = widget.width().max(1);
    let h = widget.height().max(1);
    let snap = gtk4::Snapshot::new();
    gdk4::prelude::PaintableExt::snapshot(&paintable, &snap, w as f64, h as f64);
    if let Some(node) = snap.to_node() {
        if let Some(renderer) = gtk4::prelude::NativeExt::renderer(window) {
            let rect = gtk4::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
            let tex = gtk4::prelude::GskRendererExt::render_texture(&renderer, &node, Some(&rect));
            let _ = gdk4::prelude::TextureExt::save_to_png(&tex, path);
        }
    }
}

/// Shows Pause, or Continue while paused.
fn sync_pause_button(button: &Button, image: &Image, paused: bool) {
    let shortcut = crate::shortcuts::PAUSE_LABEL;
    if paused {
        image.set_icon_name(Some("hgsr-play-symbolic"));
        button.set_tooltip_text(Some(&format!("Continue recording ({shortcut})")));
        button.remove_css_class("pause-pill");
        button.add_css_class("pause-pill-on");
    } else {
        image.set_icon_name(Some("hgsr-pause-symbolic"));
        button.set_tooltip_text(Some(&format!("Pause ({shortcut}); the pause is cut from the video")));
        button.remove_css_class("pause-pill-on");
        button.add_css_class("pause-pill");
    }
}

pub(crate) fn set_active_segment(buttons: &[Button], active_idx: usize) {
    for (idx, btn) in buttons.iter().enumerate() {
        if idx == active_idx {
            btn.remove_css_class("seg-btn");
            btn.add_css_class("seg-btn-active");
        } else {
            btn.remove_css_class("seg-btn-active");
            btn.add_css_class("seg-btn");
        }
    }
}

pub(crate) fn draw_rounded_rect(cr: &gtk4::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w * 0.5).min(h * 0.5);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        3.0 * std::f64::consts::FRAC_PI_2,
    );
    cr.close_path();
}

fn build_dynamic_island(app: &Application, state: SharedStudioState) {
    load_dynamic_island_css();
    if let (Some(display), Some(dir)) = (gdk4::Display::default(), crate::bundled::symbolic_icon_dir()) {
        gtk4::IconTheme::for_display(&display).add_search_path(dir);
    }

    let is_light_mode = Arc::new(AtomicBool::new(false));
    if std::env::var("HYPERGSR_THEME").as_deref() == Ok("light") {
        is_light_mode.store(true, Ordering::Relaxed);
    }
    let start_expanded = std::env::var("HYPERGSR_EXPANDED").as_deref() == Ok("1");

    let window = ApplicationWindow::builder()
        .application(app)
        .title(ISLAND_WINDOW_TITLE)
        .default_width(1)
        .default_height(1)
        .decorated(false)
        .resizable(false)
        .build();

    let layer_shell = init_wayland_layer_shell(&window);
    if layer_shell.is_none() {
        crate::x11_above::keep_above(&window);
    }

    let shell_box = GtkBox::new(Orientation::Vertical, 0);
    shell_box.add_css_class("island-shell");
    shell_box.set_halign(Align::Center);
    if is_light_mode.load(Ordering::Relaxed) {
        shell_box.add_css_class("theme-light");
    }

    // =========================================================================
    // COMPACT DYNAMIC ISLAND BAR (Tight ~360px Pill — Zero Empty Dead Space)
    // =========================================================================
    let top_row = GtkBox::new(Orientation::Horizontal, 5);
    top_row.add_css_class("island-top-row");
    top_row.set_valign(Align::Center);

    // 1. Left Brand & Waveform Pill
    let brand_box = GtkBox::new(Orientation::Horizontal, 5);
    brand_box.add_css_class("brand-pill");
    brand_box.set_valign(Align::Center);

    let logo_img = Image::from_file(asset_path("hypergsr-logo.svg"));
    logo_img.set_pixel_size(16);

    let wave_indicator = DrawingArea::new();
    wave_indicator.set_content_width(22);
    wave_indicator.set_content_height(13);
    {
        let st = state.clone();
        wave_indicator.set_draw_func(move |_, cr, _w, h| {
            let Some(g) = st.try_lock() else { return };
            let is_rec = g.encoder_session.is_some();
            let t = g.now_ms() as f64 / 1000.0;
            let zoom = g.last_viewport.zoom as f64;
            drop(g);

            for i in 0..4 {
                let x = 1.5 + i as f64 * 5.0;
                let phase = t * 7.5 + i as f64 * 1.15;
                let amp = if is_rec {
                    0.35 + 0.65 * phase.sin().abs()
                } else if zoom > 1.06 {
                    0.30 + 0.60 * (phase * 1.2).cos().abs()
                } else {
                    0.30
                };
                let bar_h = (h as f64 * amp).clamp(3.0, h as f64 - 2.0);
                let y = (h as f64 - bar_h) * 0.5;

                if is_rec {
                    cr.set_source_rgb(0.95, 0.24, 0.38);
                } else if zoom > 1.06 {
                    cr.set_source_rgb(0.08, 0.75, 0.52);
                } else {
                    cr.set_source_rgb(0.38, 0.40, 0.94);
                }
                draw_rounded_rect(cr, x, y, 2.8, bar_h, 1.4);
                cr.fill().ok();
            }
        });
    }

    let status_lbl = Label::new(Some("HyperGSR"));
    status_lbl.add_css_class("island-title");

    brand_box.append(&logo_img);
    brand_box.append(&wave_indicator);
    brand_box.append(&status_lbl);

    // 2. Fixed-Width Live Auto-Zoom Pill (Click: Toggle Manual Lock | Scroll: Dial Zoom | Right-Click: Reset 1.0x)
    let zoom_pill_btn = Button::new();
    zoom_pill_btn.add_css_class("lens-pill");
    zoom_pill_btn.set_tooltip_text(Some(
        "Click: Toggle Manual Zoom Lock · Scroll Wheel: Dial Zoom (1.0×–3.2×) · Right-Click: Reset 1.0×",
    ));
    let zoom_pill_box = GtkBox::new(Orientation::Horizontal, 4);
    let zoom_icon = Image::from_file(asset_path("icon-autozoom.svg"));
    zoom_icon.set_pixel_size(12);
    let zoom_pill_lbl = Label::new(Some("2.0× Auto"));
    zoom_pill_box.append(&zoom_icon);
    zoom_pill_box.append(&zoom_pill_lbl);
    zoom_pill_btn.set_child(Some(&zoom_pill_box));

    // Flexible spacer so when expanded, left and right controls stay flush with the edges
    let top_spacer = GtkBox::new(Orientation::Horizontal, 0);
    top_spacer.set_hexpand(true);

    // 3. Shutter Record / Stop Button
    let rec_btn = Button::new();
    rec_btn.add_css_class("shutter-rec");
    rec_btn.set_tooltip_text(Some("Start / Stop 60 FPS Zero-Copy GPU Recording"));
    let rec_btn_box = GtkBox::new(Orientation::Horizontal, 5);
    let rec_icon = Image::from_file(asset_path("icon-record.svg"));
    rec_icon.set_pixel_size(12);
    let rec_btn_lbl = Label::new(Some("Record"));
    rec_btn_box.append(&rec_icon);
    rec_btn_box.append(&rec_btn_lbl);
    rec_btn.set_child(Some(&rec_btn_box));

    // Shown while a recording is paused because its screen share broke.
    let resume_btn = Button::with_label("Resume");
    resume_btn.add_css_class("shutter-rec");
    resume_btn.set_visible(false);

    // Pauses the running recording; the paused time is cut from the video.
    let pause_btn = Button::new();
    let pause_icon = icon("pause", 14);
    pause_btn.set_child(Some(&pause_icon));
    pause_btn.add_css_class("pause-pill");
    pause_btn.set_tooltip_text(Some(&format!("Pause ({})", crate::shortcuts::PAUSE_LABEL)));
    pause_btn.set_visible(false);

    // Instant replay: starts the buffer, then saves clips of the last seconds.
    let replay_btn = icon_button("replay", "Replay");
    replay_btn.add_css_class("studio-pill");
    replay_btn.set_tooltip_text(Some("Start instant replay: keeps the last seconds so you can save them as a clip"));
    replay_btn.set_visible(state.lock().settings.replay_sec > 0);
    let clip_btn = icon_button("replay", "Save clip");
    clip_btn.add_css_class("shutter-rec");
    clip_btn.set_visible(false);
    let replay_stop_btn = Button::new();
    let replay_stop_icon = Image::from_file(asset_path("icon-stop.svg"));
    replay_stop_icon.set_pixel_size(12);
    replay_stop_btn.set_child(Some(&replay_stop_icon));
    replay_stop_btn.add_css_class("shutter-stop");
    replay_stop_btn.set_tooltip_text(Some("Stop instant replay"));
    replay_stop_btn.set_visible(false);

    // Opens the editor on the latest recording.
    let edit_btn = icon_button("trim", "Edit");
    edit_btn.add_css_class("studio-pill");
    edit_btn.set_tooltip_text(Some("Trim the latest recording, or export it vertical, square, WebM, or GIF"));
    edit_btn.set_visible(latest_recording(&state.lock().videos_dir).is_some());

    // 4. Quick Dark / Light Theme Pill
    let theme_btn = Button::new();
    theme_btn.add_css_class("theme-pill");
    theme_btn.set_tooltip_text(Some("Switch between Obsidian Dark & Pearl Light"));
    let theme_icon = Image::from_file(if is_light_mode.load(Ordering::Relaxed) {
        asset_path("icon-sun.svg")
    } else {
        asset_path("icon-theme.svg")
    });
    theme_icon.set_pixel_size(13);
    theme_btn.set_child(Some(&theme_icon));

    // 5. Compact Studio Drawer Toggle Pill
    let expand_btn = Button::with_label(if start_expanded {
        "Studio ▴"
    } else {
        "Studio ▾"
    });
    expand_btn.add_css_class(if start_expanded {
        "studio-pill-open"
    } else {
        "studio-pill"
    });

    // 6. Quit Button
    let close_btn = icon_button("close", "");
    close_btn.add_css_class("close-pill");
    close_btn.set_tooltip_text(Some("Quit HyperGSR"));

    top_row.append(&brand_box);
    top_row.append(&zoom_pill_btn);
    top_row.append(&top_spacer);
    top_row.append(&resume_btn);
    top_row.append(&pause_btn);
    top_row.append(&rec_btn);
    top_row.append(&replay_btn);
    top_row.append(&clip_btn);
    top_row.append(&replay_stop_btn);
    top_row.append(&edit_btn);
    top_row.append(&theme_btn);
    top_row.append(&expand_btn);
    top_row.append(&close_btn);

    shell_box.append(&top_row);

    // =========================================================================
    // COMPACT VERTICAL STUDIO POPOVER (Exact same ~372px width as Top Bar!)
    // Uses `set_visible(false)` when collapsed so GTK4 shrinks width to the pill!
    // =========================================================================
    let drawer_box = GtkBox::new(Orientation::Vertical, 7);
    drawer_box.add_css_class("island-drawer");
    drawer_box.set_size_request(404, -1);
    drawer_box.set_visible(start_expanded);

    // -------------------------------------------------------------------------
    // CARD 1: Live 16:9 Cursor Camera Viewfinder & Spring Curve
    // -------------------------------------------------------------------------
    let vis_card = GtkBox::new(Orientation::Vertical, 6);
    vis_card.add_css_class("bento-card");

    let vis_hdr = GtkBox::new(Orientation::Horizontal, 6);
    let gpu_icon = Image::from_file(asset_path("icon-gpu.svg"));
    gpu_icon.set_pixel_size(12);
    let vis_title = Label::new(Some("LIVE CURSOR CAMERA"));
    vis_title.add_css_class("section-cap");
    vis_title.set_hexpand(true);
    vis_title.set_halign(Align::Start);

    let coord_lbl = Label::new(Some("DRM #92 · (960, 540)"));
    coord_lbl.add_css_class("coord-badge");

    vis_hdr.append(&gpu_icon);
    vis_hdr.append(&vis_title);
    vis_hdr.append(&coord_lbl);
    vis_card.append(&vis_hdr);

    let radar_area = DrawingArea::new();
    radar_area.set_content_width(382);
    radar_area.set_content_height(126);

    let zoom_history: std::rc::Rc<std::cell::RefCell<VecDeque<f32>>> =
        std::rc::Rc::new(std::cell::RefCell::new(VecDeque::from(vec![1.0; 72])));

    let motion_ctrl = EventControllerMotion::new();
    {
        let st = state.clone();
        let area_ref = radar_area.clone();
        motion_ctrl.connect_motion(move |_, x, y| {
            let w = area_ref.width().max(1) as f32;
            let h = area_ref.height().max(1) as f32;
            if let Some(mut g) = st.try_lock() {
                g.set_interactive_cursor(x as f32 / w, y as f32 / h, false, false);
            }
        });
    }
    radar_area.add_controller(motion_ctrl);

    let click_ctrl = GestureClick::new();
    click_ctrl.set_button(0);
    {
        let st = state.clone();
        let area_ref = radar_area.clone();
        click_ctrl.connect_pressed(move |gesture, _n, x, y| {
            let w = area_ref.width().max(1) as f32;
            let h = area_ref.height().max(1) as f32;
            let btn = gesture.current_button();
            if let Some(mut g) = st.try_lock() {
                g.set_interactive_cursor(x as f32 / w, y as f32 / h, btn == 1, btn == 3);
            }
        });
    }
    radar_area.add_controller(click_ctrl);

    {
        let st = state.clone();
        let z_hist = zoom_history.clone();
        let light_flag = is_light_mode.clone();
        radar_area.set_draw_func(move |_area, cr, width, height| {
            let Some(g) = st.try_lock() else { return };
            let w = width as f64;
            let h = height as f64;
            let screen_h = h - 28.0;
            let light = light_flag.load(Ordering::Relaxed);

            let vp = g.last_viewport;
            let cur = g.last_cursor;
            let now_ms = g.now_ms();
            let ripples: Vec<_> = g.cursor_tracker.active_ripples().to_vec();
            drop(g);

            // 1. Rounded 16:9 Screen Canvas
            draw_rounded_rect(cr, 0.0, 0.0, w, screen_h, 9.0);
            if light {
                cr.set_source_rgb(0.94, 0.96, 0.985);
            } else {
                cr.set_source_rgb(0.035, 0.038, 0.055);
            }
            cr.fill().ok();

            // Window silhouettes
            if light {
                cr.set_source_rgba(0.06, 0.09, 0.16, 0.06);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.035);
            }
            draw_rounded_rect(cr, w * 0.07, screen_h * 0.14, w * 0.46, screen_h * 0.68, 4.0);
            cr.fill().ok();
            draw_rounded_rect(cr, w * 0.57, screen_h * 0.18, w * 0.36, screen_h * 0.60, 4.0);
            cr.fill().ok();

            // Rule-of-thirds grid
            if light {
                cr.set_source_rgba(0.06, 0.09, 0.16, 0.06);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.04);
            }
            cr.set_line_width(1.0);
            for i in 1..=2 {
                let gx = w * (i as f64 / 3.0);
                let gy = screen_h * (i as f64 / 3.0);
                cr.move_to(gx, 3.0);
                cr.line_to(gx, screen_h - 3.0);
                cr.move_to(3.0, gy);
                cr.line_to(w - 3.0, gy);
            }
            cr.stroke().ok();

            // 2. Virtual Auto-Zoom Camera Crop Box
            let vx = (vp.uv_min_x as f64 * w).clamp(1.0, w - 6.0);
            let vy = (vp.uv_min_y as f64 * screen_h).clamp(1.0, screen_h - 6.0);
            let vw = (vp.uv_w as f64 * w).clamp(8.0, w - vx - 1.0);
            let vh = (vp.uv_h as f64 * screen_h).clamp(8.0, screen_h - vy - 1.0);

            if vp.zoom > 1.06 {
                cr.set_source_rgba(0.06, 0.72, 0.50, if light { 0.16 } else { 0.14 });
                draw_rounded_rect(cr, vx, vy, vw, vh, 5.0);
                cr.fill().ok();

                cr.set_source_rgba(0.06, 0.76, 0.54, 0.95);
                cr.set_line_width(1.6);
                draw_rounded_rect(cr, vx, vy, vw, vh, 5.0);
                cr.stroke().ok();
            } else {
                cr.set_source_rgba(0.38, 0.40, 0.95, 0.45);
                cr.set_line_width(1.1);
                draw_rounded_rect(cr, vx + 1.0, vy + 1.0, vw - 2.0, vh - 2.0, 7.0);
                cr.stroke().ok();
            }

            // 3. Click Ripples
            for ripple in &ripples {
                let p = ((now_ms.saturating_sub(ripple.start_ms)) as f64 / 650.0).clamp(0.0, 1.0);
                let rx = ripple.x as f64 * w;
                let ry = ripple.y as f64 * screen_h;
                cr.set_source_rgba(0.06, 0.76, 0.54, (1.0 - p) * 0.9);
                cr.set_line_width(1.8);
                cr.arc(rx, ry, 3.0 + p * 16.0, 0.0, std::f64::consts::TAU);
                cr.stroke().ok();
            }

            // 4. Live Hardware Cursor Dot & Focus Halo
            let cx = (cur.smooth_x as f64 * w).clamp(4.0, w - 4.0);
            let cy = (cur.smooth_y as f64 * screen_h).clamp(4.0, screen_h - 4.0);

            cr.set_source_rgba(0.06, 0.76, 0.54, 0.30);
            cr.arc(cx, cy, 8.0, 0.0, std::f64::consts::TAU);
            cr.fill().ok();

            if light {
                cr.set_source_rgba(0.06, 0.09, 0.16, 0.95);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.98);
            }
            cr.arc(cx, cy, 3.4, 0.0, std::f64::consts::TAU);
            cr.fill().ok();

            // 5. Bottom Spring Zoom Curve Strip
            let strip_y = screen_h + 4.0;
            let strip_h = h - strip_y;
            draw_rounded_rect(cr, 0.0, strip_y, w, strip_h, 6.0);
            if light {
                cr.set_source_rgb(0.91, 0.94, 0.97);
            } else {
                cr.set_source_rgb(0.045, 0.048, 0.07);
            }
            cr.fill().ok();

            let hist = z_hist.borrow();
            if hist.len() >= 2 {
                cr.set_source_rgba(0.31, 0.27, 0.90, 0.95);
                cr.set_line_width(1.6);
                for (idx, &z_val) in hist.iter().enumerate() {
                    let px = (idx as f64 / (hist.len() - 1).max(1) as f64) * (w - 8.0) + 4.0;
                    let norm_z = ((z_val as f64 - 1.0) / 1.65).clamp(0.0, 1.0);
                    let py = strip_y + strip_h - 3.5 - norm_z * (strip_h - 7.0);
                    if idx == 0 {
                        cr.move_to(px, py);
                    } else {
                        cr.line_to(px, py);
                    }
                }
                cr.stroke().ok();
            }
        });
    }
    vis_card.append(&radar_area);

    let footer_lbl = Label::new(Some(&format!(
        "Ready · recordings go to {}",
        crate::studio_panel::display_dir(&state.lock().settings.recordings_dir())
    )));
    footer_lbl.set_hexpand(true);
    footer_lbl.set_halign(Align::Start);
    // Long folder names and messages wrap instead of widening the island.
    footer_lbl.set_wrap(true);
    footer_lbl.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    footer_lbl.set_lines(2);
    footer_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    footer_lbl.set_max_width_chars(34);
    footer_lbl.set_xalign(0.0);

    let vis_btn_row = GtkBox::new(Orientation::Horizontal, 6);
    let test_zoom_btn = icon_button("lock", "Lock zoom");
    test_zoom_btn.add_css_class("action-chip");
    test_zoom_btn.set_hexpand(true);
    let pulse_zoom_btn = icon_button("pulse", "Pulse");
    let reset_zoom_btn = icon_button("wide", "1.0×");
    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        test_zoom_btn.connect_clicked(move |btn| {
            let mut g = st.lock();
            let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
            let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
            let target_z = g.zoom_cfg.target_zoom;
            let locked = g.zoom_engine.toggle_manual_lock(cx, cy, target_z);
            g.tick();
            drop(g);
            if locked {
                set_btn_text(btn, "Unlock zoom");
                f_lbl.set_text(&format!("Manual Zoom Locked at {target_z:.1}×"));
            } else {
                set_btn_text(btn, "Lock zoom");
                f_lbl.set_text("Manual Zoom Unlocked (1.0× Wide)");
            }
            r_area.queue_draw();
        });
    }
    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        let lock_b = test_zoom_btn.clone();
        pulse_zoom_btn.connect_clicked(move |_| {
            let mut g = st.lock();
            let now = g.now_ms();
            let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
            let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
            let target_z = g.zoom_cfg.target_zoom;
            let cur_cfg = g.cursor_cfg.clone();
            g.cursor_tracker.update(now, cx, cy, true, false, &cur_cfg);
            g.zoom_engine
                .trigger_manual_zoom(cx, cy, target_z, now, 1650);
            g.tick();
            drop(g);
            set_btn_text(&lock_b, "Lock zoom");
            f_lbl.set_text(&format!("Triggered 1.7s Auto-Zoom Pulse ({target_z:.1}×)"));
            r_area.queue_draw();
        });
    }
    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        let lock_b = test_zoom_btn.clone();
        zoom_pill_btn.connect_clicked(move |_| {
            let mut g = st.lock();
            let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
            let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
            let target_z = g.zoom_cfg.target_zoom;
            let locked = g.zoom_engine.toggle_manual_lock(cx, cy, target_z);
            g.tick();
            drop(g);
            if locked {
                set_btn_text(&lock_b, "Unlock zoom");
                f_lbl.set_text(&format!("Manual Zoom Locked at {target_z:.1}×"));
            } else {
                set_btn_text(&lock_b, "Lock zoom");
                f_lbl.set_text("Manual Zoom Unlocked (1.0× Wide)");
            }
            r_area.queue_draw();
        });
    }
    // Right-click on Top-Bar Zoom Pill -> Reset immediately to 1.0x Wide
    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        let lock_b = test_zoom_btn.clone();
        let pill_rclick = GestureClick::new();
        pill_rclick.set_button(3);
        pill_rclick.connect_pressed(move |_, _, _, _| {
            let mut g = st.lock();
            g.zoom_engine.release_manual_lock();
            g.tick();
            drop(g);
            set_btn_text(&lock_b, "Lock zoom");
            f_lbl.set_text("↺ Reset Camera to 1.0× Wide");
            r_area.queue_draw();
        });
        zoom_pill_btn.add_controller(pill_rclick);
    }
    // Mouse Scroll Wheel on Top-Bar Zoom Pill -> Smoothly dial manual zoom (1.0x .. 3.2x)
    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        let scroll_ctrl = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
        scroll_ctrl.connect_scroll(move |_, _dx, dy| {
            let mut g = st.lock();
            let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
            let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
            let step = if dy < 0.0 { 0.20 } else { -0.20 };
            let new_z = g.zoom_engine.adjust_manual_zoom_wheel(cx, cy, step);
            g.tick();
            drop(g);
            f_lbl.set_text(&format!("Wheel Zoom -> {new_z:.1}×"));
            r_area.queue_draw();
            glib::Propagation::Stop
        });
        zoom_pill_btn.add_controller(scroll_ctrl);
    }
    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        let lock_b = test_zoom_btn.clone();
        reset_zoom_btn.connect_clicked(move |_| {
            let mut g = st.lock();
            g.zoom_engine.release_manual_lock();
            g.tick();
            drop(g);
            set_btn_text(&lock_b, "Lock zoom");
            f_lbl.set_text("↺ Reset Camera to 1.0× Wide");
            r_area.queue_draw();
        });
    }
    vis_btn_row.append(&test_zoom_btn);
    vis_btn_row.append(&pulse_zoom_btn);
    vis_btn_row.append(&reset_zoom_btn);
    vis_card.append(&vis_btn_row);

    // The drawer is split into tabs so each page fits on screen; the page
    // area still scrolls on screens too short for the tallest one.
    let drawer_stack = gtk4::Stack::builder()
        .vhomogeneous(false)
        .transition_type(gtk4::StackTransitionType::Crossfade)
        .transition_duration(120)
        .build();
    let drawer_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .child(&drawer_stack)
        .build();
    // Sizes the page area to the visible page, capped to the screen. GTK's
    // natural-height propagation does not work inside a layer surface, so
    // the height is set explicitly.
    let fit_drawer: Rc<dyn Fn()> = {
        let stack = drawer_stack.clone();
        let scroll = drawer_scroll.clone();
        let win = window.clone();
        Rc::new(move || {
            // Measure the page being shown, not the stack: mid-transition the
            // stack still reports the previous page's size.
            let Some(page) = stack.visible_child() else { return };
            let width = if stack.width() > 0 { stack.width() } else { -1 };
            let (_, natural, _, _) = page.measure(Orientation::Vertical, width);
            // Room for the top bar, the tabs, the footer, and the island margins.
            let available = screen_height(&win) - 170;
            let height = natural.min(available).max(120);
            // GTK refuses a minimum above the current maximum (and the
            // reverse), so widen the range before narrowing it to `height`.
            scroll.set_min_content_height(-1);
            scroll.set_max_content_height(height);
            scroll.set_min_content_height(height);
            // Let the surface shrink when a shorter page is shown.
            win.set_default_size(1, 1);
        })
    };
    let tab_track = GtkBox::new(Orientation::Horizontal, 3);
    tab_track.add_css_class("seg-track");
    let tabs: Vec<(Button, &str)> = [("record", "Record", "record"), ("camera", "Camera & Zoom", "camera")]
        .into_iter()
        .map(|(icon_name, label, name)| {
            let b = icon_button(icon_name, label);
            b.set_hexpand(true);
            tab_track.append(&b);
            (b, name)
        })
        .collect();
    let tab_btns: Vec<Button> = tabs.iter().map(|(b, _)| b.clone()).collect();
    set_active_segment(&tab_btns, 0);
    for (idx, (btn, name)) in tabs.iter().enumerate() {
        let stack = drawer_stack.clone();
        let all = tab_btns.clone();
        let fit = fit_drawer.clone();
        let name = name.to_string();
        btn.connect_clicked(move |_| {
            stack.set_visible_child_name(&name);
            set_active_segment(&all, idx);
            fit();
        });
    }
    drawer_box.append(&tab_track);
    drawer_box.append(&drawer_scroll);

    drawer_stack.add_named(&crate::studio_panel::build(&state, &footer_lbl), Some("record"));
    // The live camera sits with the zoom and theme controls it previews.
    let camera_page = GtkBox::new(Orientation::Vertical, 7);
    camera_page.append(&vis_card);
    drawer_stack.add_named(&camera_page, Some("camera"));

    // -------------------------------------------------------------------------
    // CARD 2: Compact Segmented Controls (Theme, Camera Mode, Depth, Spring)
    // -------------------------------------------------------------------------
    let ctrl_card = GtkBox::new(Orientation::Vertical, 6);
    ctrl_card.add_css_class("bento-card");

    // Theme + Camera Mode in tight segmented tracks
    let app_cap = Label::new(Some("THEME & CAMERA MODE"));
    app_cap.add_css_class("section-cap");
    app_cap.set_halign(Align::Start);
    ctrl_card.append(&app_cap);

    let app_track = GtkBox::new(Orientation::Horizontal, 3);
    app_track.add_css_class("seg-track");
    let t_dark = icon_button("moon", "Obsidian");
    let t_light = icon_button("sun", "Pearl");
    t_dark.set_hexpand(true);
    t_light.set_hexpand(true);
    let theme_seg_btns = vec![t_dark.clone(), t_light.clone()];
    set_active_segment(
        &theme_seg_btns,
        if is_light_mode.load(Ordering::Relaxed) {
            1
        } else {
            0
        },
    );
    app_track.append(&t_dark);
    app_track.append(&t_light);
    ctrl_card.append(&app_track);

    let mode_track = GtkBox::new(Orientation::Horizontal, 3);
    mode_track.add_css_class("seg-track");
    let m_follow = Button::with_label("Auto + Combo");
    let m_combo_only = Button::with_label("Combo Only");
    let m_click = Button::with_label("Click Zoom");
    for b in [&m_follow, &m_combo_only, &m_click] {
        b.set_hexpand(true);
    }
    let mode_btns = vec![m_follow.clone(), m_combo_only.clone(), m_click.clone()];
    set_active_segment(&mode_btns, 0);
    {
        let st = state.clone();
        let btns = mode_btns.clone();
        let f_lbl = footer_lbl.clone();
        m_follow.connect_clicked(move |_| {
            st.lock().zoom_cfg.mode = AutoZoomMode::SmartCursorFollow;
            set_active_segment(&btns, 0);
            f_lbl.set_text("✓ Camera Mode: Auto + Combo");
        });
    }
    {
        let st = state.clone();
        let btns = mode_btns.clone();
        let f_lbl = footer_lbl.clone();
        m_combo_only.connect_clicked(move |_| {
            st.lock().zoom_cfg.mode = AutoZoomMode::Manual;
            set_active_segment(&btns, 1);
            f_lbl.set_text("✓ Camera Mode: Manual Combo Only");
        });
    }
    {
        let st = state.clone();
        let btns = mode_btns.clone();
        let f_lbl = footer_lbl.clone();
        m_click.connect_clicked(move |_| {
            st.lock().zoom_cfg.mode = AutoZoomMode::ClickToZoom;
            set_active_segment(&btns, 2);
            f_lbl.set_text("✓ Camera Mode: Click-to-Zoom");
        });
    }
    mode_track.append(&m_follow);
    mode_track.append(&m_combo_only);
    mode_track.append(&m_click);
    ctrl_card.append(&mode_track);

    // -------------------------------------------------------------------------
    // MANUAL ZOOM COMBO & LIVE SELF-REGISTRATION ("Record Combo…")
    // -------------------------------------------------------------------------
    let combo_cap = Label::new(Some("MANUAL ZOOM COMBO (MOUSE / KEY / SHAKE)"));
    combo_cap.add_css_class("section-cap");
    combo_cap.set_halign(Align::Start);
    ctrl_card.append(&combo_cap);

    let combo_track = GtkBox::new(Orientation::Horizontal, 3);
    combo_track.add_css_class("seg-track");
    let c_shake_mid = icon_button("shake", "Shake");
    let c_mid = icon_button("mouse", "Middle");
    let c_ctrl = icon_button("ctrl-click", "Ctrl+Click");
    let c_dbl = icon_button("double-click", "Double");
    for b in [&c_shake_mid, &c_mid, &c_ctrl, &c_dbl] {
        b.set_hexpand(true);
    }
    let combo_preset_btns = vec![
        c_shake_mid.clone(),
        c_mid.clone(),
        c_ctrl.clone(),
        c_dbl.clone(),
    ];
    set_active_segment(&combo_preset_btns, 0);

    let record_combo_btn = icon_button("keyboard", "Record combo…");
    record_combo_btn.set_hexpand(true);
    record_combo_btn.set_tooltip_text(Some(
        "Click, then perform ANY mouse click (Middle/Right/Side/Ctrl+Click), key press, or cursor shake to bind!",
    ));

    let combo_action_btn = icon_button("toggle", "Toggle lock");
    combo_action_btn.set_tooltip_text(Some(
        "Switch how your combo behaves: Toggle Lock (stay zoomed as long as you want), Hold to Zoom, or Step 1.8×/2.5×",
    ));

    let learning_combo = Arc::new(AtomicBool::new(false));

    for (idx, (btn, binding)) in [
        (c_shake_mid.clone(), ZoomComboBinding::ShakeOrMiddleClick),
        (c_mid.clone(), ZoomComboBinding::MiddleClick),
        (c_ctrl.clone(), ZoomComboBinding::CtrlClick),
        (c_dbl.clone(), ZoomComboBinding::DoubleClick),
    ]
    .into_iter()
    .enumerate()
    {
        let st = state.clone();
        let btns = combo_preset_btns.clone();
        let rec_b = record_combo_btn.clone();
        let learn_flag = learning_combo.clone();
        let f_lbl = footer_lbl.clone();
        btn.connect_clicked(move |_| {
            learn_flag.store(false, Ordering::Relaxed);
            let short = binding.short_label();
            st.lock().zoom_cfg.combo_binding = binding.clone();
            set_active_segment(&btns, idx);
            set_btn_text(&rec_b, "Record combo…");
            rec_b.remove_css_class("action-chip");
            f_lbl.set_text(&format!("✓ Zoom Combo: {short}"));
        });
    }

    {
        let learn_flag = learning_combo.clone();
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        record_combo_btn.connect_clicked(move |btn| {
            let now_learning = !learn_flag.load(Ordering::Relaxed);
            learn_flag.store(now_learning, Ordering::Relaxed);
            if now_learning {
                set_btn_text(btn, "● Press Key, Click, or Shake…");
                btn.add_css_class("action-chip");
                f_lbl.set_text("Listening: Press any key/combo, Mid/Right-click, or Shake cursor!");
            } else {
                let lbl = st.lock().zoom_cfg.combo_binding.short_label();
                set_btn_text(btn, &lbl);
                f_lbl.set_text(&format!("✓ Zoom Combo: {lbl}"));
            }
        });
    }

    // Window-level mouse click listener for "Record Combo…" AND live custom mouse combo triggering
    {
        let learn_flag = learning_combo.clone();
        let st = state.clone();
        let btns = combo_preset_btns.clone();
        let rec_b = record_combo_btn.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        let bind_click = GestureClick::new();
        bind_click.set_button(0);
        bind_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
        bind_click.connect_pressed(move |gesture, n_press, _, _| {
            let btn_num = gesture.current_button();
            let mods = gesture.current_event_state();
            let ctrl = mods.contains(gdk4::ModifierType::CONTROL_MASK);
            let alt = mods.contains(gdk4::ModifierType::ALT_MASK);
            let shift = mods.contains(gdk4::ModifierType::SHIFT_MASK);
            let super_key = mods.contains(gdk4::ModifierType::SUPER_MASK);

            if !learn_flag.load(Ordering::Relaxed) {
                // Allow middle-click or modifier+click on the window to trigger manual zoom
                if btn_num == 2 || (btn_num == 1 && ctrl) {
                    let mut g = st.lock();
                    let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
                    let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
                    let tz = g.zoom_cfg.target_zoom;
                    let locked = g.zoom_engine.toggle_manual_lock(cx, cy, tz);
                    g.tick();
                    drop(g);
                    f_lbl.set_text(if locked {
                        "Manual Zoom Locked"
                    } else {
                        "Manual Zoom Unlocked"
                    });
                    r_area.queue_draw();
                }
                return;
            }

            // Plain unmodified single left-click is just clicking the button to toggle learning mode
            if btn_num == 1 && !ctrl && !alt && !shift && !super_key && n_press < 2 {
                return;
            }

            let new_binding = if n_press >= 2 && btn_num == 1 && !ctrl && !alt && !shift {
                ZoomComboBinding::DoubleClick
            } else {
                let mapped_btn = match btn_num {
                    1 => 1u8,
                    2 => 2u8,
                    3 => 3u8,
                    8 | 9 => 4u8,
                    _ => 2u8,
                };
                let mut parts = Vec::new();
                if ctrl {
                    parts.push("⌃ Ctrl");
                }
                if alt {
                    parts.push("⌥ Alt");
                }
                if shift {
                    parts.push("⇧ Shift");
                }
                if super_key {
                    parts.push("⌘ Super");
                }
                parts.push(match mapped_btn {
                    1 => "Click",
                    2 => "Middle-click",
                    3 => "Right-click",
                    _ => "Side button",
                });
                let label = parts.join(" + ");
                ZoomComboBinding::Custom {
                    ctrl,
                    alt,
                    shift,
                    super_key,
                    mouse_button: mapped_btn,
                    keycode: 0,
                    shake: false,
                    label,
                }
            };

            let lbl = new_binding.short_label();
            st.lock().zoom_cfg.combo_binding = new_binding;
            learn_flag.store(false, Ordering::Relaxed);
            for b in &btns {
                b.remove_css_class("seg-btn-active");
                b.add_css_class("seg-btn");
            }
            set_btn_text(&rec_b, &lbl);
            rec_b.add_css_class("action-chip");
            f_lbl.set_text(&format!("✓ Registered Custom Combo: {lbl}"));
        });
        window.add_controller(bind_click);
    }

    // Window-level keyboard listener for "Record Combo…" AND live keyboard combo triggering
    {
        let learn_flag = learning_combo.clone();
        let st = state.clone();
        let btns = combo_preset_btns.clone();
        let rec_b = record_combo_btn.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |_, keyval, hw_keycode, mods| {
            let key_name = keyval.name().map(|s| s.to_string()).unwrap_or_default();
            // Ignore bare modifier presses while waiting for the combo
            if matches!(
                key_name.as_str(),
                "Control_L"
                    | "Control_R"
                    | "Shift_L"
                    | "Shift_R"
                    | "Alt_L"
                    | "Alt_R"
                    | "Super_L"
                    | "Super_R"
                    | "Meta_L"
                    | "Meta_R"
            ) {
                return glib::Propagation::Proceed;
            }

            let ctrl = mods.contains(gdk4::ModifierType::CONTROL_MASK);
            let alt = mods.contains(gdk4::ModifierType::ALT_MASK);
            let shift = mods.contains(gdk4::ModifierType::SHIFT_MASK);
            let super_key = mods.contains(gdk4::ModifierType::SUPER_MASK);
            let kc = hw_keycode;

            if learn_flag.load(Ordering::Relaxed) {
                let mut parts = Vec::new();
                if ctrl {
                    parts.push("⌃ Ctrl".to_string());
                }
                if alt {
                    parts.push("⌥ Alt".to_string());
                }
                if shift {
                    parts.push("⇧ Shift".to_string());
                }
                if super_key {
                    parts.push("⌘ Super".to_string());
                }
                let short_key = if key_name.is_empty() {
                    format!("Key#{kc}")
                } else {
                    key_name.to_uppercase()
                };
                parts.push(short_key);
                let label = parts.join("+");
                let new_binding = ZoomComboBinding::Custom {
                    ctrl,
                    alt,
                    shift,
                    super_key,
                    mouse_button: 0,
                    keycode: kc,
                    shake: false,
                    label: label.clone(),
                };
                let disp = new_binding.short_label();
                st.lock().zoom_cfg.combo_binding = new_binding;
                learn_flag.store(false, Ordering::Relaxed);
                for b in &btns {
                    b.remove_css_class("seg-btn-active");
                    b.add_css_class("seg-btn");
                }
                set_btn_text(&rec_b, &disp);
                rec_b.add_css_class("action-chip");
                f_lbl.set_text(&format!("✓ Registered Key Combo: {disp}"));
                return glib::Propagation::Stop;
            }

            // If not learning, check if this key matches a custom key binding to toggle zoom immediately
            let mut g = st.lock();
            if let ZoomComboBinding::Custom {
                ctrl: c_req,
                alt: a_req,
                shift: s_req,
                super_key: u_req,
                mouse_button: 0,
                keycode: req_kc,
                ..
            } = g.zoom_cfg.combo_binding.clone()
            {
                if req_kc != 0 && req_kc == kc && (!c_req || ctrl) && (!a_req || alt) && (!s_req || shift) && (!u_req || super_key) {
                    let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
                    let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
                    let tz = g.zoom_cfg.target_zoom;
                    let locked = g.zoom_engine.toggle_manual_lock(cx, cy, tz);
                    g.tick();
                    drop(g);
                    f_lbl.set_text(if locked {
                        "Key Combo -> Zoom Locked"
                    } else {
                        "Key Combo -> Zoom Unlocked"
                    });
                    r_area.queue_draw();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        window.add_controller(key_ctrl);
    }

    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        combo_action_btn.connect_clicked(move |btn| {
            let mut g = st.lock();
            g.zoom_cfg.combo_action = match g.zoom_cfg.combo_action {
                ComboActionMode::ToggleLock => ComboActionMode::HoldToZoom,
                ComboActionMode::HoldToZoom => ComboActionMode::StepCycle,
                ComboActionMode::StepCycle => ComboActionMode::ToggleLock,
            };
            let lbl = g.zoom_cfg.combo_action.label();
            drop(g);
            set_btn_text(btn, lbl);
            f_lbl.set_text(&format!("✓ Combo Behavior: {lbl}"));
        });
    }

    combo_track.append(&c_shake_mid);
    combo_track.append(&c_mid);
    combo_track.append(&c_ctrl);
    combo_track.append(&c_dbl);
    ctrl_card.append(&combo_track);

    let combo_custom_row = GtkBox::new(Orientation::Horizontal, 6);
    combo_custom_row.append(&record_combo_btn);
    combo_custom_row.append(&combo_action_btn);
    ctrl_card.append(&combo_custom_row);

    // Wire Dark / Light Theme toggle across top button + segmented switcher
    {
        let shell = shell_box.clone();
        let t_icon = theme_icon.clone();
        let r_area = radar_area.clone();
        let light_flag = is_light_mode.clone();
        let seg_btns = theme_seg_btns.clone();
        theme_btn.connect_clicked(move |_| {
            let next_light = !light_flag.load(Ordering::Relaxed);
            light_flag.store(next_light, Ordering::Relaxed);
            if next_light {
                shell.add_css_class("theme-light");
                t_icon.set_from_file(Some(asset_path("icon-sun.svg")));
                set_active_segment(&seg_btns, 1);
            } else {
                shell.remove_css_class("theme-light");
                t_icon.set_from_file(Some(asset_path("icon-theme.svg")));
                set_active_segment(&seg_btns, 0);
            }
            r_area.queue_draw();
        });
    }
    {
        let shell = shell_box.clone();
        let t_icon = theme_icon.clone();
        let r_area = radar_area.clone();
        let light_flag = is_light_mode.clone();
        let seg_btns = theme_seg_btns.clone();
        t_dark.connect_clicked(move |_| {
            light_flag.store(false, Ordering::Relaxed);
            shell.remove_css_class("theme-light");
            t_icon.set_from_file(Some(asset_path("icon-theme.svg")));
            set_active_segment(&seg_btns, 0);
            r_area.queue_draw();
        });
    }
    {
        let shell = shell_box.clone();
        let t_icon = theme_icon.clone();
        let r_area = radar_area.clone();
        let light_flag = is_light_mode.clone();
        let seg_btns = theme_seg_btns.clone();
        t_light.connect_clicked(move |_| {
            light_flag.store(true, Ordering::Relaxed);
            shell.add_css_class("theme-light");
            t_icon.set_from_file(Some(asset_path("icon-sun.svg")));
            set_active_segment(&seg_btns, 1);
            r_area.queue_draw();
        });
    }

    // Zoom Depth & Spring Response
    let mag_cap = Label::new(Some("ZOOM DEPTH & SPRING RESPONSE"));
    mag_cap.add_css_class("section-cap");
    mag_cap.set_halign(Align::Start);
    ctrl_card.append(&mag_cap);

    let mag_track = GtkBox::new(Orientation::Horizontal, 3);
    mag_track.add_css_class("seg-track");
    let z_15 = Button::with_label("1.5×");
    let z_20 = Button::with_label("2.0× Studio");
    let z_24 = Button::with_label("2.4×");
    let z_28 = Button::with_label("2.8× Deep");
    for b in [&z_15, &z_20, &z_24, &z_28] {
        b.set_hexpand(true);
    }
    let mag_btns = vec![z_15.clone(), z_20.clone(), z_24.clone(), z_28.clone()];
    set_active_segment(&mag_btns, 1);
    for (idx, (btn, val)) in [
        (z_15.clone(), 1.50f32),
        (z_20.clone(), 2.00f32),
        (z_24.clone(), 2.40f32),
        (z_28.clone(), 2.80f32),
    ]
    .into_iter()
    .enumerate()
    {
        let st = state.clone();
        let btns = mag_btns.clone();
        let f_lbl = footer_lbl.clone();
        let r_area = radar_area.clone();
        btn.connect_clicked(move |_| {
            let mut g = st.lock();
            g.zoom_cfg.target_zoom = val;
            let now = g.now_ms();
            let cx = g.last_cursor.smooth_x;
            let cy = g.last_cursor.smooth_y;
            g.zoom_engine.trigger_manual_zoom(cx, cy, val, now, 1650);
            g.tick();
            drop(g);
            set_active_segment(&btns, idx);
            f_lbl.set_text(&format!("✓ Zoom Depth: {val:.1}×"));
            r_area.queue_draw();
        });
    }
    mag_track.append(&z_15);
    mag_track.append(&z_20);
    mag_track.append(&z_24);
    mag_track.append(&z_28);
    ctrl_card.append(&mag_track);

    let spr_track = GtkBox::new(Orientation::Horizontal, 3);
    spr_track.add_css_class("seg-track");
    let s_cinema = Button::with_label("Smooth Glide");
    let s_natural = Button::with_label("Balanced");
    let s_snappy = Button::with_label("Snappy");
    for b in [&s_cinema, &s_natural, &s_snappy] {
        b.set_hexpand(true);
    }
    let spr_btns = vec![s_cinema.clone(), s_natural.clone(), s_snappy.clone()];
    set_active_segment(&spr_btns, 1);
    for (idx, (btn, k_val, name)) in [
        (s_cinema.clone(), 38.0f32, "Smooth Glide"),
        (s_natural.clone(), 60.0f32, "Balanced"),
        (s_snappy.clone(), 115.0f32, "Snappy"),
    ]
    .into_iter()
    .enumerate()
    {
        let st = state.clone();
        let btns = spr_btns.clone();
        let f_lbl = footer_lbl.clone();
        btn.connect_clicked(move |_| {
            st.lock().zoom_cfg.spring_stiffness = k_val;
            set_active_segment(&btns, idx);
            f_lbl.set_text(&format!("✓ Spring Physics: {name}"));
        });
    }
    spr_track.append(&s_cinema);
    spr_track.append(&s_natural);
    spr_track.append(&s_snappy);
    ctrl_card.append(&spr_track);

    // Toggle Chips
    let tog_row1 = GtkBox::new(Orientation::Horizontal, 6);
    let bake_on = state.lock().auto_apply_zoom_on_stop;
    let chip_bake = Button::with_label(if bake_on {
        "● Auto-Zoom MP4: ON"
    } else {
        "○ Auto-Zoom MP4: OFF"
    });
    chip_bake.add_css_class(if bake_on { "toggle-chip-on" } else { "toggle-chip" });
    chip_bake.set_hexpand(true);

    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        chip_bake.connect_clicked(move |btn| {
            let mut g = st.lock();
            g.auto_apply_zoom_on_stop = !g.auto_apply_zoom_on_stop;
            let on = g.auto_apply_zoom_on_stop;
            g.zoom_cfg.enabled = on;
            g.settings.auto_zoom = on;
            g.settings.save();
            drop(g);
            if on {
                set_btn_text(btn, "● Auto-Zoom MP4: ON");
                btn.remove_css_class("toggle-chip");
                btn.add_css_class("toggle-chip-on");
                f_lbl.set_text("✓ Auto-Zoom MP4 Post-Pass: ON");
            } else {
                set_btn_text(btn, "○ Auto-Zoom MP4: OFF");
                btn.remove_css_class("toggle-chip-on");
                btn.add_css_class("toggle-chip");
                f_lbl.set_text("○ Auto-Zoom MP4 Post-Pass: OFF (Raw MP4)");
            }
        });
    }
    tog_row1.append(&chip_bake);
    ctrl_card.append(&tog_row1);

    camera_page.append(&ctrl_card);

    // Bottom Status & Quick Open Bar
    let footer_row = GtkBox::new(Orientation::Horizontal, 6);
    footer_row.add_css_class("status-strip");

    let play_last_btn = icon_button("play", "Play latest");
    play_last_btn.add_css_class("action-chip");
    let open_videos_btn = icon_button("folder", "Videos");

    footer_row.append(&footer_lbl);
    footer_row.append(&play_last_btn);
    footer_row.append(&open_videos_btn);
    drawer_box.append(&footer_row);

    shell_box.append(&drawer_box);
    {
        let fit = fit_drawer.clone();
        window.connect_map(move |_| {
            let fit = fit.clone();
            glib::idle_add_local_once(move || fit());
        });
    }
    window.set_child(Some(&shell_box));
    let keep_island_on_screen = install_island_drag(&window, &brand_box, layer_shell);

    // =========================================================================
    // MORPHING & NON-BLOCKING RECORDING HANDLERS
    // =========================================================================
    {
        let drw = drawer_box.clone();
        let win = window.clone();
        let keep_on_screen = keep_island_on_screen.clone();
        let fit = fit_drawer.clone();
        expand_btn.connect_clicked(move |btn| {
            let expand = !drw.is_visible();
            drw.set_visible(expand);
            if expand {
                fit();
            }
            let keep_on_screen = keep_on_screen.clone();
            glib::timeout_add_local_once(Duration::from_millis(80), move || keep_on_screen());
            if expand {
                set_btn_text(btn, "Studio ▴");
                btn.remove_css_class("studio-pill");
                btn.add_css_class("studio-pill-open");
            } else {
                set_btn_text(btn, "Studio ▾");
                btn.remove_css_class("studio-pill-open");
                btn.add_css_class("studio-pill");
                win.set_default_size(1, 1);
            }
        });
    }

    let bg_zoom_busy = Arc::new(AtomicBool::new(false));
    let render_progress = Arc::new(AtomicU32::new(0));
    let save_result: Arc<parking_lot::Mutex<Option<Result<PathBuf, String>>>> =
        Arc::new(parking_lot::Mutex::new(None));

    // Switches the island between its idle and recording looks.
    // While recording the island shrinks to a small pill (status, zoom, Stop)
    // so it covers as little of the screen as possible; drag it by the left end.
    let show_recording: Rc<dyn Fn(bool)> = {
        let r_icon = rec_icon.clone();
        let r_lbl = rec_btn_lbl.clone();
        let r_btn = rec_btn.clone();
        let shell = shell_box.clone();
        let s_lbl = status_lbl.clone();
        let extras = [theme_btn.clone(), expand_btn.clone(), close_btn.clone(), edit_btn.clone()];
        let drw = drawer_box.clone();
        let win = window.clone();
        let keep_on_screen = keep_island_on_screen.clone();
        Rc::new(move |recording: bool| {
            for b in &extras[..3] {
                b.set_visible(!recording);
            }
            if recording {
                extras[3].set_visible(false);
            }
            if recording && drw.is_visible() {
                drw.set_visible(false);
                set_btn_text(&extras[1], "Studio ▾");
                extras[1].remove_css_class("studio-pill-open");
                extras[1].add_css_class("studio-pill");
            }
            r_lbl.set_visible(!recording);
            win.set_default_size(1, 1);
            let keep_on_screen = keep_on_screen.clone();
            glib::timeout_add_local_once(Duration::from_millis(80), move || keep_on_screen());
            if recording {
                r_icon.set_from_file(Some(asset_path("icon-stop.svg")));
                r_lbl.set_text("Stop");
                r_btn.set_tooltip_text(Some("Stop recording"));
                r_btn.remove_css_class("shutter-rec");
                r_btn.add_css_class("shutter-stop");
                s_lbl.remove_css_class("island-title");
                s_lbl.add_css_class("island-timer-rec");
                shell.remove_css_class("island-shell");
                shell.add_css_class("island-shell-rec");
            } else {
                r_icon.set_from_file(Some(asset_path("icon-record.svg")));
                r_lbl.set_text("Record");
                r_btn.set_tooltip_text(Some("Start recording"));
                r_btn.remove_css_class("shutter-stop");
                r_btn.add_css_class("shutter-rec");
                shell.remove_css_class("island-shell-rec");
                shell.add_css_class("island-shell");
                s_lbl.remove_css_class("island-timer-rec");
                s_lbl.add_css_class("island-title");
            }
        })
    };

    // Stops the capture and finalizes, zooms, and frames it on a worker
    // thread; the 16 ms tick reports the result. `failure` is set when the
    // capture died on its own.
    let stop_and_save: Rc<dyn Fn(Option<String>)> = {
        let st = state.clone();
        let s_lbl = status_lbl.clone();
        let footer = footer_lbl.clone();
        let busy_flag = bg_zoom_busy.clone();
        let progress = render_progress.clone();
        let slot = save_result.clone();
        let show_recording = show_recording.clone();
        Rc::new(move |failure: Option<String>| {
            let Some(job) = st.lock().take_recording() else {
                return;
            };
            show_recording(false);
            s_lbl.set_text("Saving…");
            footer.set_text(&match &failure {
                Some(reason) => format!("Capture stopped: {reason}"),
                None => "Finalizing the recording…".to_string(),
            });
            busy_flag.store(true, Ordering::Relaxed);
            progress.store(0, Ordering::Relaxed);
            let busy_bg = busy_flag.clone();
            let progress = progress.clone();
            let slot = slot.clone();
            std::thread::spawn(move || {
                // Whatever happens in the render, the island must leave "Saving…".
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    job.complete(&progress)
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("the recorder hit an internal error while saving")))
                .map_err(|e| format!("{e:#}"));
                *slot.lock() = Some(result);
                busy_bg.store(false, Ordering::Relaxed);
            });
        })
    };

    {
        let st = state.clone();
        let s_lbl = status_lbl.clone();
        let footer = footer_lbl.clone();
        let busy_flag = bg_zoom_busy.clone();
        let show_recording = show_recording.clone();
        let stop_and_save = stop_and_save.clone();

        rec_btn.connect_clicked(move |_| {
            if busy_flag.load(Ordering::Relaxed) {
                footer.set_text("Still saving the last recording…");
                return;
            }
            if st.lock().countdown_left().is_some() {
                // Stopping before the countdown ends cancels the recording.
                let Some(job) = st.lock().take_recording() else { return };
                show_recording(false);
                s_lbl.remove_css_class("island-countdown");
                s_lbl.set_text("HyperGSR");
                footer.set_text("Recording cancelled");
                std::thread::spawn(move || job.discard());
                return;
            }
            if st.lock().is_recording() {
                stop_and_save(None);
                return;
            }
            let started = st.lock().start_recording();
            match started {
                Ok(_) => {
                    // The bar stays where it is, above every window, while recording.
                    show_recording(true);
                    s_lbl.set_text("Starting…");
                    footer.set_text("● Waiting for the screen share to start…");
                }
                Err(err) => {
                    s_lbl.set_text("HyperGSR");
                    footer.set_text(&format!("{err:#}"));
                }
            }
        });
    }

    {
        let st = state.clone();
        let s_lbl = status_lbl.clone();
        resume_btn.connect_clicked(move |btn| {
            let resumed = st.lock().resume_recording();
            match resumed {
                Ok(()) => {
                    btn.set_visible(false);
                    s_lbl.set_text("Starting…");
                }
                Err(err) => btn.set_tooltip_text(Some(&format!("Could not resume: {err:#}"))),
            }
        });
    }

    {
        let st = state.clone();
        let pause_icon = pause_icon.clone();
        pause_btn.connect_clicked(move |btn| {
            let paused = st.lock().toggle_pause();
            sync_pause_button(btn, &pause_icon, paused);
        });
    }

    {
        let st = state.clone();
        let app = app.clone();
        edit_btn.connect_clicked(move |_| {
            let file = {
                let g = st.lock();
                g.last_saved_file.as_ref().map(PathBuf::from).filter(|p| p.is_file()).or_else(|| latest_recording(&g.videos_dir))
            };
            if let Some(file) = file {
                crate::editor::open(&app, &file);
            }
        });
    }
    // Instant replay. The buffer's look reuses the recording one.
    let clip_slot: Arc<parking_lot::Mutex<Option<Result<PathBuf, String>>>> = Arc::default();
    let show_replay: Rc<dyn Fn(bool)> = {
        let show_recording = show_recording.clone();
        let (rec, replay, clip, stop, edit) =
            (rec_btn.clone(), replay_btn.clone(), clip_btn.clone(), replay_stop_btn.clone(), edit_btn.clone());
        let s_lbl = status_lbl.clone();
        Rc::new(move |on: bool| {
            show_recording(on);
            rec.set_visible(!on);
            replay.set_visible(!on);
            clip.set_visible(on);
            stop.set_visible(on);
            if on {
                edit.set_visible(false);
            }
            s_lbl.set_text(if on { "Starting…" } else { "HyperGSR" });
        })
    };
    {
        let st = state.clone();
        let footer = footer_lbl.clone();
        let show_replay = show_replay.clone();
        replay_btn.connect_clicked(move |_| {
            let started = st.lock().start_replay();
            match started {
                Ok(()) => {
                    show_replay(true);
                    let keep = st.lock().settings.replay_sec;
                    footer.set_text(&format!(
                        "● Instant replay is on: Save clip (or {}) keeps the last {} s",
                        crate::shortcuts::DEFAULT_LABEL,
                        keep
                    ));
                }
                Err(err) => footer.set_text(&format!("{err:#}")),
            }
        });
    }
    {
        let st = state.clone();
        let footer = footer_lbl.clone();
        let s_lbl = status_lbl.clone();
        let slot = clip_slot.clone();
        clip_btn.connect_clicked(move |btn| {
            let clip = st.lock().replay_clip();
            match clip {
                Ok(clip) => {
                    btn.set_sensitive(false);
                    s_lbl.set_text("Saving clip…");
                    let slot = slot.clone();
                    std::thread::spawn(move || *slot.lock() = Some(clip.save().map_err(|e| format!("{e:#}"))));
                }
                Err(err) => footer.set_text(&format!("{err:#}")),
            }
        });
    }
    {
        let st = state.clone();
        let show_replay = show_replay.clone();
        let edit = edit_btn.clone();
        replay_stop_btn.connect_clicked(move |_| {
            if let Some(replay) = st.lock().take_replay() {
                std::thread::spawn(move || replay.stop());
            }
            show_replay(false);
            edit.set_visible(latest_recording(&st.lock().videos_dir).is_some());
        });
    }

    // `hypergsr --edit <video>` with an island already running.
    {
        let action = gio::SimpleAction::new("edit", Some(glib::VariantTy::STRING));
        let app2 = app.clone();
        action.connect_activate(move |_, param| {
            if let Some(path) = param.and_then(|p| p.get::<String>()) {
                crate::editor::open(&app2, Path::new(&path));
            }
        });
        app.add_action(&action);
    }
    if let Some(path) = std::env::var_os("HYPERGSR_EDIT") {
        let app = app.clone();
        glib::idle_add_local_once(move || crate::editor::open(&app, Path::new(&path)));
    }

    // `hypergsr --toggle-recording` and `--pause-recording` (the desktop
    // shortcuts) land here.
    {
        let action = gio::SimpleAction::new("toggle-record", None);
        let rec = rec_btn.clone();
        let clip = clip_btn.clone();
        let st = state.clone();
        // While instant replay runs, the start/stop shortcut saves a clip.
        action.connect_activate(move |_, _| {
            if st.lock().replay_active() {
                clip.emit_clicked();
            } else {
                rec.emit_clicked();
            }
        });
        app.add_action(&action);
        let action = gio::SimpleAction::new("pause-record", None);
        let pause = pause_btn.clone();
        action.connect_activate(move |_, _| {
            if pause.is_visible() {
                pause.emit_clicked();
            }
        });
        app.add_action(&action);
    }
    if std::env::var("HYPERGSR_START_REPLAY").as_deref() == Ok("1") {
        let replay = replay_btn.clone();
        glib::timeout_add_local_once(Duration::from_millis(700), move || replay.emit_clicked());
    }
    if std::env::var("HYPERGSR_START_RECORDING").as_deref() == Ok("1") {
        let rec = rec_btn.clone();
        glib::timeout_add_local_once(Duration::from_millis(700), move || rec.emit_clicked());
    }

    {
        let st = state.clone();
        play_last_btn.connect_clicked(move |_| {
            let g = st.lock();
            if let Some(ref last) = g.last_saved_file {
                let _ = std::process::Command::new("xdg-open").arg(last).spawn();
            } else {
                let _ = std::process::Command::new("xdg-open")
                    .arg(&g.videos_dir)
                    .spawn();
            }
        });
    }

    {
        let st = state.clone();
        open_videos_btn.connect_clicked(move |_| {
            let dir = st.lock().videos_dir.clone();
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        });
    }

    {
        let st = state.clone();
        let win = window.clone();
        let busy_flag = bg_zoom_busy.clone();
        close_btn.connect_clicked(move |_| {
            // Save a running recording, and let a render in progress finish,
            // before quitting; the island hides at once. Instant replay just
            // stops.
            if let Some(replay) = st.lock().take_replay() {
                replay.stop();
            }
            let job = st.lock().take_recording();
            if job.is_none() && !busy_flag.load(Ordering::Relaxed) {
                win.close();
                return;
            }
            win.set_visible(false);
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            if let Some(job) = job {
                std::thread::spawn(move || {
                    let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        job.complete(&AtomicU32::new(0))
                    }));
                    if !matches!(saved, Ok(Ok(_))) {
                        eprintln!("[HyperGSR] Could not save the recording");
                    }
                    let _ = tx.send(());
                });
            } else {
                drop(tx);
            }
            let win = win.clone();
            let busy_flag = busy_flag.clone();
            let mut saved = false;
            glib::timeout_add_local(Duration::from_millis(100), move || {
                saved = saved || !matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
                if !saved || busy_flag.load(Ordering::Relaxed) {
                    return ControlFlow::Continue;
                }
                win.close();
                ControlFlow::Break
            });
        });
    }

    // Closing the window any other way (Alt+F4, the compositor) saves first too.
    {
        let st = state.clone();
        let busy_flag = bg_zoom_busy.clone();
        let close_btn = close_btn.clone();
        window.connect_close_request(move |_| {
            let pending = {
                let g = st.lock();
                g.is_recording() || g.replay_active()
            } || busy_flag.load(Ordering::Relaxed);
            if pending {
                close_btn.emit_clicked();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }

    // 16ms (60 Hz) High-Frequency Non-blocking Telemetry Tick
    {
        let st = state.clone();
        let z_lbl = zoom_pill_lbl.clone();
        let z_btn = zoom_pill_btn.clone();
        let c_lbl = coord_lbl.clone();
        let s_lbl = status_lbl.clone();
        let f_lbl = footer_lbl.clone();
        let drw = drawer_box.clone();
        let r_area = radar_area.clone();
        let wave = wave_indicator.clone();
        let z_hist = zoom_history.clone();
        let busy_flag = bg_zoom_busy.clone();
        let progress = render_progress.clone();
        let slot = save_result.clone();
        let stop_and_save = stop_and_save.clone();
        let learn_flag = learning_combo.clone();
        let rec_combo_b = record_combo_btn.clone();
        let preset_btns = combo_preset_btns.clone();
        let resume_b = resume_btn.clone();
        let edit_b = edit_btn.clone();
        let replay_b = replay_btn.clone();
        let clip_b = clip_btn.clone();
        let clip_slot = clip_slot.clone();
        let show_replay = show_replay.clone();
        let pause_b = pause_btn.clone();
        let pause_icon = pause_icon.clone();
        let mut last_pause_state = (false, false);
        let mut hidden = false;
        let mut notified = false;
        let win = window.clone();
        let keep_on_screen = keep_island_on_screen.clone();
        let app_n = app.clone();
        let mut last_paused: Option<String> = None;
        let mut last_z_str = String::new();
        let mut last_c_str = String::new();
        let mut last_s_str = String::new();
        let mut was_zoomed = false;
        let mut ui_div = 0u32;
        let mut idle_div = 0u32;

        glib::timeout_add_local(Duration::from_millis(16), move || {
            // Idle and collapsed, nothing on screen needs 60 Hz: poll at ~15 Hz
            // to keep the recorder's background cost negligible.
            idle_div = idle_div.wrapping_add(1);
            let hot = drw.is_visible()
                || learn_flag.load(Ordering::Relaxed)
                || busy_flag.load(Ordering::Relaxed);
            let Some(mut g) = st.try_lock() else {
                return ControlFlow::Continue;
            };
            if !hot && !g.is_recording() && !idle_div.is_multiple_of(4) {
                return ControlFlow::Continue;
            }
            g.tick();
            let failure = g.poll_capture_failure();
            let capture_live = g.recording_start.is_some();
            let vp = g.last_viewport;
            let cur = g.last_cursor;
            let target_z = g.zoom_cfg.target_zoom;
            let manual_locked = g.zoom_engine.manual_locked;
            let is_combo_only = matches!(g.zoom_cfg.mode, AutoZoomMode::Manual);
            let is_rec = g.is_recording();
            let paused = g.capture_paused.clone();
            let elapsed = g.recording_elapsed_sec();
            let countdown = g.countdown_left();
            let user_paused = g.is_paused_by_user();
            let hide_setting = g.settings.hide_while_recording;
            let replay_wanted = g.settings.replay_sec > 0;
            let replay_failure = g.poll_replay_failure();
            let replay_buffered = g.replay_active().then(|| g.replay_buffered_sec());

            // Check if user is in "Record Combo…" self-registration mode and performed a global gesture/button/key
            let mut learned_label: Option<String> = None;
            if learn_flag.load(Ordering::Relaxed) {
                let candidate = if cur.shake_gesture {
                    Some(ZoomComboBinding::CursorShake)
                } else if cur.middle_down {
                    Some(ZoomComboBinding::MiddleClick)
                } else if cur.side_down {
                    Some(ZoomComboBinding::SideButton)
                } else if cur.double_clicked {
                    Some(ZoomComboBinding::DoubleClick)
                } else if (cur.ctrl_down || cur.alt_down || cur.shift_down || cur.super_down)
                    && (cur.left_down || cur.right_down)
                {
                    let mut parts = Vec::new();
                    if cur.ctrl_down {
                        parts.push("⌃ Ctrl");
                    }
                    if cur.alt_down {
                        parts.push("⌥ Alt");
                    }
                    if cur.shift_down {
                        parts.push("⇧ Shift");
                    }
                    if cur.super_down {
                        parts.push("⌘ Super");
                    }
                    parts.push(if cur.right_down { "Right-Click" } else { "Click" });
                    Some(ZoomComboBinding::Custom {
                        ctrl: cur.ctrl_down,
                        alt: cur.alt_down,
                        shift: cur.shift_down,
                        super_key: cur.super_down,
                        mouse_button: if cur.right_down { 3 } else { 1 },
                        keycode: 0,
                        shake: false,
                        label: parts.join(" + "),
                    })
                } else if cur.active_keycode != 0 {
                    let kc = cur.active_keycode;
                    let mut parts = Vec::new();
                    if cur.ctrl_down {
                        parts.push("⌃ Ctrl".to_string());
                    }
                    if cur.alt_down {
                        parts.push("⌥ Alt".to_string());
                    }
                    if cur.shift_down {
                        parts.push("⇧ Shift".to_string());
                    }
                    if cur.super_down {
                        parts.push("⌘ Super".to_string());
                    }
                    parts.push(format!("Key#{kc}"));
                    Some(ZoomComboBinding::Custom {
                        ctrl: cur.ctrl_down,
                        alt: cur.alt_down,
                        shift: cur.shift_down,
                        super_key: cur.super_down,
                        mouse_button: 0,
                        keycode: kc,
                        shake: false,
                        label: parts.join("+"),
                    })
                } else {
                    None
                };

                if let Some(binding) = candidate {
                    let lbl = binding.short_label();
                    g.zoom_cfg.combo_binding = binding;
                    learn_flag.store(false, Ordering::Relaxed);
                    learned_label = Some(lbl);
                }
            }
            drop(g);

            if let Some(reason) = failure {
                stop_and_save(Some(reason));
                return ControlFlow::Continue;
            }
            if let Some(reason) = replay_failure {
                show_replay(false);
                f_lbl.set_text(&format!("Instant replay stopped: {reason}"));
            }
            // The Replay button follows the setting while idle.
            if replay_buffered.is_none() && !is_rec && replay_b.is_visible() != replay_wanted {
                replay_b.set_visible(replay_wanted);
                win.set_default_size(1, 1);
            }
            if let Some(saved) = clip_slot.lock().take() {
                clip_b.set_sensitive(true);
                last_s_str.clear();
                match saved {
                    Ok(path) => {
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        st.lock().last_saved_file = Some(path.display().to_string());
                        f_lbl.set_text(&format!("Saved {name}"));
                    }
                    Err(err) => f_lbl.set_text(&format!("Clip not saved: {err}")),
                }
            }
            if let Some(buffered) = replay_buffered {
                let new_s = match buffered {
                    _ if !clip_b.is_sensitive() => "Saving clip…".to_string(),
                    Some(sec) => format!("● Replay {:02}:{:02}", (sec as u64) / 60, (sec as u64) % 60),
                    None => "Starting…".to_string(),
                };
                if new_s != last_s_str {
                    s_lbl.set_text(&new_s);
                    last_s_str = new_s;
                }
                wave.queue_draw();
            }

            if paused != last_paused {
                resume_b.set_visible(paused.is_some());
                resume_b.set_tooltip_text(Some("Continue recording into the same video"));
                match &paused {
                    Some(reason) => {
                        let hint = format!(
                            "Screen sharing was interrupted ({reason}). Press Resume to continue \
                             the same video, or Stop to save it."
                        );
                        s_lbl.set_tooltip_text(Some(&hint));
                        let note = gio::Notification::new("Recording paused");
                        note.set_body(Some(&hint));
                        app_n.send_notification(Some("capture-paused"), &note);
                    }
                    None => {
                        s_lbl.set_tooltip_text(None);
                        app_n.withdraw_notification("capture-paused");
                    }
                }
                win.set_default_size(1, 1);
                let keep_on_screen = keep_on_screen.clone();
                glib::timeout_add_local_once(Duration::from_millis(80), move || keep_on_screen());
                last_paused = paused.clone();
            }

            let can_pause = is_rec && capture_live && paused.is_none() && countdown.is_none();
            // Out of the video: hidden while really recording, back for the
            // countdown, pauses (which are cut), and saving.
            let hide = hide_setting && can_pause && !user_paused;
            // The way back to a hidden bar: a quiet notification with Stop and
            // Pause, posted when the capture goes live so a banner shows
            // during the countdown, which is cut.
            let notify = hide_setting && is_rec && capture_live;
            if notify != notified {
                notified = notify;
                if notify {
                    let note = gio::Notification::new("Recording");
                    note.set_body(Some(&format!(
                        "{} stops, {} pauses.",
                        crate::shortcuts::DEFAULT_LABEL,
                        crate::shortcuts::PAUSE_LABEL
                    )));
                    note.set_priority(gio::NotificationPriority::Low);
                    note.add_button("Stop", "app.toggle-record");
                    note.add_button("Pause", "app.pause-record");
                    app_n.send_notification(Some("recording-hidden"), &note);
                } else {
                    app_n.withdraw_notification("recording-hidden");
                }
            }
            if hide != hidden {
                hidden = hide;
                if hide {
                    win.set_visible(false);
                } else {
                    win.set_visible(true);
                    let keep_on_screen = keep_on_screen.clone();
                    glib::timeout_add_local_once(Duration::from_millis(80), move || keep_on_screen());
                }
            }
            if (can_pause, user_paused) != last_pause_state {
                pause_b.set_visible(can_pause);
                sync_pause_button(&pause_b, &pause_icon, user_paused);
                last_pause_state = (can_pause, user_paused);
                win.set_default_size(1, 1);
            }

            if let Some(lbl) = learned_label {
                for b in &preset_btns {
                    b.remove_css_class("seg-btn-active");
                    b.add_css_class("seg-btn");
                }
                set_btn_text(&rec_combo_b, &lbl);
                rec_combo_b.add_css_class("action-chip");
                f_lbl.set_text(&format!("✓ Registered Global Gesture: {lbl}"));
            }

            ui_div = ui_div.wrapping_add(1);
            // Throttle GTK label/canvas redraws to ~30Hz while keeping telemetry sampling at 60Hz
            if !ui_div.is_multiple_of(2) {
                return ControlFlow::Continue;
            }

            let is_busy = busy_flag.load(Ordering::Relaxed);
            if is_busy {
                let permille = progress.load(Ordering::Relaxed);
                let new_s = if permille == 0 {
                    "Saving…".to_string()
                } else {
                    format!("Rendering {}%", permille / 10)
                };
                if new_s != last_s_str {
                    s_lbl.set_text(&new_s);
                    last_s_str = new_s;
                }
            }
            // Polled every tick: a save that fails instantly (a cancelled
            // screen share) finishes before busy is ever seen as set.
            let finished = if is_busy { None } else { slot.lock().take() };
            if let Some(finished) = finished {
                last_s_str.clear();
                match Some(finished) {
                    Some(Ok(path)) => {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        st.lock().last_saved_file = Some(path.display().to_string());
                        edit_b.set_visible(true);
                        win.set_default_size(1, 1);
                        s_lbl.set_text("Saved ✓");
                        f_lbl.set_text(&format!("Saved {name}"));
                    }
                    Some(Err(err)) => {
                        s_lbl.set_text("Not saved");
                        f_lbl.set_text(&err);
                    }
                    None => s_lbl.set_text("HyperGSR"),
                }
            }
            let is_zoomed = vp.zoom > 1.06;
            let new_z = if manual_locked {
                format!("{:.1}× Locked", vp.zoom.max(1.2))
            } else if is_zoomed {
                format!("{:.1}× Zoom", vp.zoom)
            } else if is_combo_only {
                format!("{:.1}× Combo", target_z)
            } else {
                format!("{:.1}× Auto", target_z)
            };
            if new_z != last_z_str {
                z_lbl.set_text(&new_z);
                last_z_str = new_z;
            }
            if is_zoomed != was_zoomed {
                if is_zoomed {
                    z_btn.remove_css_class("lens-pill");
                    z_btn.add_css_class("lens-pill-zoomed");
                } else {
                    z_btn.remove_css_class("lens-pill-zoomed");
                    z_btn.add_css_class("lens-pill");
                }
                was_zoomed = is_zoomed;
            }

            if let (true, Some(left)) = (is_rec, countdown) {
                let new_s = format!("Recording in {}", left.ceil() as u32);
                if new_s != last_s_str {
                    s_lbl.set_text(&new_s);
                    s_lbl.add_css_class("island-countdown");
                    last_s_str = new_s;
                }
                wave.queue_draw();
            } else if is_rec && (paused.is_some() || user_paused) {
                s_lbl.remove_css_class("island-countdown");
                let new_s = format!("⏸ Paused {:02}:{:02}", (elapsed as u64) / 60, (elapsed as u64) % 60);
                if new_s != last_s_str {
                    s_lbl.set_text(&new_s);
                    last_s_str = new_s;
                }
            } else if is_rec && !capture_live {
                if last_s_str != "Starting…" {
                    s_lbl.set_text("Starting…");
                    last_s_str = "Starting…".to_string();
                }
                wave.queue_draw();
            } else if is_rec {
                s_lbl.remove_css_class("island-countdown");
                let mins = (elapsed as u64) / 60;
                let secs = (elapsed as u64) % 60;
                let tenths = ((elapsed * 10.0) as u64) % 10;
                let new_s = format!("● {mins:02}:{secs:02}.{tenths}");
                if new_s != last_s_str {
                    s_lbl.set_text(&new_s);
                    last_s_str = new_s;
                }
                wave.queue_draw();
            } else if is_zoomed {
                wave.queue_draw();
            }

            if drw.is_visible() {
                let px = (cur.smooth_x * 1920.0) as i32;
                let py = (cur.smooth_y * 1080.0) as i32;
                let new_c = format!("DRM #92 · ({px}, {py}) · {:.2}×", vp.zoom);
                if new_c != last_c_str {
                    c_lbl.set_text(&new_c);
                    last_c_str = new_c;
                }

                let mut h_buf = z_hist.borrow_mut();
                if h_buf.len() >= 72 {
                    h_buf.pop_front();
                }
                h_buf.push_back(vp.zoom);
                drop(h_buf);
                r_area.queue_draw();
            }

            ControlFlow::Continue
        });
    }

    if let Ok(tab) = std::env::var("HYPERGSR_STUDIO_TAB") {
        if let Some((btn, _)) = tabs.iter().find(|(_, name)| *name == tab) {
            btn.emit_clicked();
        }
    }
    if std::env::var("HYPERGSR_OPEN_STUDIO").as_deref() == Ok("1") {
        drawer_box.set_visible(true);
        set_btn_text(&expand_btn, "Studio ▴");
        expand_btn.remove_css_class("studio-pill");
        expand_btn.add_css_class("studio-pill-open");
    }
    if std::env::var("HYPERGSR_LIGHT_THEME").as_deref() == Ok("1") {
        gtk4::prelude::ButtonExt::emit_clicked(&theme_btn);
    }

    if let Ok(shot_path) = std::env::var("HYPERGSR_SCREENSHOT_PATH") {
        let win_shot = window.clone();
        let shell_shot = shell_box.clone();
        let delay = std::env::var("HYPERGSR_SCREENSHOT_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(650);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            save_widget_png(&shell_shot, &win_shot, &shot_path);
            win_shot.close();
        });
    }

    window.present();
}
