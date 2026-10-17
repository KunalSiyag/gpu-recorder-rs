use crate::encoder::apply_hardware_autozoom_pass;
use crate::state::SharedStudioState;
use crate::zoom_engine::{AutoZoomMode, ComboActionMode, ZoomComboBinding};
use glib::object::ObjectType;
use glib::ControlFlow;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, DrawingArea,
    EventControllerKey, EventControllerMotion, EventControllerScroll, EventControllerScrollFlags,
    GestureClick, Image, Label, Orientation,
};
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const ISLAND_WINDOW_TITLE: &str = "HyperGSR Dynamic Island";

fn asset_path(name: &str) -> PathBuf {
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
    {
        for base in [exe_dir.join("assets"), exe_dir.join("../../assets")] {
            let candidate = base.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("assets")
        .join(name)
}

pub fn launch_gtk_ui(state: SharedStudioState, _web_port: u16) {
    let app = Application::builder()
        .application_id("com.factoryai.hypergsr.island")
        .build();

    app.connect_activate(move |app| {
        build_dynamic_island(app, state.clone());
    });

    let empty_args: Vec<String> = vec![];
    app.run_with_args(&empty_args);
}

fn init_wayland_layer_shell(window: &ApplicationWindow) -> bool {
    unsafe {
        let path = CString::new("/usr/local/lib/x86_64-linux-gnu/libgtk4-layer-shell.so.0").unwrap();
        let mut handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
        if handle.is_null() {
            let fallback = CString::new("libgtk4-layer-shell.so.0").unwrap();
            handle = libc::dlopen(fallback.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
        }
        if handle.is_null() {
            return false;
        }

        type IsSupportedFn = unsafe extern "C" fn() -> c_int;
        type InitForWinFn = unsafe extern "C" fn(*mut c_void);
        type SetLayerFn = unsafe extern "C" fn(*mut c_void, c_int);
        type SetAnchorFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
        type SetMarginFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
        type SetExclZoneFn = unsafe extern "C" fn(*mut c_void, c_int);
        type SetKbModeFn = unsafe extern "C" fn(*mut c_void, c_int);
        type SetNamespaceFn = unsafe extern "C" fn(*mut c_void, *const c_char);

        let sym = |name: &str| -> *mut c_void {
            let cs = CString::new(name).unwrap();
            libc::dlsym(handle, cs.as_ptr())
        };

        let is_sup_ptr = sym("gtk_layer_is_supported");
        let init_ptr = sym("gtk_layer_init_for_window");
        if is_sup_ptr.is_null() || init_ptr.is_null() {
            return false;
        }

        let is_supported: IsSupportedFn = std::mem::transmute(is_sup_ptr);
        if is_supported() == 0 {
            return false;
        }

        let init_for_window: InitForWinFn = std::mem::transmute(init_ptr);
        let set_layer: SetLayerFn = std::mem::transmute(sym("gtk_layer_set_layer"));
        let set_anchor: SetAnchorFn = std::mem::transmute(sym("gtk_layer_set_anchor"));
        let set_margin: SetMarginFn = std::mem::transmute(sym("gtk_layer_set_margin"));
        let set_excl: SetExclZoneFn = std::mem::transmute(sym("gtk_layer_set_exclusive_zone"));
        let set_kb: SetKbModeFn = std::mem::transmute(sym("gtk_layer_set_keyboard_mode"));
        let set_ns: SetNamespaceFn = std::mem::transmute(sym("gtk_layer_set_namespace"));

        let win_ptr = window.as_ptr() as *mut c_void;

        init_for_window(win_ptr);
        let ns = CString::new("hypergsr-dynamic-island").unwrap();
        set_ns(win_ptr, ns.as_ptr());

        set_layer(win_ptr, 3); // OVERLAY
        set_anchor(win_ptr, 0, 0);
        set_anchor(win_ptr, 1, 0);
        set_anchor(win_ptr, 2, 1); // TOP CENTER
        set_anchor(win_ptr, 3, 0);
        set_margin(win_ptr, 2, 6);
        set_excl(win_ptr, -1);
        set_kb(win_ptr, 2); // ON_DEMAND
        true
    }
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
            font-family: "Inter", "SF Pro Display", system-ui, sans-serif;
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
            font-family: "JetBrains Mono", monospace;
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
            font-family: "JetBrains Mono", monospace;
            font-size: 10.5px;
            font-weight: 700;
            padding: 4px 10px;
        }
        button.lens-pill-zoomed {
            background-color: rgba(16, 185, 129, 0.18);
            border: 1px solid rgba(52, 211, 153, 0.48);
            color: #6ee7b7;
            font-family: "JetBrains Mono", monospace;
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
            font-family: "JetBrains Mono", monospace;
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
            font-family: "JetBrains Mono", monospace;
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
    }
}

fn set_active_segment(buttons: &[Button], active_idx: usize) {
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

fn draw_rounded_rect(cr: &gtk4::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
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

    let _layer_ok = init_wayland_layer_shell(&window);

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
    let close_btn = Button::with_label("✕");
    close_btn.add_css_class("close-pill");
    close_btn.set_tooltip_text(Some("Quit HyperGSR"));

    top_row.append(&brand_box);
    top_row.append(&zoom_pill_btn);
    top_row.append(&top_spacer);
    top_row.append(&rec_btn);
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
    drawer_box.set_size_request(368, -1);
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
    radar_area.set_content_width(346);
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

    let footer_lbl = Label::new(Some("60 FPS Native GPU -> ~/Videos"));
    footer_lbl.set_hexpand(true);
    footer_lbl.set_halign(Align::Start);

    let vis_btn_row = GtkBox::new(Orientation::Horizontal, 6);
    let test_zoom_btn = Button::with_label("🔒 Lock / Unlock Zoom");
    test_zoom_btn.add_css_class("action-chip");
    test_zoom_btn.set_hexpand(true);
    let pulse_zoom_btn = Button::with_label("⚡ 1.7s Pulse");
    let reset_zoom_btn = Button::with_label("1.0× Wide");
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
                btn.set_label("🔓 Unlock Zoom");
                f_lbl.set_text(&format!("🔒 Manual Zoom Locked at {target_z:.1}×"));
            } else {
                btn.set_label("🔒 Lock / Unlock Zoom");
                f_lbl.set_text("🔓 Manual Zoom Unlocked (1.0× Wide)");
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
            lock_b.set_label("🔒 Lock / Unlock Zoom");
            f_lbl.set_text(&format!("⚡ Triggered 1.7s Auto-Zoom Pulse ({target_z:.1}×)"));
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
                lock_b.set_label("🔓 Unlock Zoom");
                f_lbl.set_text(&format!("🔒 Manual Zoom Locked at {target_z:.1}×"));
            } else {
                lock_b.set_label("🔒 Lock / Unlock Zoom");
                f_lbl.set_text("🔓 Manual Zoom Unlocked (1.0× Wide)");
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
            lock_b.set_label("🔒 Lock / Unlock Zoom");
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
            f_lbl.set_text(&format!("🖱️ Wheel Zoom -> {new_z:.1}×"));
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
            lock_b.set_label("🔒 Lock / Unlock Zoom");
            f_lbl.set_text("↺ Reset Camera to 1.0× Wide");
            r_area.queue_draw();
        });
    }
    vis_btn_row.append(&test_zoom_btn);
    vis_btn_row.append(&pulse_zoom_btn);
    vis_btn_row.append(&reset_zoom_btn);
    vis_card.append(&vis_btn_row);

    drawer_box.append(&vis_card);

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
    let t_dark = Button::with_label("🌙 Obsidian Dark");
    let t_light = Button::with_label("☀️ Pearl Light");
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
    let c_shake_mid = Button::with_label("⇄ Shake/Mid");
    let c_mid = Button::with_label("🖱️ Mid-Click");
    let c_ctrl = Button::with_label("⌃ Ctrl+Click");
    let c_dbl = Button::with_label("🖱️ Dbl-Click");
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

    let record_combo_btn = Button::with_label("🎯 Record Combo…");
    record_combo_btn.set_hexpand(true);
    record_combo_btn.set_tooltip_text(Some(
        "Click, then perform ANY mouse click (Middle/Right/Side/Ctrl+Click), key press, or cursor shake to bind!",
    ));

    let combo_action_btn = Button::with_label("⇄ Toggle Lock");
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
            rec_b.set_label("🎯 Record Combo…");
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
                btn.set_label("● Press Key, Click, or Shake…");
                btn.add_css_class("action-chip");
                f_lbl.set_text("🎯 Listening: Press any key/combo, Mid/Right-click, or Shake cursor!");
            } else {
                let lbl = st.lock().zoom_cfg.combo_binding.short_label();
                btn.set_label(&lbl);
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
                        "🔒 Manual Zoom Locked"
                    } else {
                        "🔓 Manual Zoom Unlocked"
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
                    2 => "🖱️ Mid-Click",
                    3 => "🖱️ Right-Click",
                    _ => "🖱️ Side-Btn",
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
            rec_b.set_label(&lbl);
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
                let label = format!("⌨️ {}", parts.join("+"));
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
                rec_b.set_label(&disp);
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
                        "🔒 Key Combo -> Zoom Locked"
                    } else {
                        "🔓 Key Combo -> Zoom Unlocked"
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
            btn.set_label(lbl);
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
    let chip_bake = Button::with_label("● Auto-Zoom MP4: ON");
    chip_bake.add_css_class("toggle-chip-on");
    chip_bake.set_hexpand(true);
    let chip_audio = Button::with_label("● System Audio: ON");
    chip_audio.add_css_class("toggle-chip-on");
    chip_audio.set_hexpand(true);

    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        chip_bake.connect_clicked(move |btn| {
            let mut g = st.lock();
            g.auto_apply_zoom_on_stop = !g.auto_apply_zoom_on_stop;
            let on = g.auto_apply_zoom_on_stop;
            g.zoom_cfg.enabled = on;
            drop(g);
            if on {
                btn.set_label("● Auto-Zoom MP4: ON");
                btn.remove_css_class("toggle-chip");
                btn.add_css_class("toggle-chip-on");
                f_lbl.set_text("✓ Auto-Zoom MP4 Post-Pass: ON");
            } else {
                btn.set_label("○ Auto-Zoom MP4: OFF");
                btn.remove_css_class("toggle-chip-on");
                btn.add_css_class("toggle-chip");
                f_lbl.set_text("○ Auto-Zoom MP4 Post-Pass: OFF (Raw MP4)");
            }
        });
    }
    {
        let st = state.clone();
        let f_lbl = footer_lbl.clone();
        chip_audio.connect_clicked(move |btn| {
            let mut g = st.lock();
            g.record_audio = !g.record_audio;
            let on = g.record_audio;
            drop(g);
            if on {
                btn.set_label("● System Audio: ON");
                btn.remove_css_class("toggle-chip");
                btn.add_css_class("toggle-chip-on");
                f_lbl.set_text("✓ System Desktop Audio: ON");
            } else {
                btn.set_label("○ System Audio: OFF");
                btn.remove_css_class("toggle-chip-on");
                btn.add_css_class("toggle-chip");
                f_lbl.set_text("○ System Desktop Audio: OFF");
            }
        });
    }
    tog_row1.append(&chip_bake);
    tog_row1.append(&chip_audio);
    ctrl_card.append(&tog_row1);

    drawer_box.append(&ctrl_card);

    // Bottom Status & Quick Open Bar
    let footer_row = GtkBox::new(Orientation::Horizontal, 6);
    footer_row.add_css_class("status-strip");

    let play_last_btn = Button::with_label("▶ Play Latest");
    play_last_btn.add_css_class("action-chip");
    let open_videos_btn = Button::with_label("📂 Videos");

    footer_row.append(&footer_lbl);
    footer_row.append(&play_last_btn);
    footer_row.append(&open_videos_btn);
    drawer_box.append(&footer_row);

    shell_box.append(&drawer_box);
    window.set_child(Some(&shell_box));

    // =========================================================================
    // MORPHING & NON-BLOCKING RECORDING HANDLERS
    // =========================================================================
    {
        let drw = drawer_box.clone();
        let win = window.clone();
        expand_btn.connect_clicked(move |btn| {
            let expand = !drw.is_visible();
            drw.set_visible(expand);
            if expand {
                btn.set_label("Studio ▴");
                btn.remove_css_class("studio-pill");
                btn.add_css_class("studio-pill-open");
            } else {
                btn.set_label("Studio ▾");
                btn.remove_css_class("studio-pill-open");
                btn.add_css_class("studio-pill");
                win.set_default_size(1, 1);
            }
        });
    }

    let bg_zoom_busy = Arc::new(AtomicBool::new(false));
    {
        let st = state.clone();
        let s_lbl = status_lbl.clone();
        let r_icon = rec_icon.clone();
        let r_lbl = rec_btn_lbl.clone();
        let r_btn = rec_btn.clone();
        let shell = shell_box.clone();
        let footer = footer_lbl.clone();
        let drw = drawer_box.clone();
        let exp_b = expand_btn.clone();
        let win = window.clone();
        let busy_flag = bg_zoom_busy.clone();

        rec_btn.connect_clicked(move |_| {
            if busy_flag.load(Ordering::Relaxed) {
                return;
            }
            let is_currently_rec = st.lock().encoder_session.is_some();
            if is_currently_rec {
                if let Ok(Some((raw_path, zoom_events, do_zoom))) = st.lock().stop_recording_fast()
                {
                    r_icon.set_from_file(Some(asset_path("icon-record.svg")));
                    r_lbl.set_text("Record");
                    r_btn.remove_css_class("shutter-stop");
                    r_btn.add_css_class("shutter-rec");
                    shell.remove_css_class("island-shell-rec");
                    shell.add_css_class("island-shell");
                    s_lbl.remove_css_class("island-timer-rec");
                    s_lbl.add_css_class("island-title");

                    if do_zoom && !zoom_events.is_empty() {
                        s_lbl.set_text("Zooming…");
                        footer.set_text("⚡ Baking 60fps Sub-Pixel Auto-Zoom...");
                        busy_flag.store(true, Ordering::Relaxed);
                        let st_bg = st.clone();
                        let busy_bg = busy_flag.clone();
                        std::thread::spawn(move || {
                            let final_p = apply_hardware_autozoom_pass(&raw_path, &zoom_events)
                                .unwrap_or(raw_path);
                            st_bg.lock().last_saved_file = Some(final_p.display().to_string());
                            busy_bg.store(false, Ordering::Relaxed);
                        });
                    } else {
                        s_lbl.set_text("Saved ✓");
                        footer.set_text("✔ Saved to ~/Videos");
                    }
                }
            } else if let Ok(_path) = st.lock().start_recording() {
                // Automatically collapse Studio popover into compact Dynamic Island on Record
                if drw.is_visible() {
                    drw.set_visible(false);
                    exp_b.set_label("Studio ▾");
                    exp_b.remove_css_class("studio-pill-open");
                    exp_b.add_css_class("studio-pill");
                    win.set_default_size(1, 1);
                }
                r_icon.set_from_file(Some(asset_path("icon-stop.svg")));
                r_lbl.set_text("Stop");
                r_btn.remove_css_class("shutter-rec");
                r_btn.add_css_class("shutter-stop");
                s_lbl.set_text("● 00:00.0");
                s_lbl.remove_css_class("island-title");
                s_lbl.add_css_class("island-timer-rec");
                shell.remove_css_class("island-shell");
                shell.add_css_class("island-shell-rec");
                footer.set_text("● Recording 1080p60 + Auto-Zoom...");
            }
        });
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
        close_btn.connect_clicked(move |_| {
            let _ = st.lock().stop_recording_fast();
            win.close();
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
        let learn_flag = learning_combo.clone();
        let rec_combo_b = record_combo_btn.clone();
        let preset_btns = combo_preset_btns.clone();
        let mut last_z_str = String::new();
        let mut last_c_str = String::new();
        let mut last_s_str = String::new();
        let mut was_zoomed = false;
        let mut was_busy = false;
        let mut ui_div = 0u32;

        glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(mut g) = st.try_lock() else {
                return ControlFlow::Continue;
            };
            g.tick();
            let vp = g.last_viewport;
            let cur = g.last_cursor;
            let target_z = g.zoom_cfg.target_zoom;
            let manual_locked = g.zoom_engine.manual_locked;
            let is_combo_only = matches!(g.zoom_cfg.mode, AutoZoomMode::Manual);
            let is_rec = g.encoder_session.is_some();
            let elapsed = g.recording_elapsed_sec();
            let last_saved = g.last_saved_file.clone();

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
                        label: format!("⌨️ {}", parts.join("+")),
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

            if let Some(lbl) = learned_label {
                for b in &preset_btns {
                    b.remove_css_class("seg-btn-active");
                    b.add_css_class("seg-btn");
                }
                rec_combo_b.set_label(&lbl);
                rec_combo_b.add_css_class("action-chip");
                f_lbl.set_text(&format!("✓ Registered Global Gesture: {lbl}"));
            }

            ui_div = ui_div.wrapping_add(1);
            // Throttle GTK label/canvas redraws to ~30Hz while keeping telemetry sampling at 60Hz
            if ui_div % 2 != 0 {
                return ControlFlow::Continue;
            }

            let is_busy = busy_flag.load(Ordering::Relaxed);
            if was_busy && !is_busy {
                s_lbl.set_text("HyperGSR");
                if last_saved.is_some() {
                    f_lbl.set_text("✔ Saved Smooth Auto-Zoom MP4 -> ~/Videos");
                }
            }
            was_busy = is_busy;

            let is_zoomed = vp.zoom > 1.06;
            let new_z = if manual_locked {
                format!("🔒 {:.1}× Lock", vp.zoom.max(1.2))
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

            if is_rec {
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

    if std::env::var("HYPERGSR_OPEN_STUDIO").as_deref() == Ok("1") {
        drawer_box.set_visible(true);
        expand_btn.set_label("Studio ▴");
        expand_btn.remove_css_class("studio-pill");
        expand_btn.add_css_class("studio-pill-open");
    }
    if std::env::var("HYPERGSR_LIGHT_THEME").as_deref() == Ok("1") {
        gtk4::prelude::ButtonExt::emit_clicked(&theme_btn);
    }

    if let Ok(shot_path) = std::env::var("HYPERGSR_SCREENSHOT_PATH") {
        let win_shot = window.clone();
        let shell_shot = shell_box.clone();
        glib::timeout_add_local_once(Duration::from_millis(650), move || {
            let paintable = gtk4::WidgetPaintable::new(Some(&shell_shot));
            let w = shell_shot.width().max(1);
            let h = shell_shot.height().max(1);
            let snap = gtk4::Snapshot::new();
            gdk4::prelude::PaintableExt::snapshot(&paintable, &snap, w as f64, h as f64);
            if let Some(node) = snap.to_node() {
                if let Some(renderer) = gtk4::prelude::NativeExt::renderer(&win_shot) {
                    let rect = gtk4::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
                    let tex = gtk4::prelude::GskRendererExt::render_texture(&renderer, &node, Some(&rect));
                    let _ = gdk4::prelude::TextureExt::save_to_png(&tex, &shot_path);
                }
            }
            win_shot.close();
        });
    }

    window.present();
}
