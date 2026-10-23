use crate::capture::{CaptureSourceMode, FrameBuffer, ScreenCapturer};
use crate::compositor::{FrameCompositor, FrameStyleConfig};
use crate::cursor::{CursorConfig, CursorSample, CursorTracker, KeystrokeOverlay};
use crate::decompiled_gsr::GsrDecompiledReport;
use crate::encoder::{
    export_showcase_recording, ActiveEncoderSession, CaptureOptions, HardwareEncoderBackend,
    RecordedZoomEvent,
};
use crate::settings::{CaptureSource, RecorderSettings};
use crate::studio_render::{self, RenderJob};
use crate::zoom_engine::{CameraViewport, ZoomConfig, ZoomEngine, ZoomKeyframe};
use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    pub is_recording: bool,
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
            lazy_web_buffers: None,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.start_instant.elapsed().as_millis() as u64
    }

    pub fn recording_elapsed_sec(&self) -> f32 {
        self.recording_start
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0)
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
        if self.encoder_session.is_some() {
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
            repick: self.repick_source,
        };
        let session = ActiveEncoderSession::start_capture(&capture_path, &options)?;
        self.repick_source = false;
        self.recording_settings = Some(settings.clone());
        self.target_fps = settings.fps;
        self.recording_start = None;
        self.encoder_session = Some(session);
        self.cursor_tracker
            .push_keystroke("● REC Started (Zero-Copy GPU)", self.now_ms());
        Ok(capture_path)
    }

    /// `Some(reason)` when the running capture died on its own (for example
    /// the share was revoked or the encoder failed).
    pub fn poll_capture_failure(&mut self) -> Option<String> {
        self.encoder_session.as_mut()?.poll_failure()
    }

    /// Detaches the running capture so it can be finalized and rendered on a
    /// worker thread with [`StopJob::complete`], keeping the UI responsive.
    pub fn take_recording(&mut self) -> Option<StopJob> {
        let session = self.encoder_session.take()?;
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
            session,
            auto_zoom: self.auto_apply_zoom_on_stop
                && settings.auto_zoom
                && settings.source != CaptureSource::Window,
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
        let (is_recording, recorded_frames, active_encoder) = match &self.encoder_session {
            Some(s) => (true, s.frames_written, s.active_backend.clone()),
            None => (false, 0, self.encoder_backend.short_badge().to_string()),
        };
        TelemetrySnapshot {
            is_recording,
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
    session: ActiveEncoderSession,
    auto_zoom: bool,
    settings: RecorderSettings,
}

impl StopJob {
    /// Finalizes the capture, applies zoom and background, and returns the
    /// MP4 to show the user. A failed render falls back to the plain capture,
    /// so a recording is never lost to a post-processing error.
    pub fn complete(self, progress: &AtomicU32) -> Result<PathBuf> {
        let done = self.session.finish_capture();
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
        let job = RenderJob {
            capture: capture.clone(),
            output: output.clone(),
            events: done.zoom_events,
            auto_zoom: self.auto_zoom,
            region: self.settings.active_region(),
            background: self.settings.background.clone(),
            quality: self.settings.quality,
            fps: self.settings.fps,
        };
        let result = studio_render::render(&job, progress).or_else(|err| {
            eprintln!("[HyperGSR] {err:#}; saving the recording without effects");
            crate::ffmpeg::remux_to_mp4(&capture, &output).map(|_| output.clone())
        });
        match result {
            Ok(path) => {
                let _ = std::fs::remove_file(&capture);
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
}

/// Suffix of the raw capture next to where the final MP4 will go.
const CAPTURE_SUFFIX: &str = ".capture.mp4";

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

    #[test]
    fn capture_files_map_to_their_final_name() {
        let p = std::path::Path::new("/v/HyperGSR_2026-10-23_01-02-03.capture.mp4");
        assert_eq!(final_path(p), PathBuf::from("/v/HyperGSR_2026-10-23_01-02-03.mp4"));
        assert_eq!(final_path(std::path::Path::new("/v/a.mp4")), PathBuf::from("/v/a.mp4"));
    }
}
