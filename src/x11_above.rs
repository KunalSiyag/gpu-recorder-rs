//! Keeps the island above other windows and on every workspace when GTK runs
//! on X11 (including XWayland on GNOME, which has no layer-shell). GTK 4 has
//! no API for this, so the EWMH state is set directly on the window.

use gtk4::prelude::*;
use std::ffi::{c_void, CString};
use x11_dl::xlib::{self, Xlib};

type GetXid = unsafe extern "C" fn(*mut c_void) -> std::os::raw::c_ulong;
type GetXDisplay = unsafe extern "C" fn(*mut c_void) -> *mut xlib::Display;

unsafe fn gdk_symbol(name: &str) -> *mut c_void {
    let name = CString::new(name).unwrap();
    libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr())
}

fn is_x11(window: &gtk4::ApplicationWindow) -> bool {
    WidgetExt::display(window).type_().name() == "GdkX11Display"
}

/// Asks the window manager to keep `window` above others, on all
/// workspaces, and out of the taskbar, and docks it at the top center of
/// its monitor. Does nothing outside X11.
pub fn keep_above(window: &gtk4::ApplicationWindow) {
    window.connect_map(|win| {
        if !is_x11(win) {
            return;
        }
        let win = win.clone();
        // The X window exists once mapped; apply on the next idle so GTK has
        // finished its own setup.
        glib::idle_add_local_once(move || unsafe { apply(&win) });
    });
}

unsafe fn apply(window: &gtk4::ApplicationWindow) {
    let (Some(surface), Ok(xlib)) = (window.surface(), Xlib::open()) else {
        return;
    };
    let get_xid = gdk_symbol("gdk_x11_surface_get_xid");
    let get_xdisplay = gdk_symbol("gdk_x11_display_get_xdisplay");
    if get_xid.is_null() || get_xdisplay.is_null() {
        return;
    }
    let get_xid: GetXid = std::mem::transmute(get_xid);
    let get_xdisplay: GetXDisplay = std::mem::transmute(get_xdisplay);
    use glib::translate::ToGlibPtr;
    let surface_ptr: *mut gdk4::ffi::GdkSurface = surface.to_glib_none().0;
    let gdk_display = WidgetExt::display(window);
    let display_ptr: *mut gdk4::ffi::GdkDisplay = gdk_display.to_glib_none().0;
    let xid = get_xid(surface_ptr.cast());
    let dpy = get_xdisplay(display_ptr.cast());
    if xid == 0 || dpy.is_null() {
        return;
    }

    let atom = |name: &str| {
        let c = CString::new(name).unwrap();
        (xlib.XInternAtom)(dpy, c.as_ptr(), xlib::False)
    };
    let root = (xlib.XDefaultRootWindow)(dpy);
    let net_wm_state = atom("_NET_WM_STATE");
    for state in ["_NET_WM_STATE_ABOVE", "_NET_WM_STATE_STICKY", "_NET_WM_STATE_SKIP_TASKBAR", "_NET_WM_STATE_SKIP_PAGER"] {
        let mut ev: xlib::XEvent = std::mem::zeroed();
        ev.client_message = xlib::XClientMessageEvent {
            type_: xlib::ClientMessage,
            serial: 0,
            send_event: xlib::True,
            display: dpy,
            window: xid,
            message_type: net_wm_state,
            format: 32,
            data: xlib::ClientMessageData::from([1, atom(state) as i64, 0, 1, 0]),
        };
        (xlib.XSendEvent)(
            dpy,
            root,
            xlib::False,
            xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask,
            &mut ev,
        );
    }
    // Also ask to appear on every desktop (EWMH: 0xFFFFFFFF = all).
    let mut ev: xlib::XEvent = std::mem::zeroed();
    ev.client_message = xlib::XClientMessageEvent {
        type_: xlib::ClientMessage,
        serial: 0,
        send_event: xlib::True,
        display: dpy,
        window: xid,
        message_type: atom("_NET_WM_DESKTOP"),
        format: 32,
        data: xlib::ClientMessageData::from([0xFFFF_FFFFi64, 1, 0, 0, 0]),
    };
    (xlib.XSendEvent)(dpy, root, xlib::False, xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask, &mut ev);

    if let Some(monitor) = gdk_display.monitor_at_surface(&surface) {
        let geo = monitor.geometry();
        let scale = surface.scale_factor().max(1);
        let x = (geo.x() + (geo.width() - window.width()) / 2) * scale;
        let y = (geo.y() + crate::island_position::DEFAULT_TOP_MARGIN) * scale;
        (xlib.XMoveWindow)(dpy, xid, x, y);
    }
    (xlib.XFlush)(dpy);
}
