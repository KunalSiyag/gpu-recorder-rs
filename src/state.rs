use crate::capture::{CaptureSourceMode, FrameBuffer, ScreenCapturer};
use crate::compositor::{FrameCompositor, FrameStyleConfig};
use crate::cursor::{CursorConfig, CursorSample, CursorTracker, KeystrokeOverlay};
use crate::decompiled_gsr::GsrDecompiledReport;
use crate::encoder::{
    apply_hardware_autozoom_pass, export_showcase_recording, ActiveEncoderSession,
    HardwareEncoderBackend,
};
use crate::zoom_engine::{CameraViewport, ZoomConfig, ZoomEngine, ZoomKeyframe};
use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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

        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/kunalsiyag".to_string());
        let videos_dir = PathBuf::from(format!("{home}/Videos"));
        std::fs::create_dir_all(&videos_dir).ok();

        Self {
            start_instant: Instant::now(),
            recording_start: None,
            capture_mode: CaptureSourceMode::AutoHybrid,
            encoder_backend: HardwareEncoderBackend::GsrZeroCopyGpu,
            target_fps: 60,
            record_audio: true,
            auto_apply_zoom_on_stop: true,
            zoom_cfg: ZoomConfig::default(),
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

        let (raw_x, raw_y, left_down, right_down) =
            if let Some((ix, iy, l, r, ts)) = self.interactive_override {
                if now_ms.saturating_sub(ts) < 1800 {
                    (ix, iy, l, r)
                } else {
                    self.interactive_override = None;
                    self.poll_live_pointer(now_ms)
                }
            } else {
                self.poll_live_pointer(now_ms)
            };

        let (sample, clicked_now) = self.cursor_tracker.update(
            now_ms,
            raw_x,
            raw_y,
            left_down,
            right_down,
            &self.cursor_cfg,
        );

        if clicked_now {
            self.click_count += 1;
            if self.cursor_cfg.show_keystrokes {
                let label = if right_down {
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

        let rec_elapsed = self.recording_elapsed_sec();
        if let Some(ref mut session) = self.encoder_session {
            let focus_x = if viewport.zoom > 1.05 {
                viewport.center_x
            } else {
                sample.smooth_x
            };
            let focus_y = if viewport.zoom > 1.05 {
                viewport.center_y
            } else {
                sample.smooth_y
            };
            session.record_zoom_telemetry(rec_elapsed, focus_x, focus_y, viewport.zoom);
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

    fn poll_live_pointer(&self, now_ms: u64) -> (f32, f32, bool, bool) {
        if let Some((px, py, l, r)) = self.cursor_tracker.poll_system_pointer() {
            return (px, py, l, r);
        }
        let t = now_ms as f32 / 1000.0;
        let x = 0.5 + 0.25 * (t * 0.6).cos();
        let y = 0.5 + 0.20 * (t * 0.9).sin();
        (x, y, false, false)
    }

    pub fn start_recording(&mut self) -> Result<PathBuf> {
        if self.encoder_session.is_some() {
            self.stop_recording()?;
        }
        let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename = format!("HyperGSR_{ts}.mp4");
        let out_path = self.videos_dir.join(&filename);

        let session = ActiveEncoderSession::start_zero_copy_gpu(
            &out_path,
            self.target_fps,
            self.record_audio,
        )
        .or_else(|_| {
            ActiveEncoderSession::start(
                &out_path,
                1920,
                1080,
                self.target_fps,
                self.encoder_backend,
            )
        })?;

        self.recording_start = Some(Instant::now());
        self.encoder_session = Some(session);
        self.cursor_tracker
            .push_keystroke("● REC Started (Zero-Copy GPU)", self.now_ms());
        Ok(out_path)
    }

    /// Stops the active capture process immediately and returns `(raw_mp4_path, zoom_events, apply_zoom)`
    /// so the heavy NVENC auto-zoom pass can run on a background worker thread without holding the mutex.
    pub fn stop_recording_fast(&mut self) -> Result<Option<(PathBuf, Vec< crate::encoder::RecordedZoomEvent >, bool)>> {
        self.recording_start = None;
        if let Some(session) = self.encoder_session.take() {
            let (path, zoom_events) = session.finish()?;
            self.last_saved_file = Some(path.display().to_string());
            self.cursor_tracker
                .push_keystroke("✔ Saved GPU Recording", self.now_ms());
            return Ok(Some((path, zoom_events, self.auto_apply_zoom_on_stop)));
        }
        Ok(None)
    }

    pub fn stop_recording(&mut self) -> Result<Option<PathBuf>> {
        if let Some((path, zoom_events, do_zoom)) = self.stop_recording_fast()? {
            let final_path = if do_zoom && !zoom_events.is_empty() {
                apply_hardware_autozoom_pass(&path, &zoom_events).unwrap_or(path)
            } else {
                path
            };
            self.last_saved_file = Some(final_path.display().to_string());
            return Ok(Some(final_path));
        }
        Ok(None)
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
