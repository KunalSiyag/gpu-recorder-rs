use crate::cursor::CursorSample;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoZoomMode {
    /// Zooms into cursor on click, holds for `hold_duration_ms`, then smoothly zooms out
    ClickToZoom,
    /// Automatically zooms in when cursor slows down / dwells on UI elements and pans smoothly
    SmartCursorFollow,
    /// Follows explicit/auto-generated timeline keyframes (Screen Studio style)
    TimelineKeyframes,
    /// Manual hotkey/slider control
    Manual,
}

impl AutoZoomMode {
    pub fn label(&self) -> &'static str {
        match self {
            AutoZoomMode::ClickToZoom => "Click-to-Zoom (Cinematic)",
            AutoZoomMode::SmartCursorFollow => "Smart Dwell & Cursor Follow",
            AutoZoomMode::TimelineKeyframes => "Timeline Keyframes (Studio)",
            AutoZoomMode::Manual => "Manual / Hotkey Target",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomKeyframe {
    pub id: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub target_x: f32,
    pub target_y: f32,
    pub zoom_scale: f32,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomConfig {
    pub enabled: bool,
    pub mode: AutoZoomMode,
    /// Target magnification when zoomed in (e.g. 2.0x to 3.5x)
    pub target_zoom: f32,
    /// Spring stiffness k for camera zoom/pan (e.g. 85.0 for smooth cinematic glide)
    pub spring_stiffness: f32,
    /// Hold time in ms after a click or dwell trigger before zooming back out
    pub hold_duration_ms: u64,
    /// Deadzone radius in normalized viewport coordinates so small cursor wiggles don't pan the camera
    pub deadzone_radius: f32,
    /// Directional motion blur strength during fast camera zoom/pan transitions
    pub motion_blur_strength: f32,
}

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: AutoZoomMode::SmartCursorFollow,
            target_zoom: 2.15,
            spring_stiffness: 95.0,
            hold_duration_ms: 2200,
            deadzone_radius: 0.045,
            motion_blur_strength: 0.35,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CameraViewport {
    /// Clamped normalized center X in [half_w, 1.0 - half_w]
    pub center_x: f32,
    /// Clamped normalized center Y in [half_h, 1.0 - half_h]
    pub center_y: f32,
    /// Current interpolated zoom factor >= 1.0
    pub zoom: f32,
    /// Normalized top-left UV and width/height of the visible sub-rectangle
    pub uv_min_x: f32,
    pub uv_min_y: f32,
    pub uv_w: f32,
    pub uv_h: f32,
    /// Instantaneous camera pan velocity for motion blur shader
    pub pan_vel_x: f32,
    pub pan_vel_y: f32,
}

impl Default for CameraViewport {
    fn default() -> Self {
        Self {
            center_x: 0.5,
            center_y: 0.5,
            zoom: 1.0,
            uv_min_x: 0.0,
            uv_min_y: 0.0,
            uv_w: 1.0,
            uv_h: 1.0,
            pan_vel_x: 0.0,
            pan_vel_y: 0.0,
        }
    }
}

pub struct ZoomEngine {
    cam_x: f32,
    cam_y: f32,
    cam_zoom: f32,
    vel_x: f32,
    vel_y: f32,
    vel_zoom: f32,
    target_x: f32,
    target_y: f32,
    target_zoom: f32,
    active_until_ms: u64,
    last_step_ms: u64,
    next_keyframe_id: u64,
    pub keyframes: Vec<ZoomKeyframe>,
}

impl ZoomEngine {
    pub fn new() -> Self {
        let mut engine = Self {
            cam_x: 0.5,
            cam_y: 0.5,
            cam_zoom: 1.0,
            vel_x: 0.0,
            vel_y: 0.0,
            vel_zoom: 0.0,
            target_x: 0.5,
            target_y: 0.5,
            target_zoom: 1.0,
            active_until_ms: 0,
            last_step_ms: 0,
            next_keyframe_id: 4,
            keyframes: Vec::new(),
        };
        engine.seed_default_keyframes();
        engine
    }

    pub fn seed_default_keyframes(&mut self) {
        self.keyframes = vec![
            ZoomKeyframe {
                id: 1,
                start_ms: 800,
                end_ms: 3200,
                target_x: 0.28,
                target_y: 0.34,
                zoom_scale: 2.1,
                label: "Code Editor Focus".into(),
            },
            ZoomKeyframe {
                id: 2,
                start_ms: 4200,
                end_ms: 6800,
                target_x: 0.72,
                target_y: 0.48,
                zoom_scale: 2.35,
                label: "Terminal & GPU Metrics".into(),
            },
            ZoomKeyframe {
                id: 3,
                start_ms: 7800,
                end_ms: 10200,
                target_x: 0.52,
                target_y: 0.76,
                zoom_scale: 1.95,
                label: "Timeline Controls".into(),
            },
        ];
    }

    pub fn add_keyframe(
        &mut self,
        start_ms: u64,
        end_ms: u64,
        target_x: f32,
        target_y: f32,
        zoom_scale: f32,
        label: impl Into<String>,
    ) -> ZoomKeyframe {
        let kf = ZoomKeyframe {
            id: self.next_keyframe_id,
            start_ms,
            end_ms: end_ms.max(start_ms + 400),
            target_x: target_x.clamp(0.05, 0.95),
            target_y: target_y.clamp(0.05, 0.95),
            zoom_scale: zoom_scale.clamp(1.1, 4.0),
            label: label.into(),
        };
        self.next_keyframe_id += 1;
        self.keyframes.push(kf.clone());
        self.keyframes.sort_by_key(|k| k.start_ms);
        kf
    }

    pub fn remove_keyframe(&mut self, id: u64) {
        self.keyframes.retain(|k| k.id != id);
    }

    pub fn trigger_manual_zoom(&mut self, x: f32, y: f32, zoom: f32, now_ms: u64, hold_ms: u64) {
        self.target_x = x.clamp(0.0, 1.0);
        self.target_y = y.clamp(0.0, 1.0);
        self.target_zoom = zoom.clamp(1.0, 4.0);
        self.active_until_ms = now_ms + hold_ms;
    }

    /// Step the spring-damper virtual camera in O(1) time.
    pub fn step(
        &mut self,
        now_ms: u64,
        cursor: CursorSample,
        clicked_now: bool,
        dwell_ms: u64,
        cfg: &ZoomConfig,
    ) -> CameraViewport {
        let dt = if self.last_step_ms == 0 {
            0.016
        } else {
            ((now_ms.saturating_sub(self.last_step_ms)) as f32 / 1000.0).clamp(0.001, 0.05)
        };
        self.last_step_ms = now_ms;

        if !cfg.enabled {
            self.target_x = 0.5;
            self.target_y = 0.5;
            self.target_zoom = 1.0;
        } else {
            match cfg.mode {
                AutoZoomMode::ClickToZoom => {
                    if clicked_now {
                        self.target_x = cursor.smooth_x;
                        self.target_y = cursor.smooth_y;
                        self.target_zoom = cfg.target_zoom.clamp(1.1, 4.0);
                        self.active_until_ms = now_ms + cfg.hold_duration_ms;
                    } else if now_ms < self.active_until_ms {
                        // Gently pan with cursor while zoomed in if cursor leaves deadzone
                        let dx = cursor.smooth_x - self.target_x;
                        let dy = cursor.smooth_y - self.target_y;
                        let dist = (dx * dx + dy * dy).sqrt();
                        if dist > cfg.deadzone_radius {
                            let pull = (dist - cfg.deadzone_radius) / dist;
                            self.target_x += dx * pull * 0.35;
                            self.target_y += dy * pull * 0.35;
                        }
                    } else {
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                        self.target_zoom = 1.0;
                    }
                }
                AutoZoomMode::SmartCursorFollow => {
                    // Screen Studio automatic cursor follow:
                    // 1. Fast cross-screen travel (> 0.95 screen-widths/sec) pulls back toward wide view
                    // 2. Settling/working in a region (dwell > 220ms) or clicking/interacting zooms in smoothly
                    if clicked_now || (dwell_ms > 220 && cursor.speed < 0.45) {
                        self.target_zoom = cfg.target_zoom.clamp(1.1, 4.0);
                        self.active_until_ms = now_ms + cfg.hold_duration_ms;
                    } else if cursor.speed > 0.95 {
                        self.target_zoom = 1.0;
                    } else if now_ms > self.active_until_ms && dwell_ms > 4500 {
                        // Gently return to 1.0x wide view if completely untouched for 4.5s
                        self.target_zoom = 1.0;
                    }

                    if self.target_zoom > 1.05 {
                        let dx = cursor.smooth_x - self.target_x;
                        let dy = cursor.smooth_y - self.target_y;
                        if (dx * dx + dy * dy).sqrt() > cfg.deadzone_radius * 0.4 {
                            self.target_x = cursor.smooth_x;
                            self.target_y = cursor.smooth_y;
                        }
                    } else {
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                    }
                }
                AutoZoomMode::TimelineKeyframes => {
                    // Loop timeline over 12-second cycle in live preview or absolute recording clock
                    let cycle_ms = now_ms % 12000;
                    if let Some(kf) = self
                        .keyframes
                        .iter()
                        .find(|k| cycle_ms >= k.start_ms && cycle_ms <= k.end_ms)
                    {
                        self.target_x = kf.target_x;
                        self.target_y = kf.target_y;
                        self.target_zoom = kf.zoom_scale;
                    } else {
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                        self.target_zoom = 1.0;
                    }
                }
                AutoZoomMode::Manual => {
                    if now_ms > self.active_until_ms {
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                        self.target_zoom = 1.0;
                    }
                }
            }
        }

        // Critically-damped spring integration for (cam_x, cam_y, cam_zoom)
        let k = cfg.spring_stiffness.clamp(20.0, 300.0);
        let c = 2.0 * k.sqrt();

        let ax = -k * (self.cam_x - self.target_x) - c * self.vel_x;
        let ay = -k * (self.cam_y - self.target_y) - c * self.vel_y;
        let az = -k * (self.cam_zoom - self.target_zoom) - c * self.vel_zoom;

        self.vel_x += ax * dt;
        self.vel_y += ay * dt;
        self.vel_zoom += az * dt;

        self.cam_x += self.vel_x * dt;
        self.cam_y += self.vel_y * dt;
        self.cam_zoom = (self.cam_zoom + self.vel_zoom * dt).clamp(1.0, 4.0);

        // Edge-clamp camera center so the zoomed viewport never samples outside [0, 1]
        let uv_w = (1.0 / self.cam_zoom).clamp(0.25, 1.0);
        let uv_h = (1.0 / self.cam_zoom).clamp(0.25, 1.0);
        let half_w = uv_w * 0.5;
        let half_h = uv_h * 0.5;

        let clamped_x = self.cam_x.clamp(half_w, 1.0 - half_w);
        let clamped_y = self.cam_y.clamp(half_h, 1.0 - half_h);

        CameraViewport {
            center_x: clamped_x,
            center_y: clamped_y,
            zoom: self.cam_zoom,
            uv_min_x: (clamped_x - half_w).clamp(0.0, 1.0 - uv_w),
            uv_min_y: (clamped_y - half_h).clamp(0.0, 1.0 - uv_h),
            uv_w,
            uv_h,
            pan_vel_x: self.vel_x * cfg.motion_blur_strength,
            pan_vel_y: self.vel_y * cfg.motion_blur_strength,
        }
    }
}
