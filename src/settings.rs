//! Recorder settings chosen in the island, persisted as JSON in the config
//! directory so the next launch records the same way.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureSource {
    Screen,
    Window,
    /// A rectangle of a screen, chosen with the region overlay.
    Region,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioMode {
    Off,
    System,
    Microphone,
    Both,
}

impl AudioMode {
    pub fn records_system(self) -> bool {
        matches!(self, AudioMode::System | AudioMode::Both)
    }

    pub fn records_microphone(self) -> bool {
        matches!(self, AudioMode::Microphone | AudioMode::Both)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    /// Smaller files, still sharp text.
    Standard,
    /// Near-transparent quality for editing and presenting.
    High,
}

/// Shape of an exported video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Aspect {
    /// The recording's own shape.
    #[default]
    Original,
    Wide,
    Vertical,
    Square,
}

impl Aspect {
    /// Width to height, or `None` for the recording's own shape.
    pub fn ratio(self) -> Option<(usize, usize)> {
        match self {
            Aspect::Original => None,
            Aspect::Wide => Some((16, 9)),
            Aspect::Vertical => Some((9, 16)),
            Aspect::Square => Some((1, 1)),
        }
    }

    /// Output size for a `src`-sized recording: vertical and square videos
    /// keep the recording's short side, wide ones its long side. Sizes are
    /// even and within what hardware encoders accept.
    pub fn output_size(self, src: (usize, usize)) -> (usize, usize) {
        let Some((a, b)) = self.ratio() else { return src };
        let (short, long) = (src.0.min(src.1), src.0.max(src.1));
        let (w, h) = if a > b { (long, long * b / a) } else { (short, short * b / a) };
        let fit = (4096.0 / w.max(h) as f64).min(1.0);
        let even = |v: usize| (((v as f64 * fit) / 2.0).round() as usize * 2).max(16);
        (even(w), even(h))
    }
}

/// Container and codec of an exported video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExportFormat {
    #[default]
    Mp4,
    /// VP9 and Opus, for the web.
    WebM,
    /// Silent, 15 frames per second, at most 960 pixels wide.
    Gif,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Mp4 => "mp4",
            ExportFormat::WebM => "webm",
            ExportFormat::Gif => "gif",
        }
    }
}

/// Normalized rectangle (0..1) of the captured monitor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Region {
    /// Clamps the rectangle into the unit square; `None` when it is too small
    /// to record.
    pub fn normalized(self) -> Option<Region> {
        let x = self.x.clamp(0.0, 1.0);
        let y = self.y.clamp(0.0, 1.0);
        let w = self.w.min(1.0 - x);
        let h = self.h.min(1.0 - y);
        (w >= 0.02 && h >= 0.02).then_some(Region { x, y, w, h })
    }

    pub fn is_full(&self) -> bool {
        self.x <= 0.001 && self.y <= 0.001 && self.w >= 0.999 && self.h >= 0.999
    }

    /// FFmpeg crop filter for this region, rounded to even pixel sizes so
    /// 4:2:0 encoders accept it.
    pub fn crop_filter(&self) -> String {
        format!(
            "crop=w=trunc(iw*{w:.5}/2)*2:h=trunc(ih*{h:.5}/2)*2:x=trunc(iw*{x:.5}):y=trunc(ih*{y:.5})",
            w = self.w,
            h = self.h,
            x = self.x,
            y = self.y
        )
    }

    /// Maps a point normalized to the whole monitor into this region.
    pub fn map_point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            ((x - self.x) / self.w.max(1e-4)).clamp(0.0, 1.0),
            ((y - self.y) / self.h.max(1e-4)).clamp(0.0, 1.0),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BackgroundKind {
    None,
    /// Index into `backgrounds::PRESETS`.
    Preset(usize),
    Image(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundSettings {
    pub kind: BackgroundKind,
    /// Space around the recording, as a fraction of each side (0.0..0.2).
    pub padding: f32,
    /// Corner radius as a fraction of the recording's shorter side.
    pub radius: f32,
    pub shadow: bool,
}

impl Default for BackgroundSettings {
    fn default() -> Self {
        Self {
            kind: BackgroundKind::None,
            padding: 0.06,
            radius: 0.018,
            shadow: true,
        }
    }
}

impl BackgroundSettings {
    pub fn is_enabled(&self) -> bool {
        !matches!(self.kind, BackgroundKind::None)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderSettings {
    pub source: CaptureSource,
    pub region: Option<Region>,
    pub audio: AudioMode,
    pub fps: u32,
    pub quality: Quality,
    pub show_cursor: bool,
    /// Size of the studio-drawn cursor (1.0 is the desktop's own size).
    pub cursor_scale: f32,
    /// Show a ripple where you clicked.
    pub click_ripples: bool,
    pub auto_zoom: bool,
    pub background: BackgroundSettings,
    /// Where recordings are saved; `None` means `~/Videos`.
    pub save_dir: Option<PathBuf>,
    /// Seconds counted down once the screen share is running, left out of
    /// the video; 0 starts at once.
    pub countdown_sec: u32,
}

impl Default for RecorderSettings {
    fn default() -> Self {
        Self {
            source: CaptureSource::Screen,
            region: None,
            audio: AudioMode::System,
            fps: 60,
            quality: Quality::High,
            show_cursor: true,
            cursor_scale: 1.0,
            click_ripples: true,
            auto_zoom: true,
            background: BackgroundSettings::default(),
            save_dir: None,
            countdown_sec: 3,
        }
    }
}

impl RecorderSettings {
    fn file() -> PathBuf {
        crate::bundled::config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::file())
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .map(Self::sanitized)
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::file();
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn sanitized(mut self) -> Self {
        self.fps = if self.fps <= 30 { 30 } else { 60 };
        self.countdown_sec = self.countdown_sec.min(10);
        self.cursor_scale = if self.cursor_scale.is_finite() { self.cursor_scale.clamp(1.0, 2.0) } else { 1.0 };
        self.region = self.region.and_then(Region::normalized);
        // A folder that was removed or unmounted falls back to ~/Videos.
        self.save_dir = self.save_dir.filter(|d| d.is_absolute() && d.is_dir());
        self.background.padding = self.background.padding.clamp(0.0, 0.2);
        self.background.radius = self.background.radius.clamp(0.0, 0.08);
        if let BackgroundKind::Preset(i) = self.background.kind {
            if i >= crate::backgrounds::PRESETS.len() {
                self.background.kind = BackgroundKind::Preset(0);
            }
        }
        self
    }

    /// Folder new recordings go to.
    pub fn recordings_dir(&self) -> PathBuf {
        self.save_dir
            .clone()
            .unwrap_or_else(|| crate::bundled::home_dir().join("Videos"))
    }

    /// The region to crop, when the source is a region that is not the
    /// whole screen.
    pub fn active_region(&self) -> Option<Region> {
        match self.source {
            CaptureSource::Region => self.region.filter(|r| !r.is_full()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_is_clamped_and_rejects_slivers() {
        let r = Region { x: 0.9, y: -0.1, w: 0.5, h: 0.5 }.normalized().unwrap();
        assert_eq!(r.x, 0.9);
        assert_eq!(r.y, 0.0);
        assert!((r.w - 0.1).abs() < 1e-6);
        assert!(Region { x: 0.0, y: 0.0, w: 0.01, h: 0.5 }.normalized().is_none());
    }

    #[test]
    fn region_maps_monitor_points_inside() {
        let r = Region { x: 0.25, y: 0.5, w: 0.5, h: 0.25 };
        assert_eq!(r.map_point(0.5, 0.625), (0.5, 0.5));
        assert_eq!(r.map_point(0.0, 1.0), (0.0, 1.0));
    }

    #[test]
    fn export_shapes_keep_the_recordings_resolution() {
        assert_eq!(Aspect::Original.output_size((1366, 768)), (1366, 768));
        assert_eq!(Aspect::Vertical.output_size((1920, 1080)), (1080, 1920));
        assert_eq!(Aspect::Square.output_size((1920, 1080)), (1080, 1080));
        assert_eq!(Aspect::Wide.output_size((1920, 1080)), (1920, 1080));
        assert_eq!(Aspect::Wide.output_size((800, 1200)), (1200, 676));
        // 4K-wide screens stay within the H.264 hardware limit.
        let (w, h) = Aspect::Vertical.output_size((5120, 2880));
        assert!(w <= 4096 && h <= 4096 && w % 2 == 0 && h % 2 == 0, "{w}x{h}");
    }

    #[test]
    fn old_or_partial_settings_files_load_with_defaults() {
        let s: RecorderSettings = serde_json::from_str(r#"{"fps": 45}"#).unwrap();
        let s = s.sanitized();
        assert_eq!(s.fps, 60);
        assert_eq!(s.audio, AudioMode::System);
        assert!(!s.background.is_enabled());
    }
}
