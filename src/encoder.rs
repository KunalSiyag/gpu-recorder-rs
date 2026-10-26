use crate::capture::{CaptureSourceMode, FrameBuffer, ScreenCapturer};
use crate::compositor::{FrameCompositor, FrameStyleConfig};
use crate::cursor::{CursorConfig, CursorTracker};
use crate::zoom_engine::{ZoomConfig, ZoomEngine};
use crate::settings::{AudioMode, CaptureSource, Quality, Region};
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareEncoderBackend {
    /// Zero-copy PipeWire DMA-BUF + GPU VAAPI/NVENC engine (via decompiled GSR binary + host bridge fix)
    GsrZeroCopyGpu,
    NvidiaNvencH264,
    NvidiaNvencHevc,
    NvidiaNvencAv1,
    AmdVaapiH264,
    SoftwareX264Fast,
}

impl HardwareEncoderBackend {
    pub fn label(&self) -> &'static str {
        match self {
            HardwareEncoderBackend::GsrZeroCopyGpu => {
                "Zero-Copy GPU (PipeWire DMA-BUF + VAAPI/NVENC)"
            }
            HardwareEncoderBackend::NvidiaNvencH264 => "NVIDIA NVENC H.264 (Hardware)",
            HardwareEncoderBackend::NvidiaNvencHevc => "NVIDIA NVENC HEVC (Hardware)",
            HardwareEncoderBackend::NvidiaNvencAv1 => "NVIDIA NVENC AV1 (Hardware)",
            HardwareEncoderBackend::AmdVaapiH264 => "AMD Radeon VAAPI H.264 (Hardware)",
            HardwareEncoderBackend::SoftwareX264Fast => "CPU x264 Ultrafast (Fallback)",
        }
    }

    pub fn short_badge(&self) -> &'static str {
        match self {
            HardwareEncoderBackend::GsrZeroCopyGpu => "DMA-BUF GPU",
            HardwareEncoderBackend::NvidiaNvencH264 => "NVENC H.264",
            HardwareEncoderBackend::NvidiaNvencHevc => "NVENC HEVC",
            HardwareEncoderBackend::NvidiaNvencAv1 => "NVENC AV1",
            HardwareEncoderBackend::AmdVaapiH264 => "VAAPI H.264",
            HardwareEncoderBackend::SoftwareX264Fast => "x264 Fast",
        }
    }

    pub fn ffmpeg_codec_name(&self) -> &'static str {
        match self {
            HardwareEncoderBackend::GsrZeroCopyGpu => "h264_vaapi (zero-copy)",
            HardwareEncoderBackend::NvidiaNvencH264 => "h264_nvenc",
            HardwareEncoderBackend::NvidiaNvencHevc => "hevc_nvenc",
            HardwareEncoderBackend::NvidiaNvencAv1 => "av1_nvenc",
            HardwareEncoderBackend::AmdVaapiH264 => "h264_vaapi",
            HardwareEncoderBackend::SoftwareX264Fast => "libx264",
        }
    }
}

/// Recorded cursor & zoom keyframe sample during a live zero-copy GPU recording session
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RecordedZoomEvent {
    pub timestamp_sec: f32,
    pub center_x: f32,
    pub center_y: f32,
    pub zoom: f32,
    #[serde(default)]
    pub cursor_x: f32,
    #[serde(default)]
    pub cursor_y: f32,
    #[serde(default)]
    pub clicked: bool,
    #[serde(default)]
    pub manual_lock: bool,
    /// The cursor was on a different display than the one being recorded.
    #[serde(default)]
    pub off_capture: bool,
}

/// Whether a cursor on a `cursor_output`-sized display can be on the captured
/// monitor of `capture` pixels. Displays of equal size cannot be told apart
/// and count as a match; a rotated display matches either orientation.
pub fn cursor_on_capture(capture: (u32, u32), cursor_output: (f32, f32)) -> bool {
    let (w, h) = (capture.0 as f32, capture.1 as f32);
    let same = |a: f32, b: f32| (a - b).abs() <= 2.0;
    (same(w, cursor_output.0) && same(h, cursor_output.1))
        || (same(w, cursor_output.1) && same(h, cursor_output.0))
}

pub enum RecordingEngineKind {
    /// PipeWire + XDG Desktop Portal capture helper (`hypergsr-pw-capture`)
    /// feeding an FFmpeg hardware encoder.
    ZeroCopyGsr {
        child: Child,
        stdin: Option<ChildStdin>,
        status: Arc<Mutex<CaptureStatus>>,
        /// Parses the helper's stdout; joined once the helper exits so its
        /// last ERROR line is never missed.
        reader: Option<std::thread::JoinHandle<()>>,
    },
    /// Raw frame pipe to FFmpeg hardware encoder (for synthetic/studio showcase rendering)
    FfmpegPipe {
        child: Child,
        stdin: Option<ChildStdin>,
    },
}

/// What the capture helper has reported on stdout so far.
#[derive(Debug, Default)]
pub struct CaptureStatus {
    /// When the first frame reached the encoder: time zero of the video.
    pub ready_at: Option<Instant>,
    pub size: Option<(u32, u32)>,
    pub error: Option<String>,
    /// FFmpeg's pid, which is also its process group.
    pub encoder_pid: Option<i32>,
    /// Since when no new image arrived, and the video time of the last one.
    pub stalled: Option<(Instant, f64)>,
    /// The stream went away mid-recording: the video time worth keeping, and why.
    pub lost: Option<(f64, String)>,
}

/// How a capture that ended on its own ended.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureExit {
    /// The screen share broke mid-recording; what was captured is intact and
    /// the recording can continue in a new capture.
    Interrupted(String),
    Failed(String),
}

/// How a screen recording is captured.
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub fps: u32,
    pub quality: Quality,
    pub audio: AudioMode,
    pub source: CaptureSource,
    pub region: Option<Region>,
    pub show_cursor: bool,
    /// Record the cursor beside the video (see `cursor_track`) so the
    /// studio render can draw it smoothed; needs [`portal_cursor_metadata`].
    pub studio_cursor: bool,
    /// Ignore the remembered portal selection and show the picker again.
    pub repick: bool,
}

/// Whether the screen-cast portal can send the cursor as metadata instead
/// of drawing it into the frames. Asked once per run.
pub fn portal_cursor_metadata() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        use gio::prelude::*;
        const METADATA: u32 = 4;
        let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
            return false;
        };
        bus.call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&("org.freedesktop.portal.ScreenCast", "AvailableCursorModes").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            3000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.child_value(0).as_variant())
        .and_then(|modes| modes.get::<u32>())
        .is_some_and(|modes| modes & METADATA != 0)
    })
}

/// Result of stopping a capture.
pub struct FinishedCapture {
    pub path: PathBuf,
    pub zoom_events: Vec<RecordedZoomEvent>,
    /// Set when the helper reported a failure or exited abnormally.
    pub error: Option<String>,
    /// Video time after which the capture only repeats a frozen frame.
    pub keep_sec: Option<f64>,
    /// The cursor, when it was recorded beside the video.
    pub cursor: Option<crate::cursor_track::CursorTrack>,
}

const HELPER_PROTOCOL: &str = "5";
/// The helper's exit code after it reported LOST.
const EXIT_STREAM_LOST: i32 = 6;

pub struct ActiveEncoderSession {
    pub kind: RecordingEngineKind,
    pub output_path: PathBuf,
    pub frames_written: u64,
    pub active_backend: String,
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub zoom_events: Vec<RecordedZoomEvent>,
    /// Set when the recorder itself ended a capture whose stream froze.
    interrupted: Option<(f64, String)>,
    /// Where the helper writes the cursor, in studio-cursor mode.
    cursor_track: Option<PathBuf>,
}

/// Applies one line of the helper's stdout protocol.
fn apply_helper_line(st: &mut CaptureStatus, line: &str) {
    if let Some(rest) = line.strip_prefix("READY ") {
        let mut it = rest.split_whitespace().filter_map(|v| v.parse::<u32>().ok());
        st.size = it.next().zip(it.next());
        st.ready_at.get_or_insert_with(Instant::now);
    } else if let Some(msg) = line.strip_prefix("ERROR ") {
        st.error.get_or_insert_with(|| msg.to_string());
    } else if let Some(pid) = line.strip_prefix("ENCODER ") {
        st.encoder_pid = pid.trim().parse().ok().filter(|&p: &i32| p > 0);
    } else if let Some(sec) = line.strip_prefix("STALLED ") {
        if let Ok(sec) = sec.trim().parse::<f64>() {
            st.stalled = Some((Instant::now(), sec));
        }
    } else if line == "FLOWING" {
        st.stalled = None;
    } else if let Some(rest) = line.strip_prefix("LOST ") {
        let (sec, reason) = rest.split_once(' ').unwrap_or((rest, ""));
        if let Ok(sec) = sec.parse::<f64>() {
            let reason = if reason.is_empty() { "the screen share stopped" } else { reason };
            st.lost = Some((sec, reason.to_string()));
        }
    }
}

impl ActiveEncoderSession {
    /// Starts a portal screen capture through `hypergsr-pw-capture`. The
    /// remembered portal selection is reused, so the picker only appears the
    /// first time (or when `options.repick` is set). Recording time zero is
    /// the first captured frame; see [`Self::ready_at`].
    pub fn start_capture(output_path: impl AsRef<Path>, options: &CaptureOptions) -> Result<Self> {
        let out_path = output_path.as_ref().to_path_buf();
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let caps = crate::ffmpeg::capabilities();
        if !caps.ffmpeg {
            bail!("FFmpeg is not installed; install it from your distribution to record");
        }
        let audio = if caps.pulse { options.audio } else { AudioMode::Off };
        let helper_bin = Self::find_capture_helper()?;
        let token_file = Self::portal_restore_token_file(options.source)?;

        let mut cmd = Command::new(&helper_bin);
        cmd.arg("--token")
            .arg(&token_file)
            .args(["--fps", &options.fps.to_string()])
            .args([
                "--sources",
                if options.source == CaptureSource::Window { "window" } else { "monitor" },
            ]);
        let cursor_track = (options.show_cursor && options.studio_cursor)
            .then(|| crate::cursor_track::track_path(&out_path));
        match &cursor_track {
            Some(track) => {
                cmd.args(["--cursor", "metadata", "--cursor-track"]).arg(track);
            }
            None => {
                cmd.args(["--cursor", if options.show_cursor { "embedded" } else { "hidden" }]);
            }
        }
        if options.repick {
            cmd.arg("--reset");
        }
        cmd.arg("--").args(crate::ffmpeg::capture_output_args(
            &caps.encoder,
            options.quality,
            options.fps,
            audio,
            options.region,
            &out_path,
        ));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(capture_log());

        let mut child = cmd
            .spawn()
            .context("Failed to spawn the hypergsr-pw-capture helper")?;
        let stdin = child.stdin.take();
        let status = Arc::new(Mutex::new(CaptureStatus::default()));
        let reader = child.stdout.take().map(|stdout| {
            let status = status.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    apply_helper_line(&mut status.lock(), &line);
                }
            })
        });

        Ok(Self {
            kind: RecordingEngineKind::ZeroCopyGsr { child, stdin, status, reader },
            output_path: out_path,
            frames_written: 0,
            active_backend: format!("PipeWire Portal + {}", caps.encoder.label()),
            width: 0,
            height: 0,
            fps: options.fps,
            zoom_events: Vec::with_capacity(4096),
            interrupted: None,
            cursor_track,
        })
    }

    /// When the first frame reached the encoder, once capture is running.
    pub fn ready_at(&self) -> Option<Instant> {
        match &self.kind {
            RecordingEngineKind::ZeroCopyGsr { status, .. } => status.lock().ready_at,
            RecordingEngineKind::FfmpegPipe { .. } => None,
        }
    }

    /// Set once the capture process has exited on its own (or after
    /// [`Self::interrupt`]).
    pub fn poll_exit(&mut self) -> Option<CaptureExit> {
        let RecordingEngineKind::ZeroCopyGsr { child, status, reader, .. } = &mut self.kind else {
            return None;
        };
        let exit = child.try_wait().ok().flatten()?;
        if let Some(reader) = reader.take() {
            let _ = reader.join();
        }
        let st = status.lock();
        if let Some((_, reason)) = self.interrupted.as_ref().or(st.lost.as_ref()) {
            if st.ready_at.is_some() && st.error.is_none() {
                return Some(CaptureExit::Interrupted(reason.clone()));
            }
        }
        Some(CaptureExit::Failed(match (&st.error, st.ready_at) {
            (Some(msg), _) => msg.clone(),
            (None, None) if exit.success() => "screen sharing was cancelled".to_string(),
            (None, _) => format!("the capture process stopped unexpectedly ({exit})"),
        }))
    }

    /// Since when the stream has sent no new image, and the video time of
    /// the last one. A still screen stalls too.
    pub fn stalled(&self) -> Option<(Instant, f64)> {
        match &self.kind {
            RecordingEngineKind::ZeroCopyGsr { status, .. } => status.lock().stalled,
            RecordingEngineKind::FfmpegPipe { .. } => None,
        }
    }

    /// Ends a capture whose stream has frozen; [`Self::poll_exit`] then
    /// reports it as interrupted, keeping the video up to `keep_sec`.
    pub fn interrupt(&mut self, keep_sec: f64, reason: &str) {
        if self.interrupted.is_some() {
            return;
        }
        self.interrupted = Some((keep_sec, reason.to_string()));
        if let RecordingEngineKind::ZeroCopyGsr { stdin, .. } = &mut self.kind {
            drop(stdin.take());
        }
    }

    fn portal_restore_token_file(source: CaptureSource) -> Result<PathBuf> {
        let config_dir = crate::bundled::config_dir();
        std::fs::create_dir_all(&config_dir)?;
        if source == CaptureSource::Window {
            return Ok(config_dir.join("restore_token_window"));
        }
        let hypergsr_token = config_dir.join("restore_token");
        let legacy_token = crate::bundled::home_dir()
            .join(".var/app/com.dec05eba.gpu_screen_recorder/config/gpu-screen-recorder/restore_token");
        if !hypergsr_token.exists() && legacy_token.exists() {
            let _ = std::fs::copy(&legacy_token, &hypergsr_token);
        }
        Ok(hypergsr_token)
    }

    fn helper_speaks_protocol(path: &Path) -> bool {
        let Ok(mut child) = Command::new(path)
            .arg("--protocol")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                return out.trim() == HELPER_PROTOCOL;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        false
    }

    /// Resolves the capture helper ahead of the first recording, so pressing
    /// Record does not wait for the lookup.
    pub fn warm_up() {
        let _ = Self::find_capture_helper();
    }

    fn find_capture_helper() -> Result<PathBuf> {
        // Only a hit is cached, so a slow first probe or a helper installed
        // later does not disable recording until restart.
        static HELPER: Mutex<Option<PathBuf>> = Mutex::new(None);
        if let Some(found) = HELPER.lock().clone() {
            return Ok(found);
        }
        let found = Self::search_capture_helper().ok();
        *HELPER.lock() = found.clone();
        found.context(
                "No compatible hypergsr-pw-capture helper was found; reinstall HyperGSR \
                 (building from source needs gcc, pkg-config, PipeWire, GLib and libportal)",
            )
    }

    /// Finds a capture helper that matches this build. npm installs it beside
    /// the recorder, system packages under lib/hypergsr, and every build
    /// embeds one; stale copies from older releases are skipped.
    fn search_capture_helper() -> Result<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let installed = [
            Some(exe_dir.join("hypergsr-pw-capture")),
            Some(exe_dir.join("../lib/hypergsr/hypergsr-pw-capture")),
            crate::bundled::pw_capture_helper(),
            Some(exe_dir.join("../../bin/hypergsr-pw-capture")),
        ];
        installed
            .into_iter()
            .flatten()
            .find(|cand| cand.exists() && Self::helper_speaks_protocol(cand))
            .context(
                "No compatible hypergsr-pw-capture helper was found; reinstall HyperGSR \
                 (building from source needs gcc, pkg-config, PipeWire, GLib and libportal)",
            )
    }

    pub fn start(
        output_path: impl AsRef<Path>,
        width: usize,
        height: usize,
        fps: u32,
        preferred: HardwareEncoderBackend,
    ) -> Result<Self> {
        let out_path = output_path.as_ref().to_path_buf();
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let size_arg = format!("{width}x{height}");
        let fps_arg = fps.to_string();

        let try_spawn = |backend: HardwareEncoderBackend| -> Result<Child> {
            let mut cmd = Command::new("ffmpeg");
            cmd.args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-s",
                &size_arg,
                "-r",
                &fps_arg,
                "-i",
                "pipe:0",
            ]);

            match backend {
                HardwareEncoderBackend::NvidiaNvencH264 | HardwareEncoderBackend::GsrZeroCopyGpu => {
                    cmd.args([
                        "-c:v",
                        "h264_nvenc",
                        "-preset",
                        "p4",
                        "-pix_fmt",
                        "yuv420p",
                        "-b:v",
                        "12M",
                    ]);
                }
                HardwareEncoderBackend::NvidiaNvencHevc => {
                    cmd.args([
                        "-c:v",
                        "hevc_nvenc",
                        "-preset",
                        "p4",
                        "-pix_fmt",
                        "yuv420p",
                        "-b:v",
                        "12M",
                    ]);
                }
                HardwareEncoderBackend::NvidiaNvencAv1 => {
                    cmd.args(["-c:v", "av1_nvenc", "-pix_fmt", "yuv420p", "-b:v", "10M"]);
                }
                HardwareEncoderBackend::AmdVaapiH264 => {
                    cmd.args([
                        "-vaapi_device",
                        "/dev/dri/renderD128",
                        "-vf",
                        "format=nv12,hwupload",
                        "-c:v",
                        "h264_vaapi",
                        "-qp",
                        "20",
                    ]);
                }
                HardwareEncoderBackend::SoftwareX264Fast => {
                    cmd.args([
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-crf",
                        "18",
                        "-pix_fmt",
                        "yuv420p",
                    ]);
                }
            }

            cmd.arg(&out_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("Failed to spawn ffmpeg encoder process")
        };

        let mut child = try_spawn(preferred)?;
        let active_backend = preferred.ffmpeg_codec_name().to_string();
        let stdin = child.stdin.take();

        Ok(Self {
            kind: RecordingEngineKind::FfmpegPipe { child, stdin },
            output_path: out_path,
            frames_written: 0,
            active_backend,
            width,
            height,
            fps,
            zoom_events: Vec::new(),
            interrupted: None,
            cursor_track: None,
        })
    }

    pub fn record_zoom_telemetry(
        &mut self,
        t_sec: f32,
        cx: f32,
        cy: f32,
        zoom: f32,
        cursor_x: f32,
        cursor_y: f32,
        clicked: bool,
        manual_lock: bool,
        off_capture: bool,
    ) {
        self.frames_written += 1;
        let should_push = clicked
            || self
                .zoom_events
                .last()
                .map(|last| {
                    last.manual_lock != manual_lock
                        || last.off_capture != off_capture
                        || (t_sec - last.timestamp_sec) >= 0.015
                })
                .unwrap_or(true);
        // About 3 hours of telemetry at 60 Hz before samples are thinned out.
        let thinned = self.zoom_events.len() >= 600_000
            && self
                .zoom_events
                .last()
                .is_some_and(|last| t_sec - last.timestamp_sec < 0.25);
        if should_push && !thinned {
            self.zoom_events.push(RecordedZoomEvent {
                timestamp_sec: t_sec,
                center_x: cx,
                center_y: cy,
                zoom,
                cursor_x,
                cursor_y,
                clicked,
                manual_lock,
                off_capture,
            });
        }
    }

    /// Raw size of the captured stream (a whole monitor for screen and area
    /// capture), once the first frame arrived.
    pub fn capture_size(&self) -> Option<(u32, u32)> {
        match &self.kind {
            RecordingEngineKind::ZeroCopyGsr { status, .. } => status.lock().size,
            RecordingEngineKind::FfmpegPipe { .. } => None,
        }
    }

    pub fn write_frame(&mut self, frame: &FrameBuffer) -> Result<()> {
        if let RecordingEngineKind::FfmpegPipe {
            ref mut child,
            ref mut stdin,
        } = self.kind
        {
            if let Some(ref mut pipe) = stdin {
                if pipe.write_all(&frame.data).is_err() {
                    let _ = child.wait();
                    let mut fallback = Self::start(
                        &self.output_path,
                        self.width,
                        self.height,
                        self.fps,
                        HardwareEncoderBackend::SoftwareX264Fast,
                    )?;
                    fallback.write_frame(frame)?;
                    *self = fallback;
                    return Ok(());
                }
                self.frames_written += 1;
            }
        }
        Ok(())
    }

    /// Stops the capture and waits for the file to be finalized. This can take
    /// a few seconds, so call it off the UI thread.
    pub fn finish_capture(mut self) -> FinishedCapture {
        let mut error = None;
        let mut keep_sec = self.interrupted.as_ref().map(|(sec, _)| *sec);
        match self.kind {
            RecordingEngineKind::ZeroCopyGsr {
                ref mut child,
                ref mut stdin,
                ref status,
                ref mut reader,
            } => {
                // Closing stdin is the helper's stop signal.
                drop(stdin.take());
                let exit = wait_with_escalation(child);
                if let Some(reader) = reader.take() {
                    let _ = reader.join();
                }
                // A helper that died or was killed leaves FFmpeg finishing the
                // file on its own; wait for it so nobody reads a half-written file.
                let lost = status.lock().lost.clone();
                let helper_clean = exit.is_some_and(|e| {
                    e.success() || (lost.is_some() && e.code() == Some(EXIT_STREAM_LOST))
                });
                if let (false, Some(pid)) = (helper_clean, status.lock().encoder_pid) {
                    reap_orphaned_encoder(pid);
                }
                let st = status.lock();
                error = st.error.clone();
                if error.is_none() && st.ready_at.is_none() {
                    error = Some("no frames were captured".to_string());
                } else if error.is_none() && !helper_clean {
                    error = Some("the capture process did not exit cleanly".to_string());
                }
                if let Some((sec, _)) = lost {
                    keep_sec = Some(keep_sec.map_or(sec, |k| k.min(sec)));
                }
            }
            RecordingEngineKind::FfmpegPipe {
                ref mut child,
                ref mut stdin,
            } => {
                drop(stdin.take());
                let _ = child.wait();
            }
        }
        let cursor = self.cursor_track.and_then(|track| {
            let loaded = crate::cursor_track::CursorTrack::load(&track);
            let _ = std::fs::remove_file(&track);
            loaded
                .map_err(|err| eprintln!("[HyperGSR] cursor track unusable: {err:#}"))
                .ok()
        });
        FinishedCapture {
            path: self.output_path,
            zoom_events: self.zoom_events,
            error,
            keep_sec,
            cursor,
        }
    }

    pub fn finish(self) -> Result<(PathBuf, Vec<RecordedZoomEvent>)> {
        let done = self.finish_capture();
        Ok((done.path, done.zoom_events))
    }
}

/// Waits for `child`, escalating from a polite wait to SIGTERM and SIGKILL.
fn wait_with_escalation(child: &mut Child) -> Option<std::process::ExitStatus> {
    let steps = [(0, 20_000u64), (libc::SIGTERM, 10_000), (libc::SIGKILL, 2_000)];
    for (signal, wait_ms) in steps {
        if signal != 0 {
            unsafe {
                libc::kill(child.id() as i32, signal);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return None,
            }
        }
    }
    None
}

/// Waits for an FFmpeg that outlived its helper, then stops its process group.
fn reap_orphaned_encoder(pid: i32) {
    let alive = || unsafe { libc::kill(pid, 0) == 0 };
    for (signal, wait_ms) in [(0, 10_000u64), (libc::SIGINT, 5_000), (libc::SIGKILL, 1_000)] {
        if !alive() {
            return;
        }
        if signal != 0 {
            unsafe {
                libc::kill(-pid, signal);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Helper and FFmpeg diagnostics go to a log in the cache directory instead
/// of being discarded, so failed recordings can be explained.
fn capture_log() -> Stdio {
    let dir = crate::bundled::cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("capture.log");
    // Keep the log from growing without bound across many sessions.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&path, dir.join("capture.log.1"));
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            let _ = writeln!(file, "--- {} ---", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
            Stdio::from(file)
        }
        Err(_) => Stdio::null(),
    }
}

/// Builds a Screen Studio-grade, C2-continuous 60.000 Hz camera trajectory `(center_x, center_y, zoom)`
/// from recorded telemetry events.
/// - Enforces bounded zoom hold durations (~1.75s max per focus cluster) so the video NEVER stays stuck zoomed in.
/// - Returns smoothly to `1.0x` wide view during fast cursor travel or idle reading periods.
/// - Integrates a dual-stage critically-damped spring at 240 Hz (4x sub-stepping per 60fps frame)
///   for completely kink-free, cinema-grade motion.
pub fn build_smooth_60fps_camera_frames(
    events: &[RecordedZoomEvent],
    total_frames: usize,
) -> Vec<(f32, f32, f32)> {
    if events.is_empty() {
        return vec![(0.5, 0.5, 1.0); total_frames];
    }

    let peak_zoom = events
        .iter()
        .map(|e| e.zoom)
        .fold(1.0f32, f32::max)
        .clamp(1.75, 2.50);

    // Step 1: Resample cursor position & click/zoom intent at exact 60.000 Hz
    let mut raw_cx = vec![0.5f32; total_frames];
    let mut raw_cy = vec![0.5f32; total_frames];
    let mut rec_z = vec![1.0f32; total_frames];
    let mut rec_click = vec![false; total_frames];
    let mut rec_manual = vec![false; total_frames];

    for f in 0..total_frames {
        let t = f as f32 / 60.0;
        let (cx, cy, z) = sample_trajectory(events, t);
        raw_cx[f] = cx;
        raw_cy[f] = cy;
        rec_z[f] = z;
    }
    for (idx_ev, ev) in events.iter().enumerate() {
        let idx = ((ev.timestamp_sec * 60.0).round() as usize).min(total_frames.saturating_sub(1));
        if ev.clicked {
            rec_click[idx] = true;
        }
        if ev.manual_lock {
            let next_t = events
                .get(idx_ev + 1)
                .map(|n| n.timestamp_sec)
                .unwrap_or(ev.timestamp_sec + 0.02);
            let end_idx = ((next_t * 60.0).ceil() as usize).min(total_frames.saturating_sub(1));
            for m_f in idx..=end_idx {
                rec_manual[m_f] = true;
            }
        }
    }

    // Step 2: Hybrid Manual Liberty + Screen Studio Segment Planner
    // - Whenever `rec_manual[f]` is true (user triggered their Manual Zoom Combo / Lock / Wheel),
    //   the camera honors the user's exact zoom duration and depth without cutting it off!
    // - Whenever `rec_manual[f]` is false, automatic zoom segments hold for at most ~105 frames (1.75s)
    //   and return cleanly to 1.0x wide view.
    let mut target_z = vec![1.0f32; total_frames];
    let mut target_x = vec![0.5f32; total_frames];
    let mut target_y = vec![0.5f32; total_frames];

    let mut hold_remaining: usize = 0;
    let mut cooldown_remaining: usize = 20; // Start first 0.33s at 1.0x wide view
    let mut anchor_x = raw_cx[0];
    let mut anchor_y = raw_cy[0];
    let mut prev_rec_high = false;
    let mut prev_manual = false;
    let mut settle_counter: usize = 0;
    let mut traveled_since_last = true;

    for f in 0..total_frames {
        let cx = raw_cx[f];
        let cy = raw_cy[f];

        if rec_manual[f] {
            if !prev_manual {
                anchor_x = cx;
                anchor_y = cy;
            }
            prev_manual = true;
            hold_remaining = 0;
            let dx = cx - anchor_x;
            let dy = cy - anchor_y;
            let dist = (dx * dx + dy * dy).sqrt();
            let deadzone = 0.045;
            if dist > deadzone {
                let pull = (dist - deadzone) / dist;
                anchor_x += dx * pull * 0.28;
                anchor_y += dy * pull * 0.28;
            }
            target_z[f] = rec_z[f].max(peak_zoom).clamp(1.25, 3.5);
            target_x[f] = anchor_x;
            target_y[f] = anchor_y;
            continue;
        }
        prev_manual = false;

        // Compute cursor speed over a 6-frame (100ms) window
        let prev_f = f.saturating_sub(6);
        let dt_win = ((f - prev_f).max(1) as f32) / 60.0;
        let speed = ((cx - raw_cx[prev_f]).powi(2) + (cy - raw_cy[prev_f]).powi(2)).sqrt() / dt_win;

        let dist_from_anchor = ((cx - anchor_x).powi(2) + (cy - anchor_y).powi(2)).sqrt();
        if dist_from_anchor > 0.14 || speed > 0.55 {
            traveled_since_last = true;
        }

        if speed < 0.25 {
            settle_counter += 1;
        } else {
            settle_counter = 0;
        }

        let rec_rising_edge = rec_z[f] > 1.25 && !prev_rec_high;
        prev_rec_high = rec_z[f] > 1.25;

        // Trigger a bounded 1.75s zoom segment on:
        // 1. Explicit click
        // 2. Rising edge of live zoom trigger
        // 3. Glide-and-settle to a new UI region (settle for 16 frames = 260ms after traveling)
        let settle_trigger = traveled_since_last && settle_counter == 16;

        if cooldown_remaining > 0 {
            cooldown_remaining -= 1;
        }

        if (rec_click[f] || rec_rising_edge || settle_trigger) && cooldown_remaining == 0 && hold_remaining == 0 {
            hold_remaining = 105; // 1.75 seconds hold at 60fps
            traveled_since_last = false;
            anchor_x = cx;
            anchor_y = cy;
        } else if rec_click[f] && hold_remaining > 0 {
            // Clicking while already zoomed in extends hold by 1.2s around the new click
            hold_remaining = hold_remaining.max(72);
            anchor_x = cx;
            anchor_y = cy;
        }

        // Fast cross-screen travel immediately releases zoom back to 1.0x wide view
        if speed > 0.85 && hold_remaining > 0 {
            hold_remaining = 0;
            cooldown_remaining = 36; // 0.6s cooldown
        }

        // Always zoom out to 1.0x for the final 0.45s of the video
        if f + 27 >= total_frames {
            hold_remaining = 0;
        }

        if hold_remaining > 0 {
            hold_remaining -= 1;
            if hold_remaining == 0 {
                cooldown_remaining = 54; // 0.9s wide-view breather before next auto-zoom
            }
            // Deadzone-filtered focal anchor so small mouse wiggles don't shake the camera
            let dx = cx - anchor_x;
            let dy = cy - anchor_y;
            let dist = (dx * dx + dy * dy).sqrt();
            let deadzone = 0.055;
            if dist > deadzone {
                let pull = (dist - deadzone) / dist;
                anchor_x += dx * pull * 0.22;
                anchor_y += dy * pull * 0.22;
            }
            target_z[f] = peak_zoom;
            target_x[f] = anchor_x;
            target_y[f] = anchor_y;
        } else {
            target_z[f] = 1.0;
            target_x[f] = 0.5;
            target_y[f] = 0.5;
        }
    }

    // Step 3: 240 Hz (4x sub-stepped) Critically-Damped Spring Integration
    // Produces a C2-continuous trajectory with zero velocity corners.
    let mut out = Vec::with_capacity(total_frames);
    let mut cam_x = 0.5f32;
    let mut cam_y = 0.5f32;
    let mut cam_z = 1.0f32;
    let mut vx = 0.0f32;
    let mut vy = 0.0f32;
    let mut vz = 0.0f32;

    // Natural frequency omega = 5.2 rad/s (k = 27.0) gives a silky ~0.55s Screen Studio ease-in/out
    let k_z = 28.0f32;
    let c_z = 2.0 * k_z.sqrt();
    let k_xy = 24.0f32;
    let c_xy = 2.0 * k_xy.sqrt();
    let dt_sub = 1.0f32 / 240.0f32;

    for f in 0..total_frames {
        let tx = target_x[f];
        let ty = target_y[f];
        let tz = target_z[f];

        for _ in 0..4 {
            let ax = -k_xy * (cam_x - tx) - c_xy * vx;
            let ay = -k_xy * (cam_y - ty) - c_xy * vy;
            let az = -k_z * (cam_z - tz) - c_z * vz;

            vx += ax * dt_sub;
            vy += ay * dt_sub;
            vz += az * dt_sub;

            cam_x += vx * dt_sub;
            cam_y += vy * dt_sub;
            cam_z = (cam_z + vz * dt_sub).clamp(1.0, 3.5);
        }

        out.push((cam_x.clamp(0.0, 1.0), cam_y.clamp(0.0, 1.0), cam_z));
    }

    out
}

fn sample_trajectory(events: &[RecordedZoomEvent], t_sec: f32) -> (f32, f32, f32) {
    if events.is_empty() {
        return (0.5, 0.5, 1.0);
    }
    let get_xy = |e: &RecordedZoomEvent| -> (f32, f32) {
        if e.cursor_x > 0.001 || e.cursor_y > 0.001 {
            (e.cursor_x, e.cursor_y)
        } else {
            (e.center_x, e.center_y)
        }
    };
    if t_sec <= events[0].timestamp_sec {
        let (x, y) = get_xy(&events[0]);
        return (x, y, events[0].zoom);
    }
    let last = &events[events.len() - 1];
    if t_sec >= last.timestamp_sec {
        let (x, y) = get_xy(last);
        return (x, y, last.zoom);
    }
    for win in events.windows(2) {
        let a = &win[0];
        let b = &win[1];
        if t_sec >= a.timestamp_sec && t_sec <= b.timestamp_sec {
            let span = (b.timestamp_sec - a.timestamp_sec).max(0.0001);
            let u = ((t_sec - a.timestamp_sec) / span).clamp(0.0, 1.0);
            // Smoothstep interpolation between telemetry samples to eliminate piecewise-linear kinks
            let s = u * u * (3.0 - 2.0 * u);
            let (ax, ay) = get_xy(a);
            let (bx, by) = get_xy(b);
            return (
                ax + (bx - ax) * s,
                ay + (by - ay) * s,
                a.zoom + (b.zoom - a.zoom) * s,
            );
        }
    }
    let (x, y) = get_xy(last);
    (x, y, last.zoom)
}

/// Renders a complete, self-contained Studio Demo MP4 showcasing cursor smoothing,
/// click ripples, and spring-physics automatic zoom-ins.
pub fn export_showcase_recording(
    output_path: impl AsRef<Path>,
    width: usize,
    height: usize,
    fps: u32,
    duration_secs: u32,
    backend: HardwareEncoderBackend,
    zoom_cfg: &ZoomConfig,
    cursor_cfg: &CursorConfig,
    style_cfg: &FrameStyleConfig,
) -> Result<(PathBuf, u64, String)> {
    let hw_backend = if backend == HardwareEncoderBackend::GsrZeroCopyGpu {
        HardwareEncoderBackend::NvidiaNvencH264
    } else {
        backend
    };
    let mut capturer = ScreenCapturer::new(width, height);
    let mut raw_fb = FrameBuffer::new(width, height);
    let mut comp_fb = FrameBuffer::new(width, height);
    let mut cursor_tracker = CursorTracker::new();
    let mut zoom_engine = ZoomEngine::new();

    let mut session = ActiveEncoderSession::start(&output_path, width, height, fps, hw_backend)?;
    let total_frames = fps * duration_secs;
    let dt_ms = (1000 / fps.max(1)) as u64;

    for frame_idx in 0..total_frames {
        let now_ms = frame_idx as u64 * dt_ms;
        let t_sec = now_ms as f32 / 1000.0;

        let (raw_x, raw_y, left_down, right_down) = if t_sec < 1.2 {
            let p = (t_sec / 1.2).clamp(0.0, 1.0);
            (
                0.50 - 0.22 * p,
                0.50 - 0.16 * p,
                (0.35..0.42).contains(&t_sec),
                false,
            )
        } else if t_sec < 2.6 {
            let p = ((t_sec - 1.2) / 1.4).clamp(0.0, 1.0);
            (
                0.28 + 0.46 * p,
                0.34 - 0.06 * p,
                (1.45..1.52).contains(&t_sec),
                false,
            )
        } else if t_sec < 3.9 {
            let p = ((t_sec - 2.6) / 1.3).clamp(0.0, 1.0);
            (
                0.74 - 0.06 * p,
                0.28 + 0.44 * p,
                false,
                (2.75..2.82).contains(&t_sec),
            )
        } else {
            let p = ((t_sec - 3.9) / 1.1).clamp(0.0, 1.0);
            (0.68 - 0.18 * p, 0.72 - 0.22 * p, false, false)
        };

        capturer.capture_frame(CaptureSourceMode::StudioWorkspace, now_ms, &mut raw_fb);

        let (sample, clicked) = cursor_tracker.update(
            now_ms,
            raw_x,
            raw_y,
            left_down,
            right_down,
            cursor_cfg,
        );
        if clicked && cursor_cfg.show_keystrokes {
            let label = if right_down {
                "Right Click · Context"
            } else {
                "Left Click · Auto-Zoom"
            };
            cursor_tracker.push_keystroke(label, now_ms);
        }

        let dwell = cursor_tracker.dwell_duration_ms(now_ms);
        let viewport = zoom_engine.step(now_ms, sample, clicked, dwell, zoom_cfg);
        let trail = cursor_tracker.recent_trail(cursor_cfg.trail_length);

        FrameCompositor::render_frame(
            &raw_fb,
            &viewport,
            &sample,
            &trail,
            cursor_tracker.active_ripples(),
            cursor_cfg,
            style_cfg,
            now_ms,
            &mut comp_fb,
        );

        session.write_frame(&comp_fb)?;
    }

    let (saved_path, _) = session.finish()?;
    let active_codec = hw_backend.ffmpeg_codec_name().to_string();
    Ok((saved_path, total_frames as u64, active_codec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stall_and_loss_reports() {
        let mut st = CaptureStatus::default();
        apply_helper_line(&mut st, "READY 1920 1080");
        assert_eq!(st.size, Some((1920, 1080)));
        apply_helper_line(&mut st, "STALLED 64.900");
        assert_eq!(st.stalled.map(|(_, sec)| sec), Some(64.9));
        apply_helper_line(&mut st, "FLOWING");
        assert!(st.stalled.is_none());
        apply_helper_line(&mut st, "LOST 65.250 the screen-cast source went away");
        assert_eq!(st.lost, Some((65.25, "the screen-cast source went away".to_string())));
        apply_helper_line(&mut st, "LOST nope");
        assert_eq!(st.lost.as_ref().map(|l| l.0), Some(65.25));
    }

    #[test]
    fn cursor_display_matching_handles_rotation_and_other_sizes() {
        assert!(cursor_on_capture((1920, 1080), (1920.0, 1080.0)));
        assert!(cursor_on_capture((1080, 1920), (1920.0, 1080.0)));
        assert!(!cursor_on_capture((2560, 1440), (1920.0, 1080.0)));
    }
}
