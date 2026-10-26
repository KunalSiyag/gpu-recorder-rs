//! Studio drawer cards for what to record (source, audio, frame rate,
//! quality) and how to present it (background, padding, shadow).

use crate::backgrounds::{swatch_colors, PRESETS};
use crate::gui::{draw_rounded_rect, icon_button, set_active_segment, set_btn_text};
use crate::settings::{AudioMode, BackgroundKind, CaptureSource, Quality, RecorderSettings, Webcam};
use crate::state::SharedStudioState;
use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, CssProvider, DrawingArea, Label, Orientation};
use std::rc::Rc;

const PANEL_CSS: &str = r#"
button.swatch {
    padding: 2px;
    min-width: 0;
    min-height: 0;
    border-radius: 8px;
    border: 2px solid transparent;
    background-color: transparent;
}
button.swatch:hover { border-color: rgba(255, 255, 255, 0.25); }
button.swatch-active { border-color: #818cf8; }
.theme-light button.swatch-active { border-color: #4f46e5; }
.panel-hint {
    font-size: 9.5px;
    color: #94a3b8;
}
"#;

/// Size of the "Large" cursor relative to the desktop's.
const CURSOR_LARGE: f32 = 1.6;

fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_data(PANEL_CSS);
    if let Some(display) = gdk4::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
    }
}

fn caption(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("section-cap");
    label.set_halign(Align::Start);
    label
}

/// A segmented control; `on_pick` runs with the chosen index. A label of
/// the form `"icon:Text"` gets that icon.
pub(crate) fn segments(labels: &[&str], active: usize, on_pick: impl Fn(usize) + 'static) -> (GtkBox, Vec<Button>) {
    let track = GtkBox::new(Orientation::Horizontal, 3);
    track.add_css_class("seg-track");
    let buttons: Vec<Button> = labels
        .iter()
        .map(|l| {
            let b = match l.split_once(':') {
                Some((icon, text)) => icon_button(icon, text),
                None => Button::with_label(l),
            };
            b.set_hexpand(true);
            track.append(&b);
            b
        })
        .collect();
    set_active_segment(&buttons, active);
    let on_pick = Rc::new(on_pick);
    for (idx, b) in buttons.iter().enumerate() {
        let all = buttons.clone();
        let on_pick = on_pick.clone();
        b.connect_clicked(move |_| {
            set_active_segment(&all, idx);
            on_pick(idx);
        });
    }
    (track, buttons)
}

fn update(state: &SharedStudioState, change: impl FnOnce(&mut RecorderSettings)) {
    let mut g = state.lock();
    change(&mut g.settings);
    g.auto_apply_zoom_on_stop = g.settings.auto_zoom;
    g.record_audio = g.settings.audio != AudioMode::Off;
    g.target_fps = g.settings.fps;
    g.settings.save();
}

fn source_hint(settings: &RecorderSettings) -> String {
    match (settings.source, settings.region) {
        (CaptureSource::Screen, _) => "Records a whole screen".to_string(),
        // Window zoom follows the window's own cursor track.
        (CaptureSource::Window, _) if settings.show_cursor && crate::encoder::portal_cursor_metadata() => {
            "Records one window".to_string()
        }
        (CaptureSource::Window, _) => "Records one window (auto-zoom off)".to_string(),
        (CaptureSource::Region, Some(r)) => format!(
            "Area {:.0}% × {:.0}% at ({:.0}%, {:.0}%)",
            r.w * 100.0,
            r.h * 100.0,
            r.x * 100.0,
            r.y * 100.0
        ),
        (CaptureSource::Region, None) => "No area selected yet".to_string(),
    }
}

fn swatch(draw: impl Fn(&gtk4::cairo::Context, f64, f64) + 'static) -> Button {
    let area = DrawingArea::new();
    area.set_content_width(32);
    area.set_content_height(22);
    area.set_draw_func(move |_, cr, w, h| {
        draw_rounded_rect(cr, 0.0, 0.0, w as f64, h as f64, 5.0);
        cr.clip();
        draw(cr, w as f64, h as f64);
    });
    let b = Button::new();
    b.set_child(Some(&area));
    b.add_css_class("swatch");
    b
}

fn set_active_swatch(buttons: &[Button], active: Option<usize>) {
    for (i, b) in buttons.iter().enumerate() {
        if Some(i) == active {
            b.add_css_class("swatch-active");
        } else {
            b.remove_css_class("swatch-active");
        }
    }
}

/// `~/…` for folders under home, so the row stays short.
pub fn display_dir(dir: &std::path::Path) -> String {
    let home = crate::bundled::home_dir();
    match dir.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => dir.display().to_string(),
    }
}

/// Fails unless a file can be created in `dir`.
fn check_writable(dir: &std::path::Path) -> std::io::Result<()> {
    let probe = dir.join(format!(".hypergsr-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

/// Copies a user-picked image into the config directory, so the background
/// survives the file being moved or a portal document grant expiring.
fn adopt_background_image(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| ["png", "jpg", "jpeg"].contains(&e.as_str()))
        .unwrap_or_else(|| "png".to_string());
    let dir = crate::bundled::config_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("background.{ext}"));
    std::fs::copy(path, &dest)?;
    Ok(dest)
}

/// Builds the Recording and Background cards. `footer` shows confirmations.
pub fn build(state: &SharedStudioState, footer: &Label) -> GtkBox {
    load_css();
    let settings = state.lock().settings.clone();
    let column = GtkBox::new(Orientation::Vertical, 7);

    // ---------------------------------------------------------------------
    // Recording
    // ---------------------------------------------------------------------
    let rec_card = GtkBox::new(Orientation::Vertical, 6);
    rec_card.add_css_class("bento-card");
    rec_card.append(&caption("RECORD"));

    let hint = Label::new(Some(&source_hint(&settings)));
    hint.add_css_class("panel-hint");
    hint.set_halign(Align::Start);
    hint.set_hexpand(true);
    hint.set_ellipsize(gtk4::pango::EllipsizeMode::End);

    let repick = icon_button("choose", "Choose…");
    repick.add_css_class("action-chip");
    repick.set_tooltip_text(Some("Pick a different screen, window, or area"));

    let source_idx = match settings.source {
        CaptureSource::Screen => 0,
        CaptureSource::Window => 1,
        CaptureSource::Region => 2,
    };
    let (source_track, source_btns) = segments(&["screen:Screen", "window:Window", "area:Area"], source_idx, |_| {});
    rec_card.append(&source_track);
    let source_row = GtkBox::new(Orientation::Horizontal, 6);
    source_row.append(&hint);
    source_row.append(&repick);
    rec_card.append(&source_row);

    // Asks for an area; on cancel, falls back to the previous source.
    let pick_region: Rc<dyn Fn()> = {
        let st = state.clone();
        let hint = hint.clone();
        let footer = footer.clone();
        let btns = source_btns.clone();
        let anchor = rec_card.clone();
        Rc::new(move || {
            let Some(app) = anchor
                .root()
                .and_then(|r| r.downcast::<gtk4::Window>().ok())
                .and_then(|w| w.application())
            else {
                return;
            };
            let st = st.clone();
            let hint = hint.clone();
            let footer = footer.clone();
            let btns = btns.clone();
            crate::region_select::select_region(&app, move |region| {
                match region {
                    Some(r) => {
                        update(&st, |s| {
                            s.source = CaptureSource::Region;
                            s.region = Some(r);
                        });
                        footer.set_text("✓ Area selected for the next recording");
                    }
                    None => footer.set_text("Area selection cancelled"),
                }
                let s = st.lock().settings.clone();
                if s.source == CaptureSource::Region && s.region.is_none() {
                    update(&st, |s| s.source = CaptureSource::Screen);
                }
                let s = st.lock().settings.clone();
                set_active_segment(
                    &btns,
                    match s.source {
                        CaptureSource::Screen => 0,
                        CaptureSource::Window => 1,
                        CaptureSource::Region => 2,
                    },
                );
                hint.set_text(&source_hint(&s));
            });
        })
    };

    for (idx, btn) in source_btns.iter().enumerate() {
        let st = state.clone();
        let hint = hint.clone();
        let footer = footer.clone();
        let pick_region = pick_region.clone();
        btn.connect_clicked(move |_| {
            let source = [CaptureSource::Screen, CaptureSource::Window, CaptureSource::Region][idx];
            let had_region = st.lock().settings.region.is_some();
            update(&st, |s| s.source = source);
            hint.set_text(&source_hint(&st.lock().settings));
            match source {
                CaptureSource::Region if !had_region => pick_region(),
                CaptureSource::Region => footer.set_text("✓ Recording the selected area"),
                CaptureSource::Window => footer.set_text("✓ The first window recording asks which window"),
                CaptureSource::Screen => footer.set_text("✓ Recording a whole screen"),
            }
        });
    }
    {
        let st = state.clone();
        let footer = footer.clone();
        repick.connect_clicked(move |_| {
            let source = st.lock().settings.source;
            if source == CaptureSource::Region {
                pick_region();
                return;
            }
            st.lock().repick_source = true;
            footer.set_text(match source {
                CaptureSource::Window => "✓ The next recording asks which window to share",
                _ => "✓ The next recording asks which screen to share",
            });
        });
    }

    let audio_idx = match settings.audio {
        AudioMode::Off => 0,
        AudioMode::System => 1,
        AudioMode::Microphone => 2,
        AudioMode::Both => 3,
    };
    let audio_row = GtkBox::new(Orientation::Horizontal, 6);
    let (audio_track, _) = {
        let st = state.clone();
        let footer = footer.clone();
        segments(&["audio-off:Off", "audio-system:System", "audio-mic:Mic", "audio-both:Both"], audio_idx, move |i| {
            let mode = [AudioMode::Off, AudioMode::System, AudioMode::Microphone, AudioMode::Both][i];
            update(&st, |s| s.audio = mode);
            let pulse = crate::ffmpeg::capabilities().pulse;
            footer.set_text(match (mode, pulse) {
                (AudioMode::Off, _) => "○ Recording without audio",
                (_, false) => "FFmpeg lacks PulseAudio input, so audio is off",
                (AudioMode::System, _) => "✓ Recording desktop audio",
                (AudioMode::Microphone, _) => "✓ Recording the microphone",
                (AudioMode::Both, _) => "✓ Recording desktop audio and microphone",
            });
        })
    };
    audio_track.set_hexpand(true);
    audio_row.append(&audio_track);
    let clean_label = |on: bool| if on { "● Clean" } else { "○ Clean" };
    let clean_btn = Button::with_label(clean_label(settings.clean_mic));
    clean_btn.add_css_class(if settings.clean_mic { "toggle-chip-on" } else { "toggle-chip" });
    clean_btn.set_tooltip_text(Some(
        "Noise reduction for the microphone: removes fan noise, hum, and hiss, and low rumble",
    ));
    {
        let st = state.clone();
        let footer = footer.clone();
        clean_btn.connect_clicked(move |btn| {
            update(&st, |s| s.clean_mic = !s.clean_mic);
            let on = st.lock().settings.clean_mic;
            set_btn_text(btn, clean_label(on));
            btn.remove_css_class(if on { "toggle-chip" } else { "toggle-chip-on" });
            btn.add_css_class(if on { "toggle-chip-on" } else { "toggle-chip" });
            footer.set_text(if on {
                "✓ Microphone noise reduction on"
            } else {
                "○ The microphone is recorded as is"
            });
        });
    }
    audio_row.append(&clean_btn);
    rec_card.append(&audio_row);

    let quality_row = GtkBox::new(Orientation::Horizontal, 6);
    let (fps_track, _) = {
        let st = state.clone();
        let footer = footer.clone();
        segments(&["30 FPS", "60 FPS"], usize::from(settings.fps > 30), move |i| {
            let fps = if i == 0 { 30 } else { 60 };
            update(&st, |s| s.fps = fps);
            footer.set_text(&format!("✓ {fps} frames per second"));
        })
    };
    let (q_track, _) = {
        let st = state.clone();
        let footer = footer.clone();
        segments(&["Compact", "High"], usize::from(settings.quality == Quality::High), move |i| {
            let q = if i == 0 { Quality::Standard } else { Quality::High };
            update(&st, |s| s.quality = q);
            footer.set_text(if i == 0 { "✓ Compact files" } else { "✓ High quality" });
        })
    };
    fps_track.set_hexpand(true);
    q_track.set_hexpand(true);
    quality_row.append(&fps_track);
    quality_row.append(&q_track);
    rec_card.append(&quality_row);

    // The cursor is drawn by the studio render when the portal sends it
    // separately, which smooths its path and allows a larger size.
    let cursor_row = GtkBox::new(Orientation::Horizontal, 6);
    cursor_row.append(&crate::gui::icon("cursor", 14));
    let cursor_idx = match (settings.show_cursor, settings.cursor_scale > 1.2) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 2,
    };
    let (cursor_track, _) = {
        let st = state.clone();
        let footer = footer.clone();
        segments(&["Hidden", "Normal", "Large"], cursor_idx, move |i| {
            update(&st, |s| {
                s.show_cursor = i > 0;
                s.cursor_scale = if i == 2 { CURSOR_LARGE } else { 1.0 };
            });
            let studio = crate::encoder::portal_cursor_metadata();
            footer.set_text(match (i, studio) {
                (0, _) => "○ The cursor is left out of recordings",
                (_, false) => "Your desktop draws the cursor into the video, so it stays as is",
                (1, true) => "✓ Smooth cursor at its normal size",
                _ => "✓ Smooth cursor, drawn larger",
            });
        })
    };
    cursor_track.set_hexpand(true);
    cursor_track.set_tooltip_text(Some(
        "When your desktop supports it, the cursor is recorded separately and drawn \
         back on a smoothed path, so it glides instead of jittering",
    ));
    cursor_row.append(&cursor_track);
    let ripple_label = |on: bool| if on { "● Clicks" } else { "○ Clicks" };
    let ripple_btn = Button::with_label(ripple_label(settings.click_ripples));
    ripple_btn.add_css_class(if settings.click_ripples { "toggle-chip-on" } else { "toggle-chip" });
    let clicks_seen = state.lock().cursor_tracker.reads_mouse_buttons();
    ripple_btn.set_tooltip_text(Some(if clicks_seen {
        "Shows a ripple where you click"
    } else {
        "Shows a ripple where you click. On Wayland, clicks are only seen when you may read \
         /dev/input: add yourself to the input group (sudo usermod -aG input $USER) and log in again"
    }));
    {
        let st = state.clone();
        let footer = footer.clone();
        ripple_btn.connect_clicked(move |btn| {
            update(&st, |s| s.click_ripples = !s.click_ripples);
            let (on, seen) = {
                let g = st.lock();
                (g.settings.click_ripples, g.cursor_tracker.reads_mouse_buttons())
            };
            set_btn_text(btn, ripple_label(on));
            btn.remove_css_class(if on { "toggle-chip" } else { "toggle-chip-on" });
            btn.add_css_class(if on { "toggle-chip-on" } else { "toggle-chip" });
            footer.set_text(match (on, seen) {
                (false, _) => "○ No click ripples",
                (true, true) => "✓ Clicks show a ripple",
                (true, false) => "✓ Click ripples on; join the input group so Wayland clicks are seen",
            });
        });
    }
    cursor_row.append(&ripple_btn);
    rec_card.append(&cursor_row);

    // The webcam, recorded beside the screen and drawn as a bubble.
    let webcam_row = GtkBox::new(Orientation::Horizontal, 6);
    webcam_row.append(&crate::gui::icon("webcam", 14));
    let camera = crate::webcam::default_device();
    let webcam_idx = match settings.webcam {
        Webcam::Off => 0,
        Webcam::Left => 1,
        Webcam::Right => 2,
    };
    let (webcam_track, _) = {
        let st = state.clone();
        let footer = footer.clone();
        segments(&["No webcam", "Bottom left", "Bottom right"], webcam_idx, move |i| {
            let corner = [Webcam::Off, Webcam::Left, Webcam::Right][i];
            update(&st, |s| s.webcam = corner);
            footer.set_text(if corner == Webcam::Off {
                "○ No webcam"
            } else {
                "✓ Your webcam appears as a round bubble in the saved video"
            });
        })
    };
    webcam_track.set_hexpand(true);
    match &camera {
        Some(cam) => webcam_track.set_tooltip_text(Some(&format!(
            "Records {} beside the screen and places it as a round bubble when saving",
            cam.name
        ))),
        None => {
            webcam_track.set_sensitive(false);
            webcam_track.set_tooltip_text(Some("No webcam found"));
        }
    }
    webcam_row.append(&webcam_track);
    rec_card.append(&webcam_row);

    // A countdown on the running capture, cut from the video afterwards.
    let countdown_row = GtkBox::new(Orientation::Horizontal, 6);
    countdown_row.append(&crate::gui::icon("timer", 14));
    let countdown_idx = match settings.countdown_sec {
        0 => 0,
        1..=3 => 1,
        _ => 2,
    };
    let (countdown_track, _) = {
        let st = state.clone();
        let footer = footer.clone();
        segments(&["No countdown", "3 s", "5 s"], countdown_idx, move |i| {
            let sec = [0, 3, 5][i];
            update(&st, |s| s.countdown_sec = sec);
            footer.set_text(&if sec == 0 {
                "○ Recording starts at once".to_string()
            } else {
                format!("✓ Recording starts after a {sec}-second countdown; press Stop during it to cancel")
            });
        })
    };
    countdown_track.set_hexpand(true);
    countdown_track.set_tooltip_text(Some("Counts down on the island once the screen share is running"));
    countdown_row.append(&countdown_track);
    let hide_label = |on: bool| if on { "● Hide bar" } else { "○ Hide bar" };
    let hide_btn = Button::with_label(hide_label(settings.hide_while_recording));
    hide_btn.add_css_class(if settings.hide_while_recording { "toggle-chip-on" } else { "toggle-chip" });
    hide_btn.set_tooltip_text(Some(&format!(
        "Hides this bar while recording so it is not in the video. {} stops and {} pauses \
         (the bar comes back while paused), or use the notification's buttons",
        crate::shortcuts::DEFAULT_LABEL,
        crate::shortcuts::PAUSE_LABEL
    )));
    {
        let st = state.clone();
        let footer = footer.clone();
        hide_btn.connect_clicked(move |btn| {
            update(&st, |s| s.hide_while_recording = !s.hide_while_recording);
            let on = st.lock().settings.hide_while_recording;
            set_btn_text(btn, hide_label(on));
            btn.remove_css_class(if on { "toggle-chip" } else { "toggle-chip-on" });
            btn.add_css_class(if on { "toggle-chip-on" } else { "toggle-chip" });
            footer.set_text(&if on {
                format!(
                    "✓ The bar hides while recording; {} stops, {} pauses",
                    crate::shortcuts::DEFAULT_LABEL,
                    crate::shortcuts::PAUSE_LABEL
                )
            } else {
                "○ The bar stays on screen while recording".to_string()
            });
        });
    }
    countdown_row.append(&hide_btn);
    rec_card.append(&countdown_row);

    // Start/stop and pause from anywhere with desktop keybindings.
    let shortcut_row = GtkBox::new(Orientation::Horizontal, 6);
    shortcut_row.append(&crate::gui::icon("shortcut", 14));
    let shortcut_lbl = Label::new(Some(&format!(
        "Record {}  ·  Pause {}",
        crate::shortcuts::DEFAULT_LABEL,
        crate::shortcuts::PAUSE_LABEL
    )));
    shortcut_lbl.add_css_class("panel-hint");
    shortcut_lbl.set_hexpand(true);
    shortcut_lbl.set_halign(Align::Start);
    shortcut_row.append(&shortcut_lbl);
    let ready = crate::shortcuts::all_installed();
    let shortcut_btn = icon_button(if ready { "check" } else { "keyboard" }, if ready { "Ready" } else { "Set up" });
    shortcut_btn.add_css_class(if ready { "toggle-chip-on" } else { "action-chip" });
    shortcut_btn.set_tooltip_text(Some(&format!(
        "Adds {} (start / stop) and {} (pause) to your desktop shortcuts. Start / stop runs: {}",
        crate::shortcuts::DEFAULT_LABEL,
        crate::shortcuts::PAUSE_LABEL,
        crate::shortcuts::toggle_command()
    )));
    {
        let footer = footer.clone();
        shortcut_btn.connect_clicked(move |btn| match crate::shortcuts::install() {
            Ok(msg) => {
                set_btn_text(btn, "Ready");
                btn.remove_css_class("action-chip");
                btn.add_css_class("toggle-chip-on");
                footer.set_text(&format!("✓ {msg}"));
            }
            Err(err) => footer.set_text(&format!("{err:#}")),
        });
    }
    shortcut_row.append(&shortcut_btn);
    rec_card.append(&shortcut_row);

    // Where recordings are saved.
    let folder_row = GtkBox::new(Orientation::Horizontal, 6);
    folder_row.append(&crate::gui::icon("folder", 15));
    let folder_lbl = Label::new(Some(&format!("Save to  {}", display_dir(&settings.recordings_dir()))));
    folder_lbl.add_css_class("panel-hint");
    folder_lbl.set_hexpand(true);
    folder_lbl.set_halign(Align::Start);
    folder_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    folder_lbl.set_tooltip_text(Some(&settings.recordings_dir().display().to_string()));
    folder_row.append(&folder_lbl);
    let folder_btn = icon_button("folder", "Change…");
    folder_btn.add_css_class("action-chip");
    {
        let st = state.clone();
        let footer = footer.clone();
        let folder_lbl = folder_lbl.clone();
        folder_btn.connect_clicked(move |btn| {
            let dialog = gtk4::FileDialog::builder()
                .title("Save recordings to")
                .modal(true)
                .initial_folder(&gtk4::gio::File::for_path(st.lock().videos_dir.clone()))
                .build();
            let parent = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            let st = st.clone();
            let footer = footer.clone();
            let folder_lbl = folder_lbl.clone();
            dialog.select_folder(parent.as_ref(), gtk4::gio::Cancellable::NONE, move |res| {
                let Some(dir) = res.ok().and_then(|f| f.path()) else {
                    return;
                };
                if let Err(err) = check_writable(&dir) {
                    footer.set_text(&format!("Can't save there: {err}"));
                    return;
                }
                let default = crate::bundled::home_dir().join("Videos");
                update(&st, |s| s.save_dir = (dir != default).then(|| dir.clone()));
                st.lock().videos_dir = dir.clone();
                folder_lbl.set_text(&format!("Save to  {}", display_dir(&dir)));
                folder_lbl.set_tooltip_text(Some(&dir.display().to_string()));
                footer.set_text(&format!("✓ Recordings will be saved to {}", display_dir(&dir)));
            });
        });
    }
    folder_row.append(&folder_btn);
    rec_card.append(&folder_row);
    column.append(&rec_card);

    // ---------------------------------------------------------------------
    // Background
    // ---------------------------------------------------------------------
    let bg_card = GtkBox::new(Orientation::Vertical, 6);
    bg_card.add_css_class("bento-card");
    bg_card.append(&caption("PRESENTATION BACKGROUND"));

    let swatch_row = GtkBox::new(Orientation::Horizontal, 2);
    let mut swatches = vec![swatch(|cr, w, h| {
        // "None": a crossed-out frame.
        cr.set_source_rgba(0.5, 0.5, 0.55, 0.25);
        cr.paint().ok();
        cr.set_source_rgba(0.9, 0.3, 0.35, 0.9);
        cr.set_line_width(1.6);
        cr.move_to(3.0, h - 3.0);
        cr.line_to(w - 3.0, 3.0);
        cr.stroke().ok();
    })];
    swatches[0].set_tooltip_text(Some("No background"));
    for preset in PRESETS {
        let (a, b) = swatch_colors(preset);
        let glow = preset.glows.first().map(|g| g.3);
        let btn = swatch(move |cr, w, h| {
            let grad = gtk4::cairo::LinearGradient::new(0.0, 0.0, w, h);
            grad.add_color_stop_rgb(0.0, a[0], a[1], a[2]);
            grad.add_color_stop_rgb(1.0, b[0], b[1], b[2]);
            cr.set_source(&grad).ok();
            cr.paint().ok();
            if let Some(g) = glow {
                let rad = gtk4::cairo::RadialGradient::new(w * 0.2, h * 0.2, 0.0, w * 0.2, h * 0.2, w * 0.7);
                let c = |v: u8| v as f64 / 255.0;
                rad.add_color_stop_rgba(0.0, c(g[0]), c(g[1]), c(g[2]), 0.6);
                rad.add_color_stop_rgba(1.0, c(g[0]), c(g[1]), c(g[2]), 0.0);
                cr.set_source(&rad).ok();
                cr.paint().ok();
            }
        });
        btn.set_tooltip_text(Some(preset.name));
        swatches.push(btn);
    }
    let image_btn = swatch(|cr, w, h| {
        cr.set_source_rgba(0.5, 0.5, 0.55, 0.25);
        cr.paint().ok();
        cr.set_source_rgba(0.52, 0.55, 0.66, 0.95);
        cr.move_to(4.0, h - 4.0);
        cr.line_to(w * 0.42, h * 0.38);
        cr.line_to(w * 0.62, h * 0.62);
        cr.line_to(w * 0.75, h * 0.48);
        cr.line_to(w - 4.0, h - 4.0);
        cr.close_path();
        cr.fill().ok();
    });
    image_btn.set_tooltip_text(Some("Your own image…"));
    swatches.push(image_btn);
    for b in &swatches {
        swatch_row.append(b);
    }
    let image_idx = swatches.len() - 1;
    let active = match &settings.background.kind {
        BackgroundKind::None => 0,
        BackgroundKind::Preset(i) => i + 1,
        BackgroundKind::Image(_) => image_idx,
    };
    set_active_swatch(&swatches, Some(active));
    bg_card.append(&swatch_row);

    for (idx, btn) in swatches.iter().enumerate() {
        let st = state.clone();
        let footer = footer.clone();
        let all = swatches.clone();
        btn.connect_clicked(move |btn| {
            if idx == image_idx {
                let dialog = gtk4::FileDialog::builder().title("Background image").modal(true).build();
                let filter = gtk4::FileFilter::new();
                filter.set_name(Some("Images"));
                for mime in ["image/png", "image/jpeg"] {
                    filter.add_mime_type(mime);
                }
                let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                let parent = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                let st = st.clone();
                let footer = footer.clone();
                let all = all.clone();
                dialog.open(parent.as_ref(), gtk4::gio::Cancellable::NONE, move |res| {
                    let Some(path) = res.ok().and_then(|f| f.path()) else {
                        return;
                    };
                    match adopt_background_image(&path) {
                        Ok(dest) => {
                            update(&st, |s| s.background.kind = BackgroundKind::Image(dest));
                            set_active_swatch(&all, Some(image_idx));
                            footer.set_text("✓ Using your image as the background");
                        }
                        Err(err) => footer.set_text(&format!("Could not use that image: {err}")),
                    }
                });
                return;
            }
            let kind = if idx == 0 { BackgroundKind::None } else { BackgroundKind::Preset(idx - 1) };
            update(&st, |s| s.background.kind = kind);
            set_active_swatch(&all, Some(idx));
            footer.set_text(&if idx == 0 {
                "○ No background: the recording fills the frame".to_string()
            } else {
                format!("✓ Background: {}", PRESETS[idx - 1].name)
            });
        });
    }

    let frame_row = GtkBox::new(Orientation::Horizontal, 6);
    let padding_idx = match settings.background.padding {
        p if p < 0.045 => 0,
        p if p < 0.08 => 1,
        _ => 2,
    };
    let (pad_track, _) = {
        let st = state.clone();
        let footer = footer.clone();
        segments(&["Tight", "Balanced", "Roomy"], padding_idx, move |i| {
            let (padding, name) = [(0.035, "Tight"), (0.06, "Balanced"), (0.10, "Roomy")][i];
            update(&st, |s| s.background.padding = padding);
            footer.set_text(&format!("✓ Padding: {name}"));
        })
    };
    pad_track.set_hexpand(true);
    let shadow_btn = Button::with_label(if settings.background.shadow { "● Shadow" } else { "○ Shadow" });
    shadow_btn.add_css_class(if settings.background.shadow { "toggle-chip-on" } else { "toggle-chip" });
    {
        let st = state.clone();
        shadow_btn.connect_clicked(move |btn| {
            update(&st, |s| s.background.shadow = !s.background.shadow);
            let on = st.lock().settings.background.shadow;
            set_btn_text(&btn, if on { "● Shadow" } else { "○ Shadow" });
            btn.remove_css_class(if on { "toggle-chip" } else { "toggle-chip-on" });
            btn.add_css_class(if on { "toggle-chip-on" } else { "toggle-chip" });
        });
    }
    frame_row.append(&pad_track);
    frame_row.append(&shadow_btn);
    bg_card.append(&frame_row);
    column.append(&bg_card);
    column
}
