use crate::encoder::apply_hardware_autozoom_pass;
use crate::state::SharedStudioState;
use crate::zoom_engine::AutoZoomMode;
use glib::object::ObjectType;
use glib::ControlFlow;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, DrawingArea,
    EventControllerMotion, GestureClick, Image, Label, Orientation, Revealer,
    RevealerTransitionType,
};
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const ISLAND_WINDOW_TITLE: &str = "HyperGSR Dynamic Island";

fn asset_path(name: &str) -> PathBuf {
    let base = PathBuf::from("/home/kunalsiyag/Ambassador/factoryai/gpu-recorder-rs/assets");
    let candidate = base.join(name);
    if candidate.exists() {
        return candidate;
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
        set_margin(win_ptr, 2, 4);
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

        /* =========================================================
           OBSIDIAN DARK MODE (Default Apple Dynamic Island)
           ========================================================= */
        .island-shell {
            background-color: rgba(5, 5, 8, 0.97);
            border: 1px solid rgba(255, 255, 255, 0.14);
            border-radius: 22px;
            padding: 5px 10px;
            margin: 1px;
        }
        .island-shell-rec {
            background-color: rgba(12, 4, 7, 0.98);
            border: 1.5px solid rgba(244, 63, 94, 0.72);
            border-radius: 22px;
            padding: 5px 10px;
            margin: 1px;
        }
        .island-top-row {
            min-height: 28px;
        }
        .brand-pill {
            background-color: rgba(255, 255, 255, 0.05);
            border-radius: 999px;
            padding: 2px 9px 2px 6px;
        }
        .island-title {
            color: #fafafa;
            font-size: 11.5px;
            font-weight: 700;
            letter-spacing: -0.01em;
        }
        .island-timer-rec {
            color: #fda4af;
            font-family: "JetBrains Mono", monospace;
            font-size: 11.5px;
            font-weight: 700;
        }
        button {
            background-image: none;
            box-shadow: none;
            outline: none;
            border: none;
            border-radius: 999px;
            padding: 3px 10px;
            font-size: 11px;
            font-weight: 600;
            min-height: 24px;
            background-color: rgba(255, 255, 255, 0.07);
            color: #d4d4d8;
        }
        button:hover {
            background-color: rgba(255, 255, 255, 0.14);
            color: #ffffff;
        }
        .lens-pill {
            background-color: rgba(99, 102, 241, 0.15);
            color: #c7d2fe;
            border-radius: 999px;
            padding: 3px 10px;
            font-family: "JetBrains Mono", monospace;
            font-size: 10.5px;
            font-weight: 700;
        }
        .lens-pill:hover {
            background-color: rgba(99, 102, 241, 0.26);
        }
        .lens-pill-zoomed {
            background-color: rgba(16, 185, 129, 0.19);
            color: #6ee7b7;
            border-radius: 999px;
            padding: 3px 10px;
            font-family: "JetBrains Mono", monospace;
            font-size: 10.5px;
            font-weight: 700;
        }
        .shutter-rec {
            background-color: #e11d48;
            color: #ffffff;
            border-radius: 999px;
            padding: 3px 12px;
            font-size: 11px;
            font-weight: 700;
        }
        .shutter-rec:hover {
            background-color: #f43f5e;
            color: #ffffff;
        }
        .shutter-stop {
            background-color: rgba(244, 63, 94, 0.22);
            color: #fecdd3;
            border: 1px solid rgba(244, 63, 94, 0.55);
            border-radius: 999px;
            padding: 3px 12px;
            font-size: 11px;
            font-weight: 700;
        }
        .theme-pill {
            background-color: rgba(255, 255, 255, 0.065);
            border-radius: 999px;
            padding: 3px 8px;
            min-width: 24px;
        }
        .studio-pill {
            background-color: rgba(255, 255, 255, 0.07);
            color: #e4e4e7;
            border-radius: 999px;
            padding: 3px 10px;
            font-size: 11px;
            font-weight: 600;
        }
        .studio-pill-open {
            background-color: rgba(99, 102, 241, 0.24);
            color: #e0e7ff;
            border-radius: 999px;
            padding: 3px 10px;
            font-size: 11px;
            font-weight: 600;
        }
        .close-pill {
            background-color: transparent;
            color: #71717a;
            border-radius: 999px;
            padding: 2px 7px;
            min-width: 22px;
        }
        .close-pill:hover {
            background-color: rgba(244, 63, 94, 0.2);
            color: #fda4af;
        }
        .island-drawer {
            border-top: 1px solid rgba(255, 255, 255, 0.08);
            margin-top: 7px;
            padding: 9px 4px 3px 4px;
        }
        .bento-card {
            background-color: rgba(17, 17, 23, 0.92);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 16px;
            padding: 10px;
        }
        .section-cap {
            color: #71717a;
            font-size: 9.5px;
            font-weight: 700;
            letter-spacing: 0.08em;
        }
        .seg-track {
            background-color: rgba(6, 6, 10, 0.85);
            border: 1px solid rgba(255, 255, 255, 0.06);
            border-radius: 11px;
            padding: 2px;
        }
        .seg-btn {
            background-color: transparent;
            color: #a1a1aa;
            border-radius: 9px;
            padding: 4px 8px;
            font-size: 10.5px;
            font-weight: 600;
            min-height: 22px;
        }
        .seg-btn:hover {
            background-color: rgba(255, 255, 255, 0.06);
            color: #f4f4f5;
        }
        .seg-btn-active {
            background-color: #4f46e5;
            color: #ffffff;
            border-radius: 9px;
            padding: 4px 8px;
            font-size: 10.5px;
            font-weight: 700;
            min-height: 22px;
        }
        .seg-btn-active:hover {
            background-color: #6366f1;
            color: #ffffff;
        }
        .toggle-chip {
            background-color: rgba(255, 255, 255, 0.04);
            border: 1px solid rgba(255, 255, 255, 0.07);
            color: #71717a;
            border-radius: 11px;
            padding: 5px 9px;
            font-size: 10.5px;
            font-weight: 600;
        }
        .toggle-chip-on {
            background-color: rgba(16, 185, 129, 0.13);
            border: 1px solid rgba(16, 185, 129, 0.35);
            color: #a7f3d0;
            border-radius: 11px;
            padding: 5px 9px;
            font-size: 10.5px;
            font-weight: 700;
        }
        .action-chip {
            background-color: rgba(99, 102, 241, 0.16);
            color: #c7d2fe;
            border-radius: 10px;
            padding: 4px 10px;
            font-size: 10.5px;
            font-weight: 600;
        }
        .action-chip:hover {
            background-color: rgba(99, 102, 241, 0.28);
            color: #ffffff;
        }
        .status-strip {
            border-top: 1px solid rgba(255, 255, 255, 0.07);
            margin-top: 4px;
            padding-top: 6px;
            font-family: "JetBrains Mono", monospace;
            font-size: 10px;
            color: #a1a1aa;
        }

        /* =========================================================
           PEARL FROSTED LIGHT MODE (.theme-light)
           ========================================================= */
        .theme-light.island-shell {
            background-color: rgba(248, 250, 252, 0.97);
            border: 1px solid rgba(15, 23, 42, 0.15);
            color: #0f172a;
        }
        .theme-light.island-shell-rec {
            background-color: rgba(255, 245, 247, 0.98);
            border: 1.5px solid #e11d48;
            color: #0f172a;
        }
        .theme-light .brand-pill {
            background-color: rgba(15, 23, 42, 0.06);
        }
        .theme-light .island-title {
            color: #0f172a;
        }
        .theme-light .island-timer-rec {
            color: #e11d48;
        }
        .theme-light button {
            background-color: rgba(15, 23, 42, 0.06);
            color: #334155;
        }
        .theme-light button:hover {
            background-color: rgba(15, 23, 42, 0.12);
            color: #0f172a;
        }
        .theme-light .lens-pill {
            background-color: rgba(79, 70, 229, 0.12);
            color: #4338ca;
        }
        .theme-light .lens-pill-zoomed {
            background-color: rgba(5, 150, 105, 0.15);
            color: #047857;
        }
        .theme-light .shutter-rec {
            background-color: #e11d48;
            color: #ffffff;
        }
        .theme-light .shutter-rec:hover {
            background-color: #be123c;
            color: #ffffff;
        }
        .theme-light .studio-pill {
            background-color: rgba(15, 23, 42, 0.07);
            color: #1e293b;
        }
        .theme-light .studio-pill-open {
            background-color: rgba(79, 70, 229, 0.16);
            color: #3730a3;
        }
        .theme-light .island-drawer {
            border-top: 1px solid rgba(15, 23, 42, 0.09);
        }
        .theme-light .bento-card {
            background-color: rgba(255, 255, 255, 0.94);
            border: 1px solid rgba(15, 23, 42, 0.09);
        }
        .theme-light .section-cap {
            color: #64748b;
        }
        .theme-light .seg-track {
            background-color: #e2e8f0;
            border: 1px solid rgba(15, 23, 42, 0.06);
        }
        .theme-light .seg-btn {
            background-color: transparent;
            color: #475569;
        }
        .theme-light .seg-btn:hover {
            background-color: rgba(255, 255, 255, 0.6);
            color: #0f172a;
        }
        .theme-light .seg-btn-active {
            background-color: #4f46e5;
            color: #ffffff;
        }
        .theme-light .toggle-chip {
            background-color: #f1f5f9;
            border: 1px solid #cbd5e1;
            color: #64748b;
        }
        .theme-light .toggle-chip-on {
            background-color: rgba(16, 185, 129, 0.14);
            border: 1px solid rgba(5, 150, 105, 0.4);
            color: #065f46;
        }
        .theme-light .action-chip {
            background-color: rgba(79, 70, 229, 0.12);
            color: #4338ca;
        }
        .theme-light .status-strip {
            border-top: 1px solid rgba(15, 23, 42, 0.09);
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
    if is_light_mode.load(Ordering::Relaxed) {
        shell_box.add_css_class("theme-light");
    }

    // =========================================================================
    // COMPACT DYNAMIC ISLAND CAPSULE (Tight, zero dead space, Dark & Light modes)
    // =========================================================================
    let top_row = GtkBox::new(Orientation::Horizontal, 6);
    top_row.add_css_class("island-top-row");
    top_row.set_valign(Align::Center);

    // Left Brand & Live Waveform Pill
    let brand_box = GtkBox::new(Orientation::Horizontal, 6);
    brand_box.add_css_class("brand-pill");
    brand_box.set_valign(Align::Center);

    let logo_img = Image::from_file(asset_path("hypergsr-logo.svg"));
    logo_img.set_pixel_size(18);

    let wave_indicator = DrawingArea::new();
    wave_indicator.set_content_width(26);
    wave_indicator.set_content_height(14);
    {
        let st = state.clone();
        wave_indicator.set_draw_func(move |_, cr, _w, h| {
            let Some(g) = st.try_lock() else { return };
            let is_rec = g.encoder_session.is_some();
            let t = g.now_ms() as f64 / 1000.0;
            let zoom = g.last_viewport.zoom as f64;
            drop(g);

            for i in 0..4 {
                let x = 2.0 + i as f64 * 6.0;
                let phase = t * 7.5 + i as f64 * 1.15;
                let amp = if is_rec {
                    0.35 + 0.65 * phase.sin().abs()
                } else if zoom > 1.06 {
                    0.30 + 0.60 * (phase * 1.2).cos().abs()
                } else {
                    0.28
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
                draw_rounded_rect(cr, x, y, 3.0, bar_h, 1.5);
                cr.fill().ok();
            }
        });
    }

    let status_lbl = Label::new(Some("HyperGSR"));
    status_lbl.add_css_class("island-title");

    brand_box.append(&logo_img);
    brand_box.append(&wave_indicator);
    brand_box.append(&status_lbl);

    // Center Live Auto-Zoom Lens Badge
    let zoom_pill_btn = Button::new();
    zoom_pill_btn.add_css_class("lens-pill");
    zoom_pill_btn.set_tooltip_text(Some(
        "Live Auto-Zoom Lens — Tracks your cursor automatically (Click to pulse 2.15× zoom)",
    ));
    let zoom_pill_box = GtkBox::new(Orientation::Horizontal, 5);
    let zoom_icon = Image::from_file(asset_path("icon-autozoom.svg"));
    zoom_icon.set_pixel_size(13);
    let zoom_pill_lbl = Label::new(Some("2.15× FOLLOW"));
    zoom_pill_box.append(&zoom_icon);
    zoom_pill_box.append(&zoom_pill_lbl);
    zoom_pill_btn.set_child(Some(&zoom_pill_box));

    // Right Shutter Record Button
    let rec_btn = Button::new();
    rec_btn.add_css_class("shutter-rec");
    rec_btn.set_tooltip_text(Some("Start / Stop 60 FPS Zero-Copy GPU Recording"));
    let rec_btn_box = GtkBox::new(Orientation::Horizontal, 5);
    let rec_icon = Image::from_file(asset_path("icon-record.svg"));
    rec_icon.set_pixel_size(13);
    let rec_btn_lbl = Label::new(Some("Record"));
    rec_btn_box.append(&rec_icon);
    rec_btn_box.append(&rec_btn_lbl);
    rec_btn.set_child(Some(&rec_btn_box));

    // Quick Dark / Light Theme Pill Button right on the Dynamic Island
    let theme_btn = Button::new();
    theme_btn.add_css_class("theme-pill");
    theme_btn.set_tooltip_text(Some("Switch between Obsidian Dark Mode & Pearl Light Mode"));
    let theme_icon = Image::from_file(if is_light_mode.load(Ordering::Relaxed) {
        asset_path("icon-sun.svg")
    } else {
        asset_path("icon-theme.svg")
    });
    theme_icon.set_pixel_size(14);
    theme_btn.set_child(Some(&theme_icon));

    // Studio Drawer Toggle Pill
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

    let close_btn = Button::with_label("✕");
    close_btn.add_css_class("close-pill");
    close_btn.set_tooltip_text(Some("Quit HyperGSR"));

    top_row.append(&brand_box);
    top_row.append(&zoom_pill_btn);
    top_row.append(&rec_btn);
    top_row.append(&theme_btn);
    top_row.append(&expand_btn);
    top_row.append(&close_btn);

    shell_box.append(&top_row);

    // =========================================================================
    // EXPANDED DYNAMIC ISLAND STUDIO (Custom Bento Cards & Segmented Pills)
    // =========================================================================
    let revealer = Revealer::new();
    revealer.set_transition_type(RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(160);
    revealer.set_reveal_child(start_expanded);

    let drawer_box = GtkBox::new(Orientation::Vertical, 8);
    drawer_box.add_css_class("island-drawer");

    let bento_cols = GtkBox::new(Orientation::Horizontal, 10);

    // -------------------------------------------------------------------------
    // LEFT BENTO CARD: Live Hardware Cursor + Auto-Zoom Camera Viewfinder
    // -------------------------------------------------------------------------
    let vis_card = GtkBox::new(Orientation::Vertical, 7);
    vis_card.add_css_class("bento-card");
    vis_card.set_size_request(292, -1);

    let vis_hdr = GtkBox::new(Orientation::Horizontal, 6);
    let gpu_icon = Image::from_file(asset_path("icon-gpu.svg"));
    gpu_icon.set_pixel_size(13);
    let vis_title = Label::new(Some("LIVE CURSOR CAMERA & SPRING TELEMETRY"));
    vis_title.add_css_class("section-cap");
    vis_hdr.append(&gpu_icon);
    vis_hdr.append(&vis_title);
    vis_card.append(&vis_hdr);

    let radar_area = DrawingArea::new();
    radar_area.set_content_width(272);
    radar_area.set_content_height(156);

    let zoom_history: std::rc::Rc<std::cell::RefCell<VecDeque<f32>>> =
        std::rc::Rc::new(std::cell::RefCell::new(VecDeque::from(vec![1.0; 64])));

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
            let screen_h = h - 34.0;
            let light = light_flag.load(Ordering::Relaxed);

            let vp = g.last_viewport;
            let cur = g.last_cursor;
            let now_ms = g.now_ms();
            let ripples: Vec<_> = g.cursor_tracker.active_ripples().to_vec();
            drop(g);

            // 1. Rounded 16:9 Screen Canvas (adapts to Dark & Light theme)
            draw_rounded_rect(cr, 0.0, 0.0, w, screen_h, 10.0);
            if light {
                cr.set_source_rgb(0.94, 0.96, 0.985);
            } else {
                cr.set_source_rgb(0.035, 0.038, 0.055);
            }
            cr.fill().ok();

            // Simulated desktop window silhouettes for spatial reference
            if light {
                cr.set_source_rgba(0.06, 0.09, 0.16, 0.06);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.035);
            }
            draw_rounded_rect(cr, w * 0.08, screen_h * 0.14, w * 0.46, screen_h * 0.65, 5.0);
            cr.fill().ok();
            draw_rounded_rect(cr, w * 0.58, screen_h * 0.20, w * 0.34, screen_h * 0.58, 5.0);
            cr.fill().ok();

            // Subtle rule-of-thirds grid
            if light {
                cr.set_source_rgba(0.06, 0.09, 0.16, 0.07);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.045);
            }
            cr.set_line_width(1.0);
            for i in 1..=2 {
                let gx = w * (i as f64 / 3.0);
                let gy = screen_h * (i as f64 / 3.0);
                cr.move_to(gx, 4.0);
                cr.line_to(gx, screen_h - 4.0);
                cr.move_to(4.0, gy);
                cr.line_to(w - 4.0, gy);
            }
            cr.stroke().ok();

            // 2. Virtual Auto-Zoom Camera Crop Box
            let vx = (vp.uv_min_x as f64 * w).clamp(1.0, w - 6.0);
            let vy = (vp.uv_min_y as f64 * screen_h).clamp(1.0, screen_h - 6.0);
            let vw = (vp.uv_w as f64 * w).clamp(8.0, w - vx - 1.0);
            let vh = (vp.uv_h as f64 * screen_h).clamp(8.0, screen_h - vy - 1.0);

            if vp.zoom > 1.06 {
                cr.set_source_rgba(0.06, 0.72, 0.50, if light { 0.16 } else { 0.14 });
                draw_rounded_rect(cr, vx, vy, vw, vh, 6.0);
                cr.fill().ok();

                cr.set_source_rgba(0.06, 0.76, 0.54, 0.95);
                cr.set_line_width(1.8);
                draw_rounded_rect(cr, vx, vy, vw, vh, 6.0);
                cr.stroke().ok();
            } else {
                cr.set_source_rgba(0.38, 0.40, 0.95, 0.45);
                cr.set_line_width(1.2);
                draw_rounded_rect(cr, vx + 1.0, vy + 1.0, vw - 2.0, vh - 2.0, 8.0);
                cr.stroke().ok();
            }

            // 3. Click / Interaction Ripples
            for ripple in &ripples {
                let p = ((now_ms.saturating_sub(ripple.start_ms)) as f64 / 650.0).clamp(0.0, 1.0);
                let rx = ripple.x as f64 * w;
                let ry = ripple.y as f64 * screen_h;
                cr.set_source_rgba(0.06, 0.76, 0.54, (1.0 - p) * 0.9);
                cr.set_line_width(2.0);
                cr.arc(rx, ry, 4.0 + p * 20.0, 0.0, std::f64::consts::TAU);
                cr.stroke().ok();
            }

            // 4. Live Hardware Cursor Dot & Focus Halo
            let cx = (cur.smooth_x as f64 * w).clamp(5.0, w - 5.0);
            let cy = (cur.smooth_y as f64 * screen_h).clamp(5.0, screen_h - 5.0);

            cr.set_source_rgba(0.06, 0.76, 0.54, 0.30);
            cr.arc(cx, cy, 9.0, 0.0, std::f64::consts::TAU);
            cr.fill().ok();

            if light {
                cr.set_source_rgba(0.06, 0.09, 0.16, 0.95);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.98);
            }
            cr.arc(cx, cy, 3.8, 0.0, std::f64::consts::TAU);
            cr.fill().ok();

            // 5. Bottom Spring Zoom Curve Strip
            let strip_y = screen_h + 5.0;
            let strip_h = h - strip_y;
            draw_rounded_rect(cr, 0.0, strip_y, w, strip_h, 7.0);
            if light {
                cr.set_source_rgb(0.91, 0.94, 0.97);
            } else {
                cr.set_source_rgb(0.045, 0.048, 0.07);
            }
            cr.fill().ok();

            let hist = z_hist.borrow();
            if hist.len() >= 2 {
                cr.set_source_rgba(0.31, 0.27, 0.90, 0.95);
                cr.set_line_width(1.8);
                for (idx, &z_val) in hist.iter().enumerate() {
                    let px = (idx as f64 / (hist.len() - 1).max(1) as f64) * (w - 8.0) + 4.0;
                    let norm_z = ((z_val as f64 - 1.0) / 1.85).clamp(0.0, 1.0);
                    let py = strip_y + strip_h - 4.0 - norm_z * (strip_h - 8.0);
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

    let vis_btn_row = GtkBox::new(Orientation::Horizontal, 6);
    let test_zoom_btn = Button::with_label("⚡ Pulse 2.15× Zoom");
    test_zoom_btn.add_css_class("action-chip");
    test_zoom_btn.set_hexpand(true);
    let reset_zoom_btn = Button::with_label("1.0× Wide");
    {
        let st = state.clone();
        test_zoom_btn.connect_clicked(move |_| {
            let mut g = st.lock();
            let now = g.now_ms();
            let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
            let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
            let target_z = g.zoom_cfg.target_zoom;
            let cur_cfg = g.cursor_cfg.clone();
            g.cursor_tracker.update(now, cx, cy, true, false, &cur_cfg);
            g.zoom_engine
                .trigger_manual_zoom(cx, cy, target_z, now, 2600);
        });
    }
    {
        let st = state.clone();
        zoom_pill_btn.connect_clicked(move |_| {
            let mut g = st.lock();
            let now = g.now_ms();
            let cx = g.last_cursor.smooth_x.clamp(0.18, 0.82);
            let cy = g.last_cursor.smooth_y.clamp(0.18, 0.82);
            let target_z = g.zoom_cfg.target_zoom;
            let cur_cfg = g.cursor_cfg.clone();
            g.cursor_tracker.update(now, cx, cy, true, false, &cur_cfg);
            g.zoom_engine
                .trigger_manual_zoom(cx, cy, target_z, now, 2600);
        });
    }
    {
        let st = state.clone();
        reset_zoom_btn.connect_clicked(move |_| {
            let mut g = st.lock();
            let now = g.now_ms();
            g.zoom_engine.trigger_manual_zoom(0.5, 0.5, 1.0, now, 400);
        });
    }
    vis_btn_row.append(&test_zoom_btn);
    vis_btn_row.append(&reset_zoom_btn);
    vis_card.append(&vis_btn_row);

    bento_cols.append(&vis_card);

    // -------------------------------------------------------------------------
    // RIGHT BENTO CARD: Custom Segmented Pill Controls + Appearance Mode
    // -------------------------------------------------------------------------
    let ctrl_card = GtkBox::new(Orientation::Vertical, 7);
    ctrl_card.add_css_class("bento-card");
    ctrl_card.set_hexpand(true);

    // 0. Appearance Mode Segmented Switcher (Obsidian Dark vs Pearl Light)
    let app_cap = Label::new(Some("APPEARANCE THEME"));
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

    // 1. Segmented Control: Auto-Zoom Camera Behavior
    let mode_cap = Label::new(Some("AUTO-ZOOM CAMERA MODE"));
    mode_cap.add_css_class("section-cap");
    mode_cap.set_halign(Align::Start);

    let mode_track = GtkBox::new(Orientation::Horizontal, 3);
    mode_track.add_css_class("seg-track");
    let m_follow = Button::with_label("Smart Follow");
    let m_click = Button::with_label("Click Lock");
    let m_tour = Button::with_label("Cinema Tour");
    for b in [&m_follow, &m_click, &m_tour] {
        b.set_hexpand(true);
    }
    let mode_btns = vec![m_follow.clone(), m_click.clone(), m_tour.clone()];
    set_active_segment(&mode_btns, 0);
    {
        let st = state.clone();
        let btns = mode_btns.clone();
        m_follow.connect_clicked(move |_| {
            st.lock().zoom_cfg.mode = AutoZoomMode::SmartCursorFollow;
            set_active_segment(&btns, 0);
        });
    }
    {
        let st = state.clone();
        let btns = mode_btns.clone();
        m_click.connect_clicked(move |_| {
            st.lock().zoom_cfg.mode = AutoZoomMode::ClickToZoom;
            set_active_segment(&btns, 1);
        });
    }
    {
        let st = state.clone();
        let btns = mode_btns.clone();
        m_tour.connect_clicked(move |_| {
            st.lock().zoom_cfg.mode = AutoZoomMode::TimelineKeyframes;
            set_active_segment(&btns, 2);
        });
    }
    mode_track.append(&m_follow);
    mode_track.append(&m_click);
    mode_track.append(&m_tour);

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
    app_track.append(&t_dark);
    app_track.append(&t_light);
    ctrl_card.append(&app_track);
    ctrl_card.append(&mode_cap);
    ctrl_card.append(&mode_track);

    // 2. Segmented Control: Magnification Depth
    let mag_cap = Label::new(Some("ZOOM DEPTH"));
    mag_cap.add_css_class("section-cap");
    mag_cap.set_halign(Align::Start);
    ctrl_card.append(&mag_cap);

    let mag_track = GtkBox::new(Orientation::Horizontal, 3);
    mag_track.add_css_class("seg-track");
    let z_15 = Button::with_label("1.5×");
    let z_21 = Button::with_label("2.15× Studio");
    let z_26 = Button::with_label("2.6×");
    let z_30 = Button::with_label("3.0× Deep");
    for b in [&z_15, &z_21, &z_26, &z_30] {
        b.set_hexpand(true);
    }
    let mag_btns = vec![z_15.clone(), z_21.clone(), z_26.clone(), z_30.clone()];
    set_active_segment(&mag_btns, 1);
    for (idx, (btn, val)) in [
        (z_15.clone(), 1.50f32),
        (z_21.clone(), 2.15f32),
        (z_26.clone(), 2.60f32),
        (z_30.clone(), 3.00f32),
    ]
    .into_iter()
    .enumerate()
    {
        let st = state.clone();
        let btns = mag_btns.clone();
        btn.connect_clicked(move |_| {
            let mut g = st.lock();
            g.zoom_cfg.target_zoom = val;
            let now = g.now_ms();
            let cx = g.last_cursor.smooth_x;
            let cy = g.last_cursor.smooth_y;
            g.zoom_engine.trigger_manual_zoom(cx, cy, val, now, 2000);
            drop(g);
            set_active_segment(&btns, idx);
        });
    }
    mag_track.append(&z_15);
    mag_track.append(&z_21);
    mag_track.append(&z_26);
    mag_track.append(&z_30);
    ctrl_card.append(&mag_track);

    // 3. Segmented Control: Camera Spring Physics
    let spr_cap = Label::new(Some("SPRING PHYSICS RESPONSE"));
    spr_cap.add_css_class("section-cap");
    spr_cap.set_halign(Align::Start);
    ctrl_card.append(&spr_cap);

    let spr_track = GtkBox::new(Orientation::Horizontal, 3);
    spr_track.add_css_class("seg-track");
    let s_cinema = Button::with_label("Cinematic Glide");
    let s_natural = Button::with_label("Balanced");
    let s_snappy = Button::with_label("Instant");
    for b in [&s_cinema, &s_natural, &s_snappy] {
        b.set_hexpand(true);
    }
    let spr_btns = vec![s_cinema.clone(), s_natural.clone(), s_snappy.clone()];
    set_active_segment(&spr_btns, 1);
    for (idx, (btn, k_val)) in [
        (s_cinema.clone(), 55.0f32),
        (s_natural.clone(), 95.0f32),
        (s_snappy.clone(), 185.0f32),
    ]
    .into_iter()
    .enumerate()
    {
        let st = state.clone();
        let btns = spr_btns.clone();
        btn.connect_clicked(move |_| {
            st.lock().zoom_cfg.spring_stiffness = k_val;
            set_active_segment(&btns, idx);
        });
    }
    spr_track.append(&s_cinema);
    spr_track.append(&s_natural);
    spr_track.append(&s_snappy);
    ctrl_card.append(&spr_track);

    // 4. Custom Toggle Chips
    let tog_row1 = GtkBox::new(Orientation::Horizontal, 6);
    let chip_bake = Button::with_label("● Auto-Zoom MP4: ON");
    chip_bake.add_css_class("toggle-chip-on");
    chip_bake.set_hexpand(true);
    let chip_audio = Button::with_label("● System Audio: ON");
    chip_audio.add_css_class("toggle-chip-on");
    chip_audio.set_hexpand(true);

    {
        let st = state.clone();
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
            } else {
                btn.set_label("○ Auto-Zoom MP4: OFF");
                btn.remove_css_class("toggle-chip-on");
                btn.add_css_class("toggle-chip");
            }
        });
    }
    {
        let st = state.clone();
        chip_audio.connect_clicked(move |btn| {
            let mut g = st.lock();
            g.record_audio = !g.record_audio;
            let on = g.record_audio;
            drop(g);
            if on {
                btn.set_label("● System Audio: ON");
                btn.remove_css_class("toggle-chip");
                btn.add_css_class("toggle-chip-on");
            } else {
                btn.set_label("○ System Audio: OFF");
                btn.remove_css_class("toggle-chip-on");
                btn.add_css_class("toggle-chip");
            }
        });
    }
    tog_row1.append(&chip_bake);
    tog_row1.append(&chip_audio);
    ctrl_card.append(&tog_row1);

    bento_cols.append(&ctrl_card);
    drawer_box.append(&bento_cols);

    // Bottom Status & Quick Open Bar
    let footer_row = GtkBox::new(Orientation::Horizontal, 8);
    footer_row.add_css_class("status-strip");
    let footer_lbl = Label::new(Some(
        "DRM Cursor Plane #92 Active · 60 FPS Zero-Copy GPU -> ~/Videos",
    ));
    footer_lbl.set_hexpand(true);
    footer_lbl.set_halign(Align::Start);

    let play_last_btn = Button::with_label("▶ Play Latest");
    play_last_btn.add_css_class("action-chip");
    let open_videos_btn = Button::with_label("📂 ~/Videos");

    footer_row.append(&footer_lbl);
    footer_row.append(&play_last_btn);
    footer_row.append(&open_videos_btn);
    drawer_box.append(&footer_row);

    revealer.set_child(Some(&drawer_box));
    shell_box.append(&revealer);
    window.set_child(Some(&shell_box));

    // =========================================================================
    // MORPHING & NON-BLOCKING RECORDING HANDLERS
    // =========================================================================
    {
        let rev = revealer.clone();
        let win = window.clone();
        expand_btn.connect_clicked(move |btn| {
            let expand = !rev.reveals_child();
            rev.set_reveal_child(expand);
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
                        footer.set_text("⚡ Baking NVENC Cursor Auto-Zoom into MP4...");
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
                        footer.set_text(&format!("✔ Saved: {}", raw_path.display()));
                    }
                }
            } else if let Ok(path) = st.lock().start_recording() {
                r_icon.set_from_file(Some(asset_path("icon-stop.svg")));
                r_lbl.set_text("Stop");
                r_btn.remove_css_class("shutter-rec");
                r_btn.add_css_class("shutter-stop");
                s_lbl.set_text("● 00:00.0");
                s_lbl.remove_css_class("island-title");
                s_lbl.add_css_class("island-timer-rec");
                shell.remove_css_class("island-shell");
                shell.add_css_class("island-shell-rec");
                footer.set_text(&format!(
                    "● Recording 1080p60 + Live Cursor Auto-Zoom -> {}",
                    path.display()
                ));
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

    // 40ms (25 Hz) Non-blocking Telemetry Tick
    {
        let st = state.clone();
        let z_lbl = zoom_pill_lbl.clone();
        let z_btn = zoom_pill_btn.clone();
        let s_lbl = status_lbl.clone();
        let f_lbl = footer_lbl.clone();
        let rev = revealer.clone();
        let r_area = radar_area.clone();
        let wave = wave_indicator.clone();
        let z_hist = zoom_history.clone();
        let busy_flag = bg_zoom_busy.clone();
        let mut last_z_str = String::new();
        let mut last_s_str = String::new();
        let mut was_zoomed = false;
        let mut was_busy = false;

        glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(mut g) = st.try_lock() else {
                return ControlFlow::Continue;
            };
            g.tick();
            let vp = g.last_viewport;
            let cur = g.last_cursor;
            let is_rec = g.encoder_session.is_some();
            let elapsed = g.recording_elapsed_sec();
            let last_saved = g.last_saved_file.clone();
            drop(g);

            let is_busy = busy_flag.load(Ordering::Relaxed);
            if was_busy && !is_busy {
                s_lbl.set_text("HyperGSR");
                if let Some(ref saved) = last_saved {
                    f_lbl.set_text(&format!("✔ Saved Auto-Zoom MP4: {saved}"));
                }
            }
            was_busy = is_busy;

            let is_zoomed = vp.zoom > 1.06;
            let px = (cur.smooth_x * 1920.0) as i32;
            let py = (cur.smooth_y * 1080.0) as i32;
            let new_z = if is_zoomed {
                format!("{:.2}× ({px},{py})", vp.zoom)
            } else {
                format!("1.00× ({px},{py})")
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

            if rev.reveals_child() {
                let mut h_buf = z_hist.borrow_mut();
                if h_buf.len() >= 64 {
                    h_buf.pop_front();
                }
                h_buf.push_back(vp.zoom);
                drop(h_buf);
                r_area.queue_draw();
            }

            ControlFlow::Continue
        });
    }

    window.present();
}
