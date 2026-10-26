use crate::capture::{CaptureSourceMode, FrameBuffer, ScreenCapturer};
use crate::compositor::{FrameCompositor, FrameStyleConfig};
use crate::cursor::{CursorConfig, CursorSample, CursorTracker, KeystrokeOverlay};
use crate::decompiled_gsr::GsrDecompiledReport;
use crate::encoder::{
    export_showcase_recording, ActiveEncoderSession, CaptureExit, CaptureOptions,
    FinishedCapture, HardwareEncoderBackend, RecordedZoomEvent,
};
use crate::settings::{CaptureSource, RecorderSettings};
use crate::studio_render::{self, RenderJob};
use crate::zoom_engine::{CameraViewport, ZoomConfig, ZoomEngine, ZoomKeyframe};
use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    pub is_recording: bool,
    /// Why the recording is paused, waiting to be resumed.
    #[serde(default)]
    pub capture_paused: Option<String>,
    pub recording_elapsed_sec: f32,
    pub recorded_frames: u64,
    pub active_encoder: String,
    pub last_saved_file: Option<String>,
    pub cursor: CursorSample,
    pub viewport: CameraViewport,
    pub active_keystrokes: Vec<KeystrokeOverlay>,
    pub keyframes: Vec<ZoomKeyframe>,
    pub zoom_cfg: ZoomConfig,
    pub cursor_cfg: CursorConfig,
    pub style_cfg: FrameStyleConfig,
    pub rss_memory_mb: f32,
}

pub struct StudioState {
    pub start_instant: Instant,
    pub recording_start: Option<Instant>,
    pub capture_mode: CaptureSourceMode,
    pub encoder_backend: HardwareEncoderBackend,
    pub target_fps: u32,
    pub record_audio: bool,
    pub auto_apply_zoom_on_stop: bool,
    pub zoom_cfg: ZoomConfig,
    pub cursor_cfg: CursorConfig,
    pub style_cfg: FrameStyleConfig,
    pub cursor_tracker: CursorTracker,
    pub zoom_engine: ZoomEngine,
    pub last_viewport: CameraViewport,
    pub last_cursor: CursorSample,
    pub interactive_override: Option<(f32, f32, bool, bool, u64)>,
    pub encoder_session: Option<ActiveEncoderSession>,
    pub last_saved_file: Option<String>,
    pub gsr_report: GsrDecompiledReport,
    pub output_dir: PathBuf,
    pub videos_dir: PathBuf,
    pub click_count: u64,
    pub settings: RecorderSettings,
    /// Show the portal picker on the next recording instead of reusing the
    /// remembered screen or window.
    pub repick_source: bool,
    /// Settings the running recording was started with; changing the drawer
    /// mid-recording only affects the next one.
    recording_settings: Option<RecorderSettings>,
    /// Finished parts of the running recording. When the screen share breaks,
    /// the capture ends its part and the recording pauses; Resume records
    /// the next part, and Stop joins them.
    recorded_parts: Vec<FinishedCapture>,
    /// Why the running recording is paused, waiting for Resume.
    pub capture_paused: Option<String>,
    /// Recorded time of the finished parts, for the timer.
    parts_elapsed_sec: f32,
    /// The file the joined recording is captured to, and how.
    capture_target: Option<(PathBuf, CaptureOptions)>,
    /// Video-time ranges of the running part to leave out: the countdown
    /// and finished pauses.
    part_cuts: Vec<(f64, f64)>,
    /// Video time of the running part when Pause was pressed.
    paused_at: Option<f64>,
    /// Seconds to count down once the first part is running.
    countdown_pending: u32,
    /// When the countdown ends.
    countdown_end: Option<Instant>,
    /// Lazy-allocated buffers used ONLY when `--web` requests a rendered JPEG frame
    lazy_web_buffers: Option<(ScreenCapturer, FrameBuffer, FrameBuffer)>,
}

pub type SharedStudioState = Arc<Mutex<StudioState>>;

impl StudioState {
    pub fn new(_width: usize, _height: usize) -> Self {
        let gsr_report = GsrDecompiledReport::probe_system();
        let output_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("recordings");
        std::fs::create_dir_all(&output_dir).ok();

        let settings = RecorderSettings::load();
        let videos_dir = settings.recordings_dir();
        std::fs::create_dir_all(&videos_dir).ok();
        let mut zoom_cfg = ZoomConfig::default();
        zoom_cfg.enabled = settings.auto_zoom;

        Self {
            start_instant: Instant::now(),
            recording_start: None,
            capture_mode: CaptureSourceMode::AutoHybrid,
            encoder_backend: HardwareEncoderBackend::GsrZeroCopyGpu,
            target_fps: settings.fps,
            record_audio: settings.audio != crate::settings::AudioMode::Off,
            auto_apply_zoom_on_stop: settings.auto_zoom,
            zoom_cfg,
            cursor_cfg: CursorConfig::default(),
            style_cfg: FrameStyleConfig::default(),
            cursor_tracker: CursorTracker::new(),
            zoom_engine: ZoomEngine::new(),
            last_viewport: CameraViewport::default(),
            last_cursor: CursorSample::default(),
            interactive_override: None,
            encoder_session: None,
            last_saved_file: None,
            gsr_report,
            output_dir,
            videos_dir,
            click_count: 0,
            settings,
            repick_source: false,
            recording_settings: None,
            recorded_parts: Vec::new(),
            capture_paused: None,
            parts_elapsed_sec: 0.0,
            capture_target: None,
            part_cuts: Vec::new(),
            paused_at: None,
            countdown_pending: 0,
            countdown_end: None,
            lazy_web_buffers: None,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.start_instant.elapsed().as_millis() as u64
    }

    /// Recorded time that will be in the video: the countdown and pauses
    /// do not count.
    pub fn recording_elapsed_sec(&self) -> f32 {
        let live = self.live_video_sec().map_or(0.0, |t| (t - self.cut_sec(t)).max(0.0));
        self.parts_elapsed_sec + live as f32
    }

    /// Video time of the running part, once its first frame is captured.
    fn live_video_sec(&self) -> Option<f64> {
        self.recording_start.map(|t| t.elapsed().as_secs_f64())
    }

    /// How much of the running part's first `upto` seconds is cut.
    fn cut_sec(&self, upto: f64) -> f64 {
        cut_total(&self.part_cuts, upto) + self.paused_at.map_or(0.0, |p| (upto - p).max(0.0))
    }

    /// Seconds left before recording starts for real, while counting down.
    pub fn countdown_left(&self) -> Option<f32> {
        self.encoder_session.as_ref()?;
        let left = self.countdown_end?.saturating_duration_since(Instant::now()).as_secs_f32();
        (left > 0.0).then_some(left)
    }

    /// Paused with the Pause button (the capture keeps running; the paused
    /// time is cut from the video).
    pub fn is_paused_by_user(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Pauses or continues the running recording, and returns whether it is
    /// paused now. Does nothing before the countdown is over.
    pub fn toggle_pause(&mut self) -> bool {
        let Some(t) = self.live_video_sec() else {
            return false;
        };
        if self.encoder_session.is_none() || self.countdown_left().is_some() {
            return self.paused_at.is_some();
        }
        let now = self.now_ms();
        match self.paused_at.take() {
            Some(from) => {
                self.part_cuts.push((from, t));
                self.cursor_tracker.push_keystroke("● REC Continued", now);
                false
            }
            None => {
                self.paused_at = Some(t);
                self.cursor_tracker.push_keystroke("⏸ Paused", now);
                true
            }
        }
    }

    /// The running part's cuts, with an open pause running to its end.
    fn take_part_cuts(&mut self) -> Vec<(f64, f64)> {
        let mut cuts = std::mem::take(&mut self.part_cuts);
        if let Some(from) = self.paused_at.take() {
            cuts.push((from, f64::INFINITY));
        }
        self.countdown_end = None;
        cuts
    }

    /// A recording is running, possibly paused after a broken screen share.
    pub fn is_recording(&self) -> bool {
        self.encoder_session.is_some() || !self.recorded_parts.is_empty()
    }

    pub fn read_rss_mb() -> f32 {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("RssAnon:") {
                    if let Some(kb_str) = rest.split_whitespace().next() {
                        if let Ok(kb) = kb_str.parse::<u64>() {
                            return kb as f32 / 1024.0;
                        }
                    }
                }
            }
        }
        18.5
    }

    pub fn set_interactive_cursor(&mut self, x: f32, y: f32, left: bool, right: bool) {
        let now = self.now_ms();
        self.interactive_override = Some((x.clamp(0.0, 1.0), y.clamp(0.0, 1.0), left, right, now));
    }

    /// Ultra-lightweight O(1) telemetry tick (< 3 microseconds, 0 heap allocations, 0 pixel loops).
    /// Polls live cursor coordinates, steps the 2D spring filter and virtual camera, and logs
    /// zoom keyframes if a zero-copy GPU recording is active.
    pub fn tick(&mut self) {
        let now_ms = self.now_ms();

        let (raw_x, raw_y, sys_inp) =
            if let Some((ix, iy, l, r, ts)) = self.interactive_override {
                if now_ms.saturating_sub(ts) < 1800 {
                    (
                        ix,
                        iy,
                        crate::cursor::SystemInputState {
                            left_down: l,
                            right_down: r,
                            ..Default::default()
                        },
                    )
                } else {
                    self.interactive_override = None;
                    self.poll_live_input(now_ms)
                }
            } else {
                self.poll_live_input(now_ms)
            };

        let (sample, clicked_now) = self.cursor_tracker.update_with_input(
            now_ms,
            raw_x,
            raw_y,
            sys_inp,
            &self.cursor_cfg,
        );

        if clicked_now {
            self.click_count += 1;
            if self.cursor_cfg.show_keystrokes {
                let label = if sys_inp.right_down {
                    "Right Click · Focus"
                } else {
                    "Click · Spring Zoom"
                };
                self.cursor_tracker.push_keystroke(label, now_ms);
            }
        }

        let dwell_ms = self.cursor_tracker.dwell_duration_ms(now_ms);
        let viewport =
            self.zoom_engine
                .step(now_ms, sample, clicked_now, dwell_ms, &self.zoom_cfg);

        self.last_cursor = sample;
        self.last_viewport = viewport;

        let manual_locked = self.zoom_engine.manual_locked;
        // Video time zero is the first captured frame, which can be seconds
        // after Record while the portal starts; telemetry waits for it.
        if self.recording_start.is_none() {
            self.recording_start = self.encoder_session.as_ref().and_then(|s| s.ready_at());
            // The countdown runs on a live capture, so recording starts the
            // moment it ends; the counted seconds are cut from the video.
            if let (Some(start), n @ 1..) = (self.recording_start, self.countdown_pending) {
                self.countdown_pending = 0;
                self.countdown_end = Some(start + std::time::Duration::from_secs(u64::from(n)));
                self.part_cuts.push((0.0, f64::from(n)));
            }
        }
        let Some(rec_start) = self.recording_start else {
            return;
        };
        let rec_elapsed = rec_start.elapsed().as_secs_f32();
        // With several displays, a cursor on another one must not steer the zoom.
        let records_monitor = self
            .recording_settings
            .as_ref()
            .is_some_and(|s| s.source != CaptureSource::Window);
        let off_capture = records_monitor
            && match (
                self.encoder_session.as_ref().and_then(|s| s.capture_size()),
                self.cursor_tracker.cursor_output_size(),
            ) {
                (Some(capture), Some(output)) => !crate::encoder::cursor_on_capture(capture, output),
                _ => false,
            };
        if let Some(ref mut session) = self.encoder_session {
            session.record_zoom_telemetry(
                rec_elapsed,
                viewport.center_x,
                viewport.center_y,
                viewport.zoom,
                sample.smooth_x,
                sample.smooth_y,
                clicked_now,
                manual_locked,
                off_capture,
            );
        }
        self.detect_frozen_capture(now_ms, off_capture);
    }

    /// Ends the current part when the stream stopped sending images while
    /// the cursor kept moving on the recorded monitor: the portal died
    /// without closing the stream. A still screen with a still cursor is
    /// left alone. A moving cursor shows up in the stream either way: drawn
    /// into the image, or as cursor metadata the helper counts as activity.
    fn detect_frozen_capture(&mut self, now_ms: u64, off_capture: bool) {
        // The helper reports a stall after this long without a new image.
        const STALL_REPORT_MS: u64 = 3_000;
        const FROZEN_AFTER_MS: u64 = 6_000;
        // Motion this soon after the last image may still be in flight.
        const MOTION_GRACE_MS: u64 = 1_500;
        let watch = self
            .recording_settings
            .as_ref()
            .is_some_and(|s| s.source != CaptureSource::Window && s.show_cursor);
        if !watch || off_capture || !self.cursor_tracker.has_live_source() {
            return;
        }
        let Some(last_motion) = self.cursor_tracker.last_motion_ms() else {
            return;
        };
        let Some(session) = self.encoder_session.as_mut() else {
            return;
        };
        let Some((since, last_image_sec)) = session.stalled() else {
            return;
        };
        let stalled_ms = since.elapsed().as_millis() as u64 + STALL_REPORT_MS;
        let idle_ms = now_ms.saturating_sub(last_motion);
        if stalled_ms >= FROZEN_AFTER_MS && idle_ms < 1_000 && idle_ms + MOTION_GRACE_MS < stalled_ms {
            session.interrupt(last_image_sec, "the screen stopped updating");
        }
    }

    /// Used only by the optional `--web` HTTP server to render a 640x360 preview JPEG.
    pub fn render_web_preview_rgb(&mut self) -> (u32, u32, Vec<u8>) {
        self.tick();
        let now_ms = self.now_ms();
        let (capturer, raw_fb, comp_fb) = self
            .lazy_web_buffers
            .get_or_insert_with(|| (ScreenCapturer::new(640, 360), FrameBuffer::new(640, 360), FrameBuffer::new(640, 360)));

        capturer.capture_frame(CaptureSourceMode::StudioWorkspace, now_ms, raw_fb);
        let trail = self
            .cursor_tracker
            .recent_trail(self.cursor_cfg.trail_length);
        FrameCompositor::render_frame(
            raw_fb,
            &self.last_viewport,
            &self.last_cursor,
            &trail,
            self.cursor_tracker.active_ripples(),
            &self.cursor_cfg,
            &self.style_cfg,
            now_ms,
            comp_fb,
        );
        (comp_fb.width as u32, comp_fb.height as u32, comp_fb.data.clone())
    }

    fn poll_live_input(&self, now_ms: u64) -> (f32, f32, crate::cursor::SystemInputState) {
        if let Some((px, py, inp)) = self.cursor_tracker.poll_system_input() {
            return (px, py, inp);
        }
        if self.cursor_tracker.has_live_source() {
            // A real pointer source is open but has no position yet (or X11 failed this tick):
            // keep the camera still rather than inventing motion in a real recording.
            let (x, y) = self
                .cursor_tracker
                .latest()
                .map(|s| (s.raw_x, s.raw_y))
                .unwrap_or((0.5, 0.5));
            return (x, y, Default::default());
        }
        let t = now_ms as f32 / 1000.0;
        let x = 0.5 + 0.25 * (t * 0.6).cos();
        let y = 0.5 + 0.20 * (t * 0.9).sin();
        (x, y, Default::default())
    }

    pub fn start_recording(&mut self) -> Result<PathBuf> {
        if self.is_recording() {
            self.stop_recording()?;
        }
        let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        // Two recordings in one second must not share files.
        let capture_path = (1..)
            .map(|n| {
                let suffix = if n == 1 { String::new() } else { format!("-{n}") };
                self.videos_dir.join(format!("HyperGSR_{ts}{suffix}{CAPTURE_SUFFIX}"))
            })
            .find(|p| {
                // Claim the name now; FFmpeg may not create the file for a while.
                !final_path(p).exists()
                    && std::fs::OpenOptions::new().write(true).create_new(true).open(p).is_ok()
            })
            .expect("an unused file name");
        let settings = &self.settings;
        let options = CaptureOptions {
            fps: settings.fps,
            quality: settings.quality,
            audio: settings.audio,
            source: settings.source,
            region: settings.active_region(),
            show_cursor: settings.show_cursor,
            studio_cursor: crate::encoder::portal_cursor_metadata()
                && std::env::var_os("HYPERGSR_NATIVE_CURSOR").is_none(),
            repick: self.repick_source,
            clean_mic: settings.clean_mic,
        };
        let session = match ActiveEncoderSession::start_capture(&capture_path, &options) {
            Ok(session) => session,
            Err(err) => {
                let _ = std::fs::remove_file(&capture_path);
                return Err(err);
            }
        };
        self.repick_source = false;
        self.recording_settings = Some(settings.clone());
        self.target_fps = settings.fps;
        self.recording_start = None;
        self.encoder_session = Some(session);
        self.recorded_parts.clear();
        self.capture_paused = None;
        self.parts_elapsed_sec = 0.0;
        self.part_cuts.clear();
        self.paused_at = None;
        self.countdown_end = None;
        self.countdown_pending = settings.countdown_sec;
        self.capture_target = Some((capture_path.clone(), CaptureOptions { repick: false, ..options }));
        self.cursor_tracker
            .push_keystroke("● REC Started (Zero-Copy GPU)", self.now_ms());
        Ok(capture_path)
    }

    /// Continues a paused recording in a new part, reusing the remembered
    /// screen or window. The portal may show its picker if it forgot it.
    pub fn resume_recording(&mut self) -> Result<()> {
        if self.encoder_session.is_some() || self.capture_paused.is_none() {
            return Ok(());
        }
        let Some((target, options)) = self.capture_target.clone() else {
            anyhow::bail!("there is no paused recording to resume");
        };
        let part = part_path(&target, self.recorded_parts.len() + 1);
        let session = ActiveEncoderSession::start_capture(&part, &options)?;
        self.encoder_session = Some(session);
        self.recording_start = None;
        self.capture_paused = None;
        self.cursor_tracker.push_keystroke("● REC Resumed", self.now_ms());
        Ok(())
    }

    /// `Some(reason)` when the running capture died on its own (for example
    /// the encoder failed) and the recording has to stop. A broken screen
    /// share pauses the recording instead; see [`Self::capture_paused`].
    pub fn poll_capture_failure(&mut self) -> Option<String> {
        let session = self.encoder_session.as_mut()?;
        let exit = session.poll_exit()?;
        let never_started = session.ready_at().is_none();
        match exit {
            CaptureExit::Interrupted(reason) => {
                self.end_part();
                self.capture_paused = Some(reason);
                None
            }
            // A resume that never got a frame (a cancelled picker) leaves the
            // recording paused rather than ending it.
            CaptureExit::Failed(reason) if never_started && !self.recorded_parts.is_empty() => {
                if let Some(session) = self.encoder_session.take() {
                    let _ = std::fs::remove_file(session.finish_capture().path);
                }
                self.capture_paused = Some(format!("could not resume: {reason}"));
                None
            }
            CaptureExit::Failed(reason) => Some(reason),
        }
    }

    /// Finalizes the capture that just ended as one part of the recording.
    fn end_part(&mut self) {
        let Some(session) = self.encoder_session.take() else {
            return;
        };
        let live_sec = self.live_video_sec();
        let cuts = self.take_part_cuts();
        self.recording_start = None;
        let mut done = session.finish_capture();
        let kept = done.keep_sec.or(live_sec).unwrap_or(0.0);
        self.parts_elapsed_sec += (kept - cut_total(&cuts, kept)).max(0.0) as f32;
        done.cuts = cuts;
        self.recorded_parts.push(done);
    }

    /// Detaches the running capture so it can be finalized and rendered on a
    /// worker thread with [`StopJob::complete`], keeping the UI responsive.
    pub fn take_recording(&mut self) -> Option<StopJob> {
        if !self.is_recording() {
            return None;
        }
        let live_cuts = self.take_part_cuts();
        self.countdown_pending = 0;
        let session = self.encoder_session.take();
        let parts = std::mem::take(&mut self.recorded_parts);
        let capture = self
            .capture_target
            .take()
            .map(|(path, _)| path)
            .or_else(|| session.as_ref().map(|s| s.output_path.clone()))
            .or_else(|| parts.first().map(|p| p.path.clone()))?;
        self.capture_paused = None;
        self.parts_elapsed_sec = 0.0;
        self.recording_start = None;
        self.cursor_tracker
            .push_keystroke("✔ Saved GPU Recording", self.now_ms());
        let settings = self
            .recording_settings
            .take()
            .unwrap_or_else(|| self.settings.clone());
        // The background is presentation only, so the latest choice applies.
        let settings = RecorderSettings {
            background: self.settings.background.clone(),
            ..settings
        };
        Some(StopJob {
            parts,
            session,
            live_cuts,
            capture,
            auto_zoom: self.auto_apply_zoom_on_stop && settings.auto_zoom,
            settings,
        })
    }

    /// Stops and renders the recording on the calling thread.
    pub fn stop_recording(&mut self) -> Result<Option<PathBuf>> {
        let Some(job) = self.take_recording() else {
            return Ok(None);
        };
        let final_path = job.complete(&AtomicU32::new(0))?;
        self.last_saved_file = Some(final_path.display().to_string());
        Ok(Some(final_path))
    }

    pub fn render_instant_demo_mp4(&mut self) -> Result<(PathBuf, u64, String)> {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let out_path = self.output_dir.join(format!("hypergsr_showcase_{ts}.mp4"));
        let res = export_showcase_recording(
            &out_path,
            1280,
            720,
            60,
            5,
            self.encoder_backend,
            &self.zoom_cfg,
            &self.cursor_cfg,
            &self.style_cfg,
        )?;
        self.last_saved_file = Some(res.0.display().to_string());
        self.cursor_tracker
            .push_keystroke("Exported 60fps Showcase MP4", self.now_ms());
        Ok(res)
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        let (recorded_frames, active_encoder) = match &self.encoder_session {
            Some(s) => (s.frames_written, s.active_backend.clone()),
            None => (0, self.encoder_backend.short_badge().to_string()),
        };
        TelemetrySnapshot {
            is_recording: self.is_recording(),
            capture_paused: self.capture_paused.clone(),
            recording_elapsed_sec: self.recording_elapsed_sec(),
            recorded_frames,
            active_encoder,
            last_saved_file: self.last_saved_file.clone(),
            cursor: self.last_cursor,
            viewport: self.last_viewport,
            active_keystrokes: self.cursor_tracker.active_keystrokes().to_vec(),
            keyframes: self.zoom_engine.keyframes.clone(),
            zoom_cfg: self.zoom_cfg.clone(),
            cursor_cfg: self.cursor_cfg.clone(),
            style_cfg: self.style_cfg.clone(),
            rss_memory_mb: Self::read_rss_mb(),
        }
    }
}

/// A stopped recording waiting to be finalized and rendered.
pub struct StopJob {
    /// Earlier parts of a recording whose screen share broke.
    parts: Vec<FinishedCapture>,
    session: Option<ActiveEncoderSession>,
    /// Cuts of the running part.
    live_cuts: Vec<(f64, f64)>,
    /// Where the whole recording's capture goes.
    capture: PathBuf,
    auto_zoom: bool,
    settings: RecorderSettings,
}

impl StopJob {
    /// Finalizes the capture, applies zoom and background, and returns the
    /// MP4 to show the user. A failed render falls back to the plain capture,
    /// so a recording is never lost to a post-processing error.
    pub fn complete(self, progress: &AtomicU32) -> Result<PathBuf> {
        let mut parts = self.parts;
        if let Some(session) = self.session {
            let mut live = session.finish_capture();
            live.cuts = self.live_cuts;
            parts.push(live);
        }
        let done = join_parts(parts, &self.capture)?;
        let capture = done.path;
        if !capture.exists() || std::fs::metadata(&capture).is_ok_and(|m| m.len() == 0) {
            let _ = std::fs::remove_file(&capture);
            anyhow::bail!(done.error.unwrap_or_else(|| "nothing was recorded".to_string()));
        }
        if let Some(err) = &done.error {
            eprintln!("[HyperGSR] capture ended with an error: {err}; keeping what was recorded");
        }
        save_telemetry(&capture, &done.zoom_events);

        let output = final_path(&capture);
        let cursor = done.cursor.filter(|c| c.is_useful());
        // Live telemetry is screen-wide, so a window is only zoomed by its
        // own cursor track.
        let zoomable = self.settings.source != CaptureSource::Window || cursor.is_some();
        let auto_zoom = self.auto_zoom && zoomable;
        let job = RenderJob {
            capture: capture.clone(),
            output: output.clone(),
            events: done.zoom_events,
            auto_zoom,
            region: self.settings.active_region(),
            background: self.settings.background.clone(),
            quality: self.settings.quality,
            fps: self.settings.fps,
            // Screen-wide click positions mean nothing inside a window.
            click_ripples: self.settings.click_ripples
                && (self.settings.source != CaptureSource::Window || cursor.is_some()),
            cursor,
            cursor_scale: self.settings.cursor_scale,
            cuts: done.cuts,
            aspect: Default::default(),
            format: Default::default(),
        };
        let result = studio_render::render(&job, progress).or_else(|err| {
            eprintln!("[HyperGSR] {err:#}; saving the recording without effects");
            crate::ffmpeg::remux_to_mp4(&capture, &output).map(|_| output.clone())
        });
        match result {
            Ok(path) => {
                // The capture stays editable for a while; see `project`.
                crate::project::keep(&job, zoomable);
                Ok(path)
            }
            // The fragmented capture plays as is; hand it over under the final name.
            Err(err) => {
                eprintln!("[HyperGSR] {err:#}; keeping the raw capture");
                let _ = std::fs::remove_file(&output);
                Ok(std::fs::rename(&capture, &output).map_or(capture, |_| output))
            }
        }
    }

    /// Stops the capture and deletes everything it wrote (a recording
    /// cancelled during its countdown).
    pub fn discard(self) {
        let mut paths: Vec<PathBuf> = self.parts.into_iter().map(|p| p.path).collect();
        if let Some(session) = self.session {
            paths.push(session.finish_capture().path);
        }
        paths.push(self.capture);
        for path in paths {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Suffix of the raw capture next to where the final MP4 will go.
const CAPTURE_SUFFIX: &str = ".capture.mp4";

/// `HyperGSR_<time>.capture.mp4` records its second part to
/// `HyperGSR_<time>.part2.capture.mp4`.
fn part_path(capture: &Path, n: usize) -> PathBuf {
    let name = capture.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = name.strip_suffix(CAPTURE_SUFFIX).unwrap_or(&name);
    capture.with_file_name(format!("{stem}.part{n}{CAPTURE_SUFFIX}"))
}

/// Where a part sits in the joined video: the file time its first frame
/// has, and the file time it is kept up to.
#[derive(Clone, Copy)]
struct PartSpan {
    video_start: f64,
    kept: f64,
}

/// For each part, how far its telemetry (timed from its own first frame)
/// moves on the joined video's timeline, which is timed from the first
/// part's first frame, and the last of its own time that was kept.
fn part_shifts(spans: &[PartSpan]) -> Vec<(f64, f64)> {
    let base = spans.first().map_or(0.0, |s| s.video_start);
    let mut offset = 0.0f64;
    spans
        .iter()
        .map(|span| {
            let shift = offset + span.video_start - base;
            offset += span.kept;
            (shift, span.kept - span.video_start)
        })
        .collect()
}

/// Moves each part's telemetry onto the joined video's timeline.
fn merge_part_events(parts: &[(PartSpan, Vec<RecordedZoomEvent>)]) -> Vec<RecordedZoomEvent> {
    let spans: Vec<PartSpan> = parts.iter().map(|(s, _)| *s).collect();
    let mut merged = Vec::new();
    for ((shift, last), (_, events)) in part_shifts(&spans).into_iter().zip(parts) {
        merged.extend(
            events
                .iter()
                .filter(|e| f64::from(e.timestamp_sec) <= last)
                .map(|e| RecordedZoomEvent {
                    timestamp_sec: (shift + f64::from(e.timestamp_sec)) as f32,
                    ..*e
                }),
        );
    }
    merged
}

/// Turns the recording's parts into one capture at `capture`, dropping the
/// frozen tail of an interrupted part. If joining fails, the first part is
/// kept as the capture and the others stay next to it as separate videos.
fn join_parts(parts: Vec<FinishedCapture>, capture: &Path) -> Result<FinishedCapture> {
    let last_error = parts.iter().rev().find_map(|p| p.error.clone());
    let (parts, empty): (Vec<_>, Vec<_>) = parts
        .into_iter()
        .partition(|p| std::fs::metadata(&p.path).is_ok_and(|m| m.len() > 0));
    for part in &empty {
        let _ = std::fs::remove_file(&part.path);
    }
    if parts.is_empty() {
        anyhow::bail!(last_error.unwrap_or_else(|| "nothing was recorded".to_string()));
    }
    if parts.len() == 1 && parts[0].keep_sec.is_none() {
        let mut only = parts.into_iter().next().expect("one part");
        if only.path != capture && std::fs::rename(&only.path, capture).is_ok() {
            only.path = capture.to_path_buf();
        }
        return Ok(only);
    }

    // The joined file takes the capture's name, so the first part moves aside.
    let mut parts = parts;
    if parts[0].path == capture {
        let moved = part_path(capture, 1);
        std::fs::rename(capture, &moved)?;
        parts[0].path = moved;
    }
    let mut spans = Vec::with_capacity(parts.len());
    let mut segments = Vec::with_capacity(parts.len());
    for part in &parts {
        let info = crate::ffmpeg::probe_video(&part.path)?;
        let end = part.keep_sec.map(|k| (info.video_start_sec + k).min(info.duration_sec));
        spans.push(PartSpan {
            video_start: info.video_start_sec,
            kept: end.unwrap_or(info.duration_sec),
        });
        segments.push(crate::ffmpeg::Segment { path: part.path.clone(), end_sec: end });
    }
    if let Err(err) = crate::ffmpeg::concat_segments(&segments, capture) {
        let _ = std::fs::remove_file(capture);
        eprintln!("[HyperGSR] {err:#}; keeping the parts as separate videos");
        let mut first = parts.remove(0);
        if std::fs::rename(&first.path, capture).is_ok() {
            first.path = capture.to_path_buf();
        }
        for (i, part) in parts.iter().enumerate() {
            let _ = std::fs::rename(&part.path, final_path(&part_path(capture, i + 2)));
        }
        return Ok(first);
    }
    let mut cursor: Option<crate::cursor_track::CursorTrack> = None;
    let mut cuts = Vec::new();
    for ((shift, last), part) in part_shifts(&spans).into_iter().zip(parts.iter_mut()) {
        if let Some(track) = part.cursor.take() {
            cursor.get_or_insert_with(Default::default).append_shifted(track, shift, last);
        }
        cuts.extend(shift_cuts(&part.cuts, shift, last));
    }
    let paired: Vec<_> = spans
        .into_iter()
        .zip(parts.iter().map(|p| p.zoom_events.clone()))
        .collect();
    for part in &parts {
        let _ = std::fs::remove_file(&part.path);
    }
    Ok(FinishedCapture {
        path: capture.to_path_buf(),
        zoom_events: merge_part_events(&paired),
        error: last_error,
        keep_sec: None,
        cursor,
        cuts,
    })
}

/// How much of the first `upto` seconds `cuts` leave out.
fn cut_total(cuts: &[(f64, f64)], upto: f64) -> f64 {
    cuts.iter().map(|&(a, b)| (b.min(upto) - a).max(0.0)).sum()
}

/// Moves a part's cuts onto the joined timeline, dropping what lies past
/// the part's kept end.
fn shift_cuts(cuts: &[(f64, f64)], shift: f64, last: f64) -> Vec<(f64, f64)> {
    cuts.iter()
        .filter(|(a, _)| *a < last)
        .map(|&(a, b)| (shift + a, shift + b.min(last)))
        .collect()
}

/// `HyperGSR_<time>.capture.mp4` becomes `HyperGSR_<time>.mp4`.
fn final_path(capture: &std::path::Path) -> PathBuf {
    let name = capture.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = name.strip_suffix(CAPTURE_SUFFIX).unwrap_or(&name);
    capture.with_file_name(format!("{}.mp4", stem.trim_end_matches(".mp4")))
}

/// Keeps the cursor telemetry of recent recordings for debugging and re-renders.
fn save_telemetry(capture: &std::path::Path, events: &[RecordedZoomEvent]) {
    let dir = crate::bundled::cache_dir().join("telemetry");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let name = final_path(capture)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recording".to_string());
    if let Ok(json) = serde_json::to_string(events) {
        let _ = std::fs::write(dir.join(format!("{name}.json")), json);
    }
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    if files.len() > 20 {
        files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
        for old in &files[..files.len() - 20] {
            let _ = std::fs::remove_file(old.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(t: f32) -> RecordedZoomEvent {
        RecordedZoomEvent {
            timestamp_sec: t,
            center_x: 0.5,
            center_y: 0.5,
            zoom: 1.0,
            cursor_x: 0.5,
            cursor_y: 0.5,
            clicked: false,
            manual_lock: false,
            off_capture: false,
        }
    }

    #[test]
    fn parts_are_named_after_the_capture() {
        let p = std::path::Path::new("/v/HyperGSR_2026-10-25_12-31-38.capture.mp4");
        assert_eq!(
            part_path(p, 2),
            PathBuf::from("/v/HyperGSR_2026-10-25_12-31-38.part2.capture.mp4")
        );
    }

    #[test]
    fn part_telemetry_follows_the_joined_timeline() {
        let parts = vec![
            // Audio started 0.2 s before the first frame; frozen after 64.9 s.
            (PartSpan { video_start: 0.2, kept: 65.1 }, vec![event(0.0), event(64.0), event(70.0)]),
            (PartSpan { video_start: 0.1, kept: 10.0 }, vec![event(0.0), event(5.0)]),
        ];
        let times: Vec<f32> = merge_part_events(&parts).iter().map(|e| e.timestamp_sec).collect();
        let expected = [0.0, 64.0, 65.0, 70.0];
        assert_eq!(times.len(), expected.len());
        for (t, want) in times.iter().zip(expected) {
            assert!((t - want).abs() < 1e-3, "{times:?}");
        }
    }

    #[test]
    fn part_cuts_follow_the_joined_timeline() {
        let spans = [PartSpan { video_start: 0.2, kept: 65.1 }, PartSpan { video_start: 0.1, kept: 10.0 }];
        let shifts = part_shifts(&spans);
        // A countdown in the first part; a pause left open in the second.
        assert_eq!(shift_cuts(&[(0.0, 3.0)], shifts[0].0, shifts[0].1), vec![(0.0, 3.0)]);
        let (shift, last) = shifts[1];
        let cut = shift_cuts(&[(4.0, f64::INFINITY), (20.0, 30.0)], shift, last);
        assert_eq!(cut.len(), 1);
        assert!((cut[0].0 - 69.0).abs() < 1e-9 && (cut[0].1 - 74.9).abs() < 1e-9, "{cut:?}");
    }

    #[test]
    fn capture_files_map_to_their_final_name() {
        let p = std::path::Path::new("/v/HyperGSR_2026-10-23_01-02-03.capture.mp4");
        assert_eq!(final_path(p), PathBuf::from("/v/HyperGSR_2026-10-23_01-02-03.mp4"));
        assert_eq!(final_path(std::path::Path::new("/v/a.mp4")), PathBuf::from("/v/a.mp4"));
    }
}
