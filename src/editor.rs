//! The editor: trims a saved recording and exports it again, optionally in
//! another shape (16:9, 9:16, 1:1), another format (MP4, WebM, GIF), or with
//! other effects. Recent recordings re-render from their kept capture (see
//! `project`), so zoom, cursor, and background are applied afresh; any
//! other video is re-encoded as it is.

use crate::backgrounds::PRESETS;
use crate::gui::{icon_button, set_btn_text};
use crate::project::Project;
use crate::settings::{Aspect, BackgroundKind, BackgroundSettings, ExportFormat, Quality};
use crate::studio_panel::segments;
use crate::studio_render::{kept_ranges, RenderJob};
use gtk4::prelude::*;
use gtk4::{Align, Application, Box as GtkBox, Button, DrawingArea, GestureDrag, Label, Orientation};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const EDITOR_CSS: &str = r#"
window.editor-window { background-color: #0c0e13; color: #e4e4e7; }
headerbar.editor-header {
    background-color: #0c0e13;
    background-image: none;
    color: #e4e4e7;
    box-shadow: none;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    min-height: 42px;
}
headerbar.editor-header .title { font-weight: 650; color: #f1f5f9; }
.editor-root { padding: 14px 16px 16px 16px; }
.editor-times { font-feature-settings: "tnum"; font-size: 12px; color: #b8bfca; }
.editor-label { font-size: 11.5px; font-weight: 650; color: #aab3c0; min-width: 76px; }
.editor-note { font-size: 11px; color: #8b93a1; }
button.export-btn {
    background-image: linear-gradient(180deg, #6d70f5, #4f46e5);
    border-color: rgba(199, 210, 254, 0.35);
    color: #ffffff;
    font-weight: 650;
    padding: 6px 20px;
}
button.export-btn:disabled { opacity: 0.5; }
progressbar.editor-progress trough {
    min-height: 6px;
    border-radius: 3px;
    background-color: rgba(255, 255, 255, 0.08);
}
progressbar.editor-progress progress {
    min-height: 6px;
    border-radius: 3px;
    background-color: #818cf8;
}
dropdown.editor-drop button { padding: 3px 10px; }
"#;

fn load_css() {
    thread_local! { static LOADED: Cell<bool> = const { Cell::new(false) }; }
    if LOADED.with(|l| l.replace(true)) {
        return;
    }
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(EDITOR_CSS);
    if let Some(display) = gdk4::Display::default() {
        gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_USER + 2);
    }
}

/// Video time of the recording `t` seconds into a saved video whose frames
/// are the `kept` ranges played back to back.
fn source_time(kept: &[(f64, f64)], t: f64) -> f64 {
    let mut at = 0.0;
    for &(a, b) in kept {
        if t <= at + (b - a) {
            return a + (t - at).max(0.0);
        }
        at += b - a;
    }
    kept.last().map_or(t, |r| r.1)
}

/// The cuts that trim a `duration`-second saved video to `start..end`, on
/// top of the cuts it was made with.
pub fn trimmed_cuts(base: &[(f64, f64)], (start, end): (f64, f64), duration: f64) -> Vec<(f64, f64)> {
    let kept = kept_ranges(base, 1e9);
    let mut cuts = base.to_vec();
    if start > 0.005 {
        cuts.push((0.0, source_time(&kept, start)));
    }
    if end < duration - 0.005 {
        cuts.push((source_time(&kept, end), f64::INFINITY));
    }
    cuts
}

/// `HyperGSR_x.mp4` exports to `HyperGSR_x-vertical.webm` and so on, next
/// to it, never over an existing file.
fn export_path(file: &Path, aspect: Aspect, format: ExportFormat) -> PathBuf {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tag = match aspect {
        Aspect::Original => "edit",
        Aspect::Wide => "wide",
        Aspect::Vertical => "vertical",
        Aspect::Square => "square",
    };
    (1..)
        .map(|n| {
            let suffix = if n == 1 { String::new() } else { format!("-{n}") };
            file.with_file_name(format!("{stem}-{tag}{suffix}.{}", format.extension()))
        })
        .find(|p| !p.exists())
        .expect("an unused file name")
}

fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u64, t % 60.0)
}

struct Model {
    file: PathBuf,
    /// How the video was made, when its capture is still kept.
    project: Option<Project>,
    duration: f64,
    fps: u32,
    trim: (f64, f64),
    playhead: f64,
    aspect: Aspect,
    format: ExportFormat,
    zoom: bool,
    clicks: bool,
    /// 0: as recorded, 1: none, 2..: `PRESETS[i - 2]`.
    background: usize,
    thumbs: Vec<gtk4::cairo::ImageSurface>,
}

impl Model {
    fn job(&self) -> RenderJob {
        let mut job = match &self.project {
            Some(p) => p.job.clone(),
            None => RenderJob {
                capture: self.file.clone(),
                output: PathBuf::new(),
                events: Vec::new(),
                auto_zoom: false,
                region: None,
                background: BackgroundSettings::default(),
                quality: Quality::High,
                fps: self.fps,
                cursor: None,
                cursor_scale: 1.0,
                click_ripples: false,
                cuts: Vec::new(),
                aspect: Aspect::Original,
                format: ExportFormat::Mp4,
                webcam: None,
            },
        };
        job.cuts = trimmed_cuts(&job.cuts, self.trim, self.duration);
        if let Some(p) = &self.project {
            job.auto_zoom = self.zoom && p.zoomable;
            job.click_ripples = self.clicks;
        }
        job.background = match self.background {
            0 => job.background,
            1 => BackgroundSettings { kind: BackgroundKind::None, ..job.background },
            i => {
                let kind = BackgroundKind::Preset((i - 2).min(PRESETS.len() - 1));
                // A video that already has its frame keeps a little room.
                BackgroundSettings { kind, ..job.background }
            }
        };
        job.aspect = self.aspect;
        job.format = self.format;
        job.output = export_path(&self.file, self.aspect, self.format);
        job
    }
}

/// Timeline frames: width, height, and BGRx rows.
type Thumbnails = Vec<(usize, usize, Vec<u8>)>;

/// Frames for the timeline, `count` evenly spaced, `h` pixels tall.
fn thumbnails(file: &Path, duration: f64, size: (usize, usize), count: usize, h: usize) -> Thumbnails {
    use rayon::prelude::*;
    let w = ((h * size.0 / size.1.max(1)) / 2 * 2).max(2);
    (0..count)
        .into_par_iter()
        .filter_map(|i| {
            let t = duration * (i as f64 + 0.5) / count as f64;
            let out = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{t:.3}"), "-i"])
                .arg(file)
                .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            (out.stdout.len() == w * h * 4).then_some((w, h, out.stdout))
        })
        .collect()
}

fn draw_timeline(cr: &gtk4::cairo::Context, width: f64, height: f64, m: &Model) {
    use crate::gui::draw_rounded_rect;
    let dur = m.duration.max(0.001);
    let x_of = |t: f64| (t / dur * width).clamp(0.0, width);
    let strip_y = 6.0;
    let strip_h = height - 12.0;

    cr.save().ok();
    draw_rounded_rect(cr, 0.0, strip_y, width, strip_h, 8.0);
    cr.clip();
    cr.set_source_rgb(0.08, 0.09, 0.12);
    cr.paint().ok();
    if !m.thumbs.is_empty() {
        let tile = width / m.thumbs.len() as f64;
        for (i, surf) in m.thumbs.iter().enumerate() {
            let scale = strip_h / surf.height() as f64;
            cr.save().ok();
            cr.rectangle(i as f64 * tile, strip_y, tile + 0.5, strip_h);
            cr.clip();
            let tw = surf.width() as f64 * scale;
            cr.translate(i as f64 * tile + (tile - tw) / 2.0, strip_y);
            cr.scale(scale, scale);
            cr.set_source_surface(surf, 0.0, 0.0).ok();
            cr.paint().ok();
            cr.restore().ok();
        }
    }
    // Everything outside the trim is dimmed.
    let (xs, xe) = (x_of(m.trim.0), x_of(m.trim.1));
    cr.set_source_rgba(0.02, 0.02, 0.04, 0.72);
    cr.rectangle(0.0, strip_y, xs, strip_h);
    cr.rectangle(xe, strip_y, width - xe, strip_h);
    cr.fill().ok();
    cr.restore().ok();

    // The kept span: an amber frame with grips at both ends.
    cr.set_source_rgb(0.98, 0.75, 0.14);
    cr.set_line_width(2.5);
    cr.rectangle(xs + 6.0, strip_y + 1.25, (xe - xs - 12.0).max(0.0), strip_h - 2.5);
    cr.stroke().ok();
    for (x, left) in [(xs, true), (xe, false)] {
        let hx = if left { x } else { x - 12.0 };
        draw_rounded_rect(cr, hx.clamp(0.0, width - 12.0), strip_y, 12.0, strip_h, 4.0);
        cr.set_source_rgb(0.98, 0.75, 0.14);
        cr.fill().ok();
        cr.set_source_rgba(0.25, 0.16, 0.0, 0.85);
        let gx = hx.clamp(0.0, width - 12.0) + 6.0;
        for dy in [-5.0, 0.0, 5.0] {
            cr.rectangle(gx - 2.0, strip_y + strip_h / 2.0 + dy - 0.75, 4.0, 1.5);
        }
        cr.fill().ok();
    }

    // Playhead.
    let px = x_of(m.playhead);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.rectangle(px - 1.0, 0.0, 2.0, height);
    cr.fill().ok();
    cr.arc(px, 3.0, 3.5, 0.0, std::f64::consts::TAU);
    cr.fill().ok();
}

fn chip(label: &str, on: bool) -> Button {
    let b = Button::with_label(&format!("{} {label}", if on { "●" } else { "○" }));
    b.add_css_class(if on { "toggle-chip-on" } else { "toggle-chip" });
    b
}

fn set_chip(b: &Button, label: &str, on: bool) {
    set_btn_text(b, &format!("{} {label}", if on { "●" } else { "○" }));
    b.remove_css_class(if on { "toggle-chip" } else { "toggle-chip-on" });
    b.add_css_class(if on { "toggle-chip-on" } else { "toggle-chip" });
}

fn row(label: &str, content: &impl IsA<gtk4::Widget>) -> GtkBox {
    let r = GtkBox::new(Orientation::Horizontal, 10);
    let l = Label::new(Some(label));
    l.add_css_class("editor-label");
    l.set_xalign(0.0);
    r.append(&l);
    content.set_hexpand(true);
    r.append(content);
    r
}

/// Opens an editor window for `file`.
pub fn open(app: &Application, file: &Path) {
    load_css();
    let info = match crate::ffmpeg::probe_video(file) {
        Ok(info) => info,
        Err(err) => {
            eprintln!("[HyperGSR] cannot edit {}: {err:#}", file.display());
            return;
        }
    };
    let duration = (info.duration_sec - info.video_start_sec).max(0.1);
    let project = crate::project::find(file);
    let model = Rc::new(RefCell::new(Model {
        file: file.to_path_buf(),
        duration,
        fps: info.fps.map_or(60, |f| f.round() as u32).clamp(1, 240),
        trim: (0.0, duration),
        playhead: 0.0,
        aspect: Aspect::Original,
        format: ExportFormat::Mp4,
        zoom: project.as_ref().is_some_and(|p| p.job.auto_zoom),
        clicks: project.as_ref().is_some_and(|p| p.job.click_ripples),
        background: 0,
        thumbs: Vec::new(),
        project,
    }));

    let window = gtk4::Window::builder()
        .application(app)
        .title("Edit recording")
        .default_width(860)
        .default_height(780)
        .build();
    window.add_css_class("editor-window");
    let header = gtk4::HeaderBar::new();
    header.add_css_class("editor-header");
    let title = Label::new(Some(&file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));
    let open_btn = icon_button("folder", "Open…");
    header.pack_start(&open_btn);
    window.set_titlebar(Some(&header));

    let root = GtkBox::new(Orientation::Vertical, 10);
    root.add_css_class("editor-root");

    let video = gtk4::Video::for_filename(Some(file));
    video.set_autoplay(false);
    video.set_vexpand(true);
    video.set_size_request(-1, 300);
    root.append(&video);

    let timeline = DrawingArea::new();
    timeline.set_content_height(64);
    timeline.set_hexpand(true);
    {
        let model = model.clone();
        timeline.set_draw_func(move |_, cr, w, h| draw_timeline(cr, w as f64, h as f64, &model.borrow()));
    }
    root.append(&timeline);

    let times = Label::new(None);
    times.add_css_class("editor-times");
    times.set_hexpand(true);
    times.set_xalign(0.0);
    let refresh_times: Rc<dyn Fn()> = {
        let model = model.clone();
        let times = times.clone();
        Rc::new(move || {
            let m = model.borrow();
            times.set_text(&format!(
                "Start {}    End {}    Length {}",
                clock(m.trim.0),
                clock(m.trim.1),
                clock(m.trim.1 - m.trim.0)
            ));
        })
    };
    refresh_times();
    let start_btn = icon_button("trim", "Start here");
    let end_btn = icon_button("trim", "End here");
    start_btn.add_css_class("action-chip");
    end_btn.add_css_class("action-chip");
    start_btn.set_tooltip_text(Some("Trim the start to the playhead"));
    end_btn.set_tooltip_text(Some("Trim the end to the playhead"));
    let trim_row = GtkBox::new(Orientation::Horizontal, 6);
    trim_row.append(&times);
    trim_row.append(&start_btn);
    trim_row.append(&end_btn);
    root.append(&trim_row);

    let seek = {
        let video = video.clone();
        move |t: f64| {
            if let Some(stream) = video.media_stream() {
                stream.seek((t * 1e6) as i64);
            }
        }
    };

    // Drag a handle to trim, anywhere else to scrub.
    {
        let drag = GestureDrag::new();
        let target = Rc::new(Cell::new(0u8));
        let time_at = {
            let model = model.clone();
            let area = timeline.clone();
            move |x: f64| (x / area.width().max(1) as f64 * model.borrow().duration).clamp(0.0, model.borrow().duration)
        };
        {
            let model = model.clone();
            let target = target.clone();
            let area = timeline.clone();
            let time_at = time_at.clone();
            let seek = seek.clone();
            drag.connect_drag_begin(move |_, x, _| {
                let w = area.width().max(1) as f64;
                let (xs, xe) = {
                    let m = model.borrow();
                    (m.trim.0 / m.duration * w, m.trim.1 / m.duration * w)
                };
                let pick = if (x - xs).abs() <= 14.0 && (x - xs).abs() <= (x - xe).abs() {
                    1
                } else if (x - xe).abs() <= 14.0 {
                    2
                } else {
                    3
                };
                target.set(pick);
                if pick == 3 {
                    let t = time_at(x);
                    model.borrow_mut().playhead = t;
                    seek(t);
                    area.queue_draw();
                }
            });
        }
        {
            let model = model.clone();
            let area = timeline.clone();
            let refresh_times = refresh_times.clone();
            let seek = seek.clone();
            drag.connect_drag_update(move |g, dx, _| {
                let Some((sx, _)) = g.start_point() else { return };
                let t = time_at(sx + dx);
                {
                    let mut m = model.borrow_mut();
                    match target.get() {
                        1 => m.trim.0 = t.min(m.trim.1 - 0.2).max(0.0),
                        2 => m.trim.1 = t.max(m.trim.0 + 0.2).min(m.duration),
                        _ => m.playhead = t,
                    }
                }
                let m = model.borrow();
                let shown = match target.get() {
                    1 => m.trim.0,
                    2 => m.trim.1,
                    _ => t,
                };
                drop(m);
                seek(shown);
                refresh_times();
                area.queue_draw();
            });
        }
        timeline.add_controller(drag);
    }
    for (btn, start) in [(start_btn.clone(), true), (end_btn.clone(), false)] {
        let model = model.clone();
        let area = timeline.clone();
        let refresh_times = refresh_times.clone();
        btn.connect_clicked(move |_| {
            let mut m = model.borrow_mut();
            let t = m.playhead;
            if start {
                m.trim.0 = t.min(m.trim.1 - 0.2).max(0.0);
            } else {
                m.trim.1 = t.max(m.trim.0 + 0.2).min(m.duration);
            }
            drop(m);
            refresh_times();
            area.queue_draw();
        });
    }

    let options = GtkBox::new(Orientation::Vertical, 8);
    options.add_css_class("bento-card");
    let (shape_track, _) = {
        let model = model.clone();
        // U+2236 RATIO: `segments` reads "a:b" as an icon and a label.
        segments(&["Original", "16\u{2236}9", "9\u{2236}16", "1\u{2236}1"], 0, move |i| {
            model.borrow_mut().aspect = [Aspect::Original, Aspect::Wide, Aspect::Vertical, Aspect::Square][i];
        })
    };
    options.append(&row("Shape", &shape_track));
    let (format_track, _) = {
        let model = model.clone();
        segments(&["MP4", "WebM", "GIF"], 0, move |i| {
            model.borrow_mut().format = [ExportFormat::Mp4, ExportFormat::WebM, ExportFormat::Gif][i];
        })
    };
    format_track.set_tooltip_text(Some("MP4 plays everywhere; WebM is for the web; GIF is silent, 15 fps"));
    options.append(&row("Format", &format_track));

    let effects = GtkBox::new(Orientation::Horizontal, 6);
    let has_project = model.borrow().project.is_some();
    let zoomable = model.borrow().project.as_ref().is_some_and(|p| p.zoomable);
    let zoom_btn = chip("Auto-zoom", model.borrow().zoom && zoomable);
    zoom_btn.set_sensitive(zoomable);
    let clicks_btn = chip("Clicks", model.borrow().clicks);
    clicks_btn.set_sensitive(has_project);
    {
        let model = model.clone();
        zoom_btn.connect_clicked(move |b| {
            let on = !model.borrow().zoom;
            model.borrow_mut().zoom = on;
            set_chip(b, "Auto-zoom", on);
        });
    }
    {
        let model = model.clone();
        clicks_btn.connect_clicked(move |b| {
            let on = !model.borrow().clicks;
            model.borrow_mut().clicks = on;
            set_chip(b, "Clicks", on);
        });
    }
    let mut names: Vec<&str> = vec!["Background as recorded", "No background"];
    names.extend(PRESETS.iter().map(|p| p.name));
    let background = gtk4::DropDown::from_strings(&names);
    background.add_css_class("editor-drop");
    {
        let model = model.clone();
        background.connect_selected_notify(move |d| model.borrow_mut().background = d.selected() as usize);
    }
    effects.append(&zoom_btn);
    effects.append(&clicks_btn);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    effects.append(&spacer);
    effects.append(&background);
    options.append(&row("Effects", &effects));
    let note = Label::new(Some(if has_project {
        "Re-rendered from the original capture: zoom, cursor, and background are applied again."
    } else {
        "This video's capture is no longer kept, so it is re-encoded as it looks now."
    }));
    note.add_css_class("editor-note");
    note.set_xalign(0.0);
    note.set_wrap(true);
    options.append(&note);
    root.append(&options);

    let bottom = GtkBox::new(Orientation::Horizontal, 10);
    let status = Label::new(Some("Drag the yellow handles to trim."));
    status.add_css_class("editor-note");
    status.set_hexpand(true);
    status.set_xalign(0.0);
    status.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    let progress_bar = gtk4::ProgressBar::new();
    progress_bar.add_css_class("editor-progress");
    progress_bar.set_visible(false);
    progress_bar.set_valign(Align::Center);
    progress_bar.set_size_request(140, -1);
    let show_btn = icon_button("folder", "Show");
    show_btn.set_visible(false);
    let export_btn = icon_button("export", "Export");
    export_btn.add_css_class("export-btn");
    bottom.append(&status);
    bottom.append(&progress_bar);
    bottom.append(&show_btn);
    bottom.append(&export_btn);
    root.append(&bottom);
    window.set_child(Some(&root));

    // Thumbnails load in the background.
    let thumbs_slot: Arc<parking_lot::Mutex<Option<Thumbnails>>> = Default::default();
    {
        let slot = thumbs_slot.clone();
        let (file, size) = (file.to_path_buf(), (info.width, info.height));
        std::thread::spawn(move || *slot.lock() = Some(thumbnails(&file, duration, size, 12, 52)));
    }

    let render_progress = Arc::new(AtomicU32::new(0));
    let export_slot: Arc<parking_lot::Mutex<Option<Result<PathBuf, String>>>> = Default::default();
    let exporting = Rc::new(Cell::new(false));
    let last_export: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    {
        let model = model.clone();
        let progress = render_progress.clone();
        let slot = export_slot.clone();
        let exporting = exporting.clone();
        let status = status.clone();
        let bar = progress_bar.clone();
        let show = show_btn.clone();
        let video = video.clone();
        export_btn.connect_clicked(move |btn| {
            if exporting.get() {
                return;
            }
            let job = model.borrow().job();
            if let Some(stream) = video.media_stream() {
                stream.pause();
            }
            exporting.set(true);
            btn.set_sensitive(false);
            show.set_visible(false);
            bar.set_fraction(0.0);
            bar.set_visible(true);
            status.set_text(&format!(
                "Exporting {}…",
                job.output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ));
            progress.store(0, Ordering::Relaxed);
            let progress = progress.clone();
            let slot = slot.clone();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::studio_render::render(&job, &progress)
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("the export hit an internal error")))
                .map_err(|e| format!("{e:#}"));
                *slot.lock() = Some(result);
            });
        });
    }
    {
        let last = last_export.clone();
        show_btn.connect_clicked(move |_| {
            if let Some(dir) = last.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf)) {
                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
            }
        });
    }
    {
        let app = app.clone();
        let win = window.clone();
        open_btn.connect_clicked(move |_| {
            let filter = gtk4::FileFilter::new();
            filter.set_name(Some("Videos"));
            for mime in ["video/mp4", "video/webm", "video/x-matroska"] {
                filter.add_mime_type(mime);
            }
            let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
            filters.append(&filter);
            let dialog = gtk4::FileDialog::builder().title("Edit a recording").modal(true).filters(&filters).build();
            let app = app.clone();
            let win2 = win.clone();
            dialog.open(Some(&win), gtk4::gio::Cancellable::NONE, move |res| {
                if let Some(path) = res.ok().and_then(|f| f.path()) {
                    open(&app, &path);
                    win2.close();
                }
            });
        });
    }

    // Playhead, thumbnails, and export progress.
    {
        let weak = window.downgrade();
        let model = model.clone();
        let area = timeline.clone();
        let video = video.clone();
        let shown_first = Cell::new(false);
        glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(_win) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if let Some(frames) = thumbs_slot.lock().take() {
                model.borrow_mut().thumbs = frames
                    .into_iter()
                    .filter_map(|(w, h, data)| {
                        let stride = gtk4::cairo::Format::Rgb24.stride_for_width(w as u32).ok()?;
                        (stride as usize == w * 4).then_some(())?;
                        gtk4::cairo::ImageSurface::create_for_data(data, gtk4::cairo::Format::Rgb24, w as i32, h as i32, stride)
                            .ok()
                    })
                    .collect();
                area.queue_draw();
            }
            if let Some(stream) = video.media_stream() {
                // Show the first frame instead of a black box before playing.
                if !shown_first.get() && stream.is_prepared() {
                    shown_first.set(true);
                    stream.seek(0);
                }
                let t = stream.timestamp() as f64 / 1e6;
                let (end, playing) = (model.borrow().trim.1, stream.is_playing());
                // Playback stops at the end of the trim.
                if playing && t >= end {
                    stream.pause();
                }
                if playing && (t - model.borrow().playhead).abs() > 0.01 {
                    model.borrow_mut().playhead = t;
                    area.queue_draw();
                }
            }
            if exporting.get() {
                progress_bar.set_fraction(f64::from(render_progress.load(Ordering::Relaxed)) / 1000.0);
                if let Some(result) = export_slot.lock().take() {
                    exporting.set(false);
                    export_btn.set_sensitive(true);
                    progress_bar.set_visible(false);
                    match result {
                        Ok(path) => {
                            status.set_text(&format!(
                                "Saved {}",
                                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                            ));
                            *last_export.borrow_mut() = Some(path);
                            show_btn.set_visible(true);
                        }
                        Err(err) => status.set_text(&format!("Export failed: {err}")),
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let video = video.clone();
        window.connect_close_request(move |_| {
            if let Some(stream) = video.media_stream() {
                stream.pause();
            }
            glib::Propagation::Proceed
        });
    }

    if let Ok(path) = std::env::var("HYPERGSR_EDITOR_SHOT") {
        let win = window.clone();
        let root = root.clone();
        let delay = std::env::var("HYPERGSR_SCREENSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            crate::gui::save_widget_png(&root, &win, &path);
            if let Some(app) = win.application() {
                app.quit();
            }
        });
    }
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_map_through_the_recordings_own_cuts() {
        // The saved video skipped a 3 s countdown and a pause from 5 to 6.5 s.
        let base = vec![(0.0, 3.0), (5.0, 6.5)];
        let kept = kept_ranges(&base, 1e9);
        assert_eq!(source_time(&kept, 0.0), 3.0);
        assert_eq!(source_time(&kept, 2.5), 7.0);
        let cuts = trimmed_cuts(&base, (1.0, 4.0), 10.0);
        assert_eq!(&cuts[..2], &base[..]);
        assert_eq!(cuts[2], (0.0, 4.0));
        assert_eq!(cuts[3].0, 8.5);
        assert!(cuts[3].1.is_infinite());
        // An untouched trim adds nothing.
        assert_eq!(trimmed_cuts(&base, (0.0, 10.0), 10.0), base);
    }

    /// `HYPERGSR_EDIT_TEST=<saved video with a kept project>` exports it
    /// trimmed, vertical, as WebM.
    #[test]
    #[ignore = "needs a recording; set HYPERGSR_EDIT_TEST"]
    fn exports_a_kept_project() {
        let file = PathBuf::from(std::env::var("HYPERGSR_EDIT_TEST").unwrap());
        let info = crate::ffmpeg::probe_video(&file).unwrap();
        let duration = info.duration_sec - info.video_start_sec;
        let project = crate::project::find(&file).expect("a kept project");
        let model = Model {
            file: file.clone(),
            duration,
            fps: 60,
            trim: (1.0, duration - 1.0),
            playhead: 0.0,
            aspect: Aspect::Vertical,
            format: ExportFormat::WebM,
            zoom: true,
            clicks: true,
            background: 2,
            thumbs: Vec::new(),
            project: Some(project),
        };
        let job = model.job();
        let out = crate::studio_render::render(&job, &AtomicU32::new(0)).unwrap();
        let info = crate::ffmpeg::probe_video(&out).unwrap();
        println!("{} {}x{} {:.2}s", out.display(), info.width, info.height, info.duration_sec);
        assert_eq!((info.width, info.height), (1080, 1920));
        assert!((info.duration_sec - (duration - 2.0)).abs() < 0.2, "{}", info.duration_sec);
    }

    #[test]
    fn exports_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("hypergsr-editor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("HyperGSR_1.mp4");
        let first = export_path(&src, Aspect::Vertical, ExportFormat::WebM);
        assert_eq!(first, dir.join("HyperGSR_1-vertical.webm"));
        std::fs::write(&first, b"").unwrap();
        assert_eq!(export_path(&src, Aspect::Vertical, ExportFormat::WebM), dir.join("HyperGSR_1-vertical-2.webm"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
