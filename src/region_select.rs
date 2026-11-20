//! Full-screen overlay for dragging out the area of the screen to record.
//!
//! The overlay covers one monitor; the chosen rectangle is stored normalized
//! to that monitor and cropped out of the captured screen by FFmpeg.
//!
//! With layer-shell the overlay is see-through. Without it (sandboxed apps on
//! COSMIC, GNOME) it has to be a fullscreen window, which compositors back
//! with an opaque fill, so a still of the screen from the Screenshot portal
//! is drawn underneath instead.

use crate::settings::Region;
use glib::object::ObjectType;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, DrawingArea, EventControllerKey, GestureClick, GestureDrag};
use gtk4::cairo::{Format, ImageSurface};
use std::cell::{Cell, RefCell};
use std::ffi::{c_void, CString};
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Default)]
struct Selection {
    start: Option<(f64, f64)>,
    end: (f64, f64),
}

impl Selection {
    fn rect(&self) -> Option<(f64, f64, f64, f64)> {
        let (sx, sy) = self.start?;
        let (ex, ey) = self.end;
        Some((sx.min(ex), sy.min(ey), (ex - sx).abs(), (ey - sy).abs()))
    }
}

type ScreenshotDone = Box<dyn FnOnce(Option<PathBuf>)>;

/// Asks the Screenshot portal for a still of the screen, without its dialog,
/// and passes the saved PNG to `done` (`None` when the portal refuses).
fn portal_screenshot(done: impl FnOnce(Option<PathBuf>) + 'static) {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return done(None);
    };
    let Some(sender) = bus.unique_name() else {
        return done(None);
    };
    let token = format!("hypergsr_area_{}", std::process::id());
    let request = format!(
        "/org/freedesktop/portal/desktop/request/{}/{token}",
        sender.trim_start_matches(':').replace('.', "_")
    );
    // Taken by whichever comes first: the portal's answer or a failed call.
    let done: Rc<RefCell<Option<ScreenshotDone>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    let subscription = Rc::new(RefCell::new(None));
    {
        let done = done.clone();
        let sub = subscription.clone();
        *subscription.borrow_mut() = Some(bus.subscribe_to_signal(
            Some("org.freedesktop.portal.Desktop"),
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&request),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let path = signal
                    .parameters
                    .get::<(u32, glib::VariantDict)>()
                    .filter(|(code, _)| *code == 0)
                    .and_then(|(_, results)| results.lookup::<String>("uri").ok().flatten())
                    .and_then(|uri| gio::File::for_uri(&uri).path());
                if let Some(done) = done.borrow_mut().take() {
                    done(path);
                }
                let sub = sub.clone();
                glib::idle_add_local_once(move || drop(sub.borrow_mut().take()));
            },
        ));
    }
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &token);
    options.insert("interactive", false);
    options.insert("modal", false);
    bus.call(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Screenshot",
        "Screenshot",
        Some(&glib::Variant::tuple_from_iter(["".to_variant(), options.end()])),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |result| {
            if result.is_err() {
                subscription.borrow_mut().take();
                if let Some(done) = done.borrow_mut().take() {
                    done(None);
                }
            }
        },
    );
}

/// Loads the portal's PNG for cairo, and deletes it when it is one of
/// COSMIC's temporary screenshots (GNOME saves into Pictures; those stay).
fn load_backdrop(path: &std::path::Path) -> Option<ImageSurface> {
    let texture = gdk4::Texture::from_filename(path).ok();
    let temporary = path.starts_with("/tmp")
        || (path.starts_with("/run/user")
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("screenshot-")));
    if temporary {
        let _ = std::fs::remove_file(path);
    }
    let texture = texture?;
    let (w, h) = (texture.width(), texture.height());
    let stride = Format::ARgb32.stride_for_width(w as u32).ok()?;
    // GDK's default download format is premultiplied BGRA, which is cairo's
    // ARGB32 on little-endian machines.
    let mut data = vec![0u8; stride as usize * h as usize];
    texture.download(&mut data, stride as usize);
    ImageSurface::create_for_data(data, Format::ARgb32, w, h, stride).ok()
}

fn place_overlay(window: &ApplicationWindow) {
    let Some(api) = crate::gui::layer_api() else {
        window.fullscreen();
        return;
    };
    unsafe {
        let win = window.as_ptr() as *mut c_void;
        (api.init_for_window)(win);
        let ns = CString::new("hypergsr-region").unwrap();
        (api.set_namespace)(win, ns.as_ptr());
        (api.set_layer)(win, 3); // OVERLAY
        for edge in 0..4 {
            (api.set_anchor)(win, edge, 1);
        }
        (api.set_exclusive_zone)(win, -1);
        (api.set_keyboard_mode)(win, 1); // EXCLUSIVE, so Esc always reaches us
    }
}

/// Opens the overlay; `on_done` receives the region, or `None` when the user
/// cancels with Esc or a right click.
pub fn select_region(app: &Application, on_done: impl Fn(Option<Region>) + 'static) {
    if crate::gui::layer_api().is_some() {
        return open_overlay(app, None, on_done);
    }
    let app = app.clone();
    portal_screenshot(move |path| {
        let backdrop = path.as_deref().and_then(load_backdrop);
        if backdrop.is_none() {
            eprintln!("[HyperGSR] Screenshot portal gave no image; the area picker has no preview");
        }
        open_overlay(&app, backdrop, on_done);
    });
}

fn open_overlay(app: &Application, backdrop: Option<ImageSurface>, on_done: impl Fn(Option<Region>) + 'static) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("HyperGSR Region")
        .decorated(false)
        .build();
    place_overlay(&window);

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_cursor_from_name(Some("crosshair"));
    window.set_child(Some(&area));

    let sel = Rc::new(RefCell::new(Selection::default()));
    let finished = Rc::new(Cell::new(false));
    let on_done = Rc::new(on_done);
    let finish = {
        let window = window.clone();
        let finished = finished.clone();
        let on_done = on_done.clone();
        Rc::new(move |region: Option<Region>| {
            if finished.replace(true) {
                return;
            }
            window.close();
            on_done(region);
        })
    };
    {
        let finish = finish.clone();
        window.connect_close_request(move |_| {
            finish(None);
            glib::Propagation::Proceed
        });
    }

    {
        let sel = sel.clone();
        let window = window.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (w as f64, h as f64);
            let sel = sel.borrow();
            if let Some(shot) = &backdrop {
                cr.save().ok();
                cr.scale(w / shot.width().max(1) as f64, h / shot.height().max(1) as f64);
                cr.set_source_surface(shot, 0.0, 0.0).ok();
                cr.source().set_filter(gtk4::cairo::Filter::Good);
                cr.paint().ok();
                cr.restore().ok();
            }
            cr.set_source_rgba(0.02, 0.02, 0.05, 0.55);
            cr.rectangle(0.0, 0.0, w, h);
            if let Some((x, y, rw, rh)) = sel.rect() {
                cr.rectangle(x, y, rw, rh);
                cr.set_fill_rule(gtk4::cairo::FillRule::EvenOdd);
            }
            cr.fill().ok();

            if let Some((x, y, rw, rh)) = sel.rect() {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                cr.set_line_width(1.5);
                cr.rectangle(x + 0.75, y + 0.75, (rw - 1.5).max(0.0), (rh - 1.5).max(0.0));
                cr.stroke().ok();
                // Corner handles.
                for (hx, hy) in [(x, y), (x + rw, y), (x, y + rh), (x + rw, y + rh)] {
                    cr.arc(hx, hy, 4.0, 0.0, std::f64::consts::TAU);
                    cr.fill().ok();
                }
                let scale = window.scale_factor().max(1) as f64;
                let label = format!("{} × {}", (rw * scale).round() as i64, (rh * scale).round() as i64);
                cr.select_font_face("sans-serif", gtk4::cairo::FontSlant::Normal, gtk4::cairo::FontWeight::Bold);
                cr.set_font_size(13.0);
                if let Ok(ext) = cr.text_extents(&label) {
                    let lx = (x + rw - ext.width() - 18.0).max(x + 4.0);
                    let ly = if y + rh + 30.0 < h { y + rh + 8.0 } else { y + rh - 30.0 };
                    crate::gui::draw_rounded_rect(cr, lx, ly, ext.width() + 14.0, 22.0, 6.0);
                    cr.set_source_rgba(0.06, 0.06, 0.1, 0.9);
                    cr.fill().ok();
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.move_to(lx + 7.0, ly + 15.5);
                    cr.show_text(&label).ok();
                }
            }

            let hint = "Drag to select the area to record   ·   Esc to cancel";
            cr.select_font_face("sans-serif", gtk4::cairo::FontSlant::Normal, gtk4::cairo::FontWeight::Bold);
            cr.set_font_size(14.0);
            if let Ok(ext) = cr.text_extents(hint) {
                let bw = ext.width() + 28.0;
                let bx = (w - bw) / 2.0;
                crate::gui::draw_rounded_rect(cr, bx, h * 0.12, bw, 34.0, 17.0);
                cr.set_source_rgba(0.06, 0.06, 0.1, 0.88);
                cr.fill().ok();
                cr.set_source_rgb(0.95, 0.95, 0.98);
                cr.move_to(bx + 14.0, h * 0.12 + 22.0);
                cr.show_text(hint).ok();
            }
        });
    }

    let drag = GestureDrag::new();
    drag.set_button(gdk4::BUTTON_PRIMARY);
    {
        let sel = sel.clone();
        let area = area.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let mut s = sel.borrow_mut();
            s.start = Some((x, y));
            s.end = (x, y);
            area.queue_draw();
        });
    }
    {
        let sel = sel.clone();
        let area = area.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let mut s = sel.borrow_mut();
            if let Some((sx, sy)) = s.start {
                let (w, h) = (area.width() as f64, area.height() as f64);
                s.end = ((sx + dx).clamp(0.0, w), (sy + dy).clamp(0.0, h));
            }
            area.queue_draw();
        });
    }
    {
        let sel = sel.clone();
        let area_ref = area.clone();
        let finish = finish.clone();
        drag.connect_drag_end(move |_, _, _| {
            let rect = sel.borrow().rect();
            let (w, h) = (area_ref.width().max(1) as f64, area_ref.height().max(1) as f64);
            match rect {
                // A click without a drag is not a selection; let the user try again.
                Some((_, _, rw, rh)) if rw < 16.0 || rh < 16.0 => {
                    sel.borrow_mut().start = None;
                    area_ref.queue_draw();
                }
                Some((x, y, rw, rh)) => finish(
                    Region {
                        x: (x / w) as f32,
                        y: (y / h) as f32,
                        w: (rw / w) as f32,
                        h: (rh / h) as f32,
                    }
                    .normalized(),
                ),
                None => {}
            }
        });
    }
    area.add_controller(drag);

    let cancel_click = GestureClick::new();
    cancel_click.set_button(gdk4::BUTTON_SECONDARY);
    {
        let finish = finish.clone();
        cancel_click.connect_pressed(move |_, _, _, _| finish(None));
    }
    area.add_controller(cancel_click);

    let keys = EventControllerKey::new();
    {
        let finish = finish.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk4::Key::Escape {
                finish(None);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);
    window.present();
}
