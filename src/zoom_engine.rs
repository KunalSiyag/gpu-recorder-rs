use crate::cursor::CursorSample;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoZoomMode {
    /// Zooms into cursor on click, holds briefly (`hold_duration_ms`), then smoothly returns to 1.0x
    ClickToZoom,
    /// Screen Studio style: zooms in on clicks or glide-and-settle focus events, holds briefly (~1.6s),
    /// and automatically returns to 1.0x wide view when idle or moving across the screen.
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
            AutoZoomMode::SmartCursorFollow => "Auto + Combo (Hybrid)",
            AutoZoomMode::TimelineKeyframes => "Timeline Keyframes (Studio)",
            AutoZoomMode::Manual => "Combo Only (Manual Liberty)",
        }
    }
}

/// User-selectable or self-registered combination (mouse operation, modifier+click, gesture, or key)
/// that triggers or toggles zoom on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZoomComboBinding {
    /// Default: Quick Cursor Shake (⇄ wiggle) OR Middle-Click (Mouse 2) — works across all Wayland & X11 windows!
    ShakeOrMiddleClick,
    /// Middle Mouse Button (Wheel Click / Mouse 2)
    MiddleClick,
    /// Ctrl + Click (Left or Right Click while holding Ctrl)
    CtrlClick,
    /// Right Mouse Button (Mouse 3)
    RightClick,
    /// Double Left-Click (< 320ms)
    DoubleClick,
    /// Side Mouse Button (Mouse 4 / Mouse 5)
    SideButton,
    /// Quick Cursor Shake (wiggle mouse left-right twice anywhere on screen)
    CursorShake,
    /// Custom user-registered combo recorded via "🎯 Record Combo…" in the UI
    Custom {
        ctrl: bool,
        alt: bool,
        shift: bool,
        super_key: bool,
        /// 0 = none, 1 = left, 2 = middle, 3 = right, 4 = side
        mouse_button: u8,
        /// X11 keycode (0 = none)
        keycode: u32,
        shake: bool,
        label: String,
    },
}

impl Default for ZoomComboBinding {
    fn default() -> Self {
        ZoomComboBinding::ShakeOrMiddleClick
    }
}

impl ZoomComboBinding {
    pub fn short_label(&self) -> String {
        match self {
            ZoomComboBinding::ShakeOrMiddleClick => "⇄ Shake / Mid-Click".to_string(),
            ZoomComboBinding::MiddleClick => "🖱️ Middle Click".to_string(),
            ZoomComboBinding::CtrlClick => "⌃ Ctrl + Click".to_string(),
            ZoomComboBinding::RightClick => "🖱️ Right Click".to_string(),
            ZoomComboBinding::DoubleClick => "🖱️ Double Click".to_string(),
            ZoomComboBinding::SideButton => "🖱️ Side Button".to_string(),
            ZoomComboBinding::CursorShake => "⇄ Shake Cursor".to_string(),
            ZoomComboBinding::Custom { label, .. } => format!("🎯 {label}"),
        }
    }

    pub fn is_active(&self, c: &CursorSample) -> bool {
        match self {
            ZoomComboBinding::ShakeOrMiddleClick => c.shake_gesture || c.middle_down,
            ZoomComboBinding::MiddleClick => c.middle_down,
            ZoomComboBinding::CtrlClick => c.ctrl_down && (c.left_down || c.right_down),
            ZoomComboBinding::RightClick => c.right_down,
            ZoomComboBinding::DoubleClick => c.double_clicked,
            ZoomComboBinding::SideButton => c.side_down,
            ZoomComboBinding::CursorShake => c.shake_gesture,
            ZoomComboBinding::Custom {
                ctrl,
                alt,
                shift,
                super_key,
                mouse_button,
                keycode,
                shake,
                ..
            } => {
                if *shake {
                    return c.shake_gesture;
                }
                let mods_ok = (!*ctrl || c.ctrl_down)
                    && (!*alt || c.alt_down)
                    && (!*shift || c.shift_down)
                    && (!*super_key || c.super_down);
                if !mods_ok {
                    return false;
                }
                let btn_ok = match *mouse_button {
                    0 => true,
                    1 => c.left_down,
                    2 => c.middle_down,
                    3 => c.right_down,
                    4 => c.side_down,
                    _ => false,
                };
                let key_ok = *keycode == 0 || c.active_keycode == *keycode;
                let has_any = *ctrl
                    || *alt
                    || *shift
                    || *super_key
                    || *mouse_button != 0
                    || *keycode != 0;
                has_any && btn_ok && key_ok
            }
        }
    }
}

/// How the zoom combo behaves when triggered by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComboActionMode {
    /// Trigger once to lock zoom IN (follows cursor smoothly as long as you want); trigger again to return to 1.0x
    ToggleLock,
    /// Stay zoomed in while holding the combo; smoothly return to 1.0x on release
    HoldToZoom,
    /// Step through zoom depths on each trigger: 1.0x -> 1.8x -> 2.5x -> 1.0x
    StepCycle,
}

impl Default for ComboActionMode {
    fn default() -> Self {
        ComboActionMode::ToggleLock
    }
}

impl ComboActionMode {
    pub fn label(&self) -> &'static str {
        match self {
            ComboActionMode::ToggleLock => "⇄ Toggle Lock",
            ComboActionMode::HoldToZoom => "✋ Hold to Zoom",
            ComboActionMode::StepCycle => "🪜 Step 1.8×/2.5×",
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
    /// Target magnification when zoomed in (e.g. 2.0x Studio default)
    pub target_zoom: f32,
    /// Spring stiffness k for camera zoom/pan (e.g. 55.0 for smooth Screen Studio glide)
    pub spring_stiffness: f32,
    /// Hold time in ms after a click or settle trigger before zooming back out to 1.0x
    pub hold_duration_ms: u64,
    /// Deadzone radius in normalized viewport coordinates so small cursor wiggles don't pan the camera
    pub deadzone_radius: f32,
    /// Directional motion blur strength during fast camera zoom/pan transitions
    pub motion_blur_strength: f32,
    /// User-configurable mouse/key/gesture combination for manual zoom control
    #[serde(default)]
    pub combo_binding: ZoomComboBinding,
    /// Whether the combo toggles zoom lock, holds while pressed, or steps through zoom levels
    #[serde(default)]
    pub combo_action: ComboActionMode,
}

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: AutoZoomMode::SmartCursorFollow,
            target_zoom: 2.00,
            spring_stiffness: 60.0,
            hold_duration_ms: 1650,
            deadzone_radius: 0.065,
            motion_blur_strength: 0.35,
            combo_binding: ZoomComboBinding::ShakeOrMiddleClick,
            combo_action: ComboActionMode::ToggleLock,
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
    cooldown_until_ms: u64,
    last_trigger_x: f32,
    last_trigger_y: f32,
    armed_after_travel: bool,
    last_step_ms: u64,
    next_keyframe_id: u64,
    pub keyframes: Vec<ZoomKeyframe>,
    /// True when the user has manually engaged zoom via their combo, scroll wheel, or lock toggle.
    /// While true, the camera stays zoomed in and follows the cursor indefinitely until unlocked!
    pub manual_locked: bool,
    pub manual_zoom_level: f32,
    prev_combo_active: bool,
    hold_impulse_until_ms: u64,
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
            cooldown_until_ms: 0,
            last_trigger_x: 0.5,
            last_trigger_y: 0.5,
            armed_after_travel: true,
            last_step_ms: 0,
            next_keyframe_id: 4,
            keyframes: Vec::new(),
            manual_locked: false,
            manual_zoom_level: 2.0,
            prev_combo_active: false,
            hold_impulse_until_ms: 0,
        };
        engine.seed_default_keyframes();
        engine
    }

    pub fn seed_default_keyframes(&mut self) {
        self.keyframes = vec![
            ZoomKeyframe {
                id: 1,
                start_ms: 800,
                end_ms: 3000,
                target_x: 0.28,
                target_y: 0.34,
                zoom_scale: 2.0,
                label: "Code Editor Focus".into(),
            },
            ZoomKeyframe {
                id: 2,
                start_ms: 4400,
                end_ms: 6600,
                target_x: 0.72,
                target_y: 0.48,
                zoom_scale: 2.15,
                label: "Terminal & GPU Metrics".into(),
            },
            ZoomKeyframe {
                id: 3,
                start_ms: 8000,
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
        self.last_trigger_x = self.target_x;
        self.last_trigger_y = self.target_y;
        if zoom <= 1.01 {
            self.manual_locked = false;
        }
    }

    /// Directly toggle manual zoom lock IN / OUT at the current cursor position.
    pub fn toggle_manual_lock(&mut self, x: f32, y: f32, default_zoom: f32) {
        self.manual_locked = !self.manual_locked;
        if self.manual_locked {
            self.manual_zoom_level = default_zoom.clamp(1.25, 3.5);
            self.target_x = x.clamp(0.0, 1.0);
            self.target_y = y.clamp(0.0, 1.0);
            self.target_zoom = self.manual_zoom_level;
        } else {
            self.target_x = 0.5;
            self.target_y = 0.5;
            self.target_zoom = 1.0;
            self.active_until_ms = 0;
        }
    }

    /// Adjust zoom depth via mouse scroll wheel (`delta_zoom > 0` zooms in, `< 0` zooms out).
    pub fn adjust_manual_zoom_wheel(&mut self, x: f32, y: f32, delta_zoom: f32) {
        let base = if self.manual_locked {
            self.manual_zoom_level
        } else {
            self.cam_zoom.max(1.0)
        };
        let next = (base + delta_zoom).clamp(1.0, 3.2);
        if next <= 1.05 {
            self.manual_locked = false;
            self.manual_zoom_level = 1.0;
            self.target_zoom = 1.0;
            self.target_x = 0.5;
            self.target_y = 0.5;
            self.active_until_ms = 0;
        } else {
            self.manual_locked = true;
            self.manual_zoom_level = next;
            self.target_zoom = next;
            self.target_x = x.clamp(0.0, 1.0);
            self.target_y = y.clamp(0.0, 1.0);
        }
    }

    pub fn release_manual_lock(&mut self) {
        self.manual_locked = false;
        self.target_zoom = 1.0;
        self.target_x = 0.5;
        self.target_y = 0.5;
        self.active_until_ms = 0;
    }

    /// Step the spring-damper virtual camera in O(1) time.
    /// Supports both user-triggered Manual Combo Zoom (with full duration & depth liberty)
    /// and Screen Studio's bounded-segment Auto-Zoom.
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

        // 1. Evaluate user's Manual Zoom Combo (Mouse button / Modifier+Click / Shake Gesture / Custom Key)
        let combo_now = cfg.combo_binding.is_active(&cursor);
        let combo_rising = combo_now && !self.prev_combo_active;
        self.prev_combo_active = combo_now;

        match cfg.combo_action {
            ComboActionMode::ToggleLock => {
                if combo_rising {
                    self.manual_locked = !self.manual_locked;
                    if self.manual_locked {
                        self.manual_zoom_level = cfg.target_zoom.clamp(1.25, 3.5);
                        self.target_x = cursor.smooth_x;
                        self.target_y = cursor.smooth_y;
                        self.target_zoom = self.manual_zoom_level;
                    } else {
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                        self.target_zoom = 1.0;
                        self.active_until_ms = 0;
                    }
                }
            }
            ComboActionMode::HoldToZoom => {
                if combo_now {
                    // If triggered by an impulse gesture (shake or double-click), hold for 2.2s
                    if cursor.shake_gesture || cursor.double_clicked {
                        self.hold_impulse_until_ms = now_ms + 2200;
                    }
                    self.manual_locked = true;
                    self.manual_zoom_level = cfg.target_zoom.clamp(1.25, 3.5);
                    if combo_rising {
                        self.target_x = cursor.smooth_x;
                        self.target_y = cursor.smooth_y;
                    }
                } else if now_ms >= self.hold_impulse_until_ms && self.manual_locked {
                    self.manual_locked = false;
                    self.target_x = 0.5;
                    self.target_y = 0.5;
                    self.target_zoom = 1.0;
                }
            }
            ComboActionMode::StepCycle => {
                if combo_rising {
                    if !self.manual_locked {
                        self.manual_locked = true;
                        self.manual_zoom_level = 1.80;
                        self.target_x = cursor.smooth_x;
                        self.target_y = cursor.smooth_y;
                    } else if self.manual_zoom_level < 2.2 {
                        self.manual_zoom_level = 2.50;
                        self.target_x = cursor.smooth_x;
                        self.target_y = cursor.smooth_y;
                    } else {
                        self.manual_locked = false;
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                        self.target_zoom = 1.0;
                        self.active_until_ms = 0;
                    }
                }
            }
        }

        // Track if cursor has traveled far enough from the last trigger point to arm a new settle-zoom
        let dist_from_trigger = ((cursor.smooth_x - self.last_trigger_x).powi(2)
            + (cursor.smooth_y - self.last_trigger_y).powi(2))
        .sqrt();
        if dist_from_trigger > 0.14 || cursor.speed > 0.55 {
            self.armed_after_travel = true;
        }

        if self.manual_locked {
            // Manual Combo Lock is active: user has 100% liberty to stay zoomed in for as long as they want!
            self.target_zoom = self.manual_zoom_level.clamp(1.2, 3.5);
            let dx = cursor.smooth_x - self.target_x;
            let dy = cursor.smooth_y - self.target_y;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > cfg.deadzone_radius * 0.75 {
                let pull = (dist - cfg.deadzone_radius * 0.75) / dist;
                self.target_x += dx * pull * 0.35;
                self.target_y += dy * pull * 0.35;
            }
        } else if !cfg.enabled {
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
                        self.last_trigger_x = cursor.smooth_x;
                        self.last_trigger_y = cursor.smooth_y;
                    } else if now_ms < self.active_until_ms {
                        // Deadzone-filtered pan while zoomed in
                        let dx = cursor.smooth_x - self.target_x;
                        let dy = cursor.smooth_y - self.target_y;
                        let dist = (dx * dx + dy * dy).sqrt();
                        if dist > cfg.deadzone_radius {
                            let pull = (dist - cfg.deadzone_radius) / dist;
                            self.target_x += dx * pull * 0.25;
                            self.target_y += dy * pull * 0.25;
                        }
                    } else {
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                        self.target_zoom = 1.0;
                    }
                }
                AutoZoomMode::SmartCursorFollow => {
                    // Edge-triggered settle detection: triggers ONCE when the cursor arrives at a new UI region
                    // and settles for 240..420ms. Sitting idle afterwards does NOT keep extending the zoom!
                    let fresh_settle = self.armed_after_travel
                        && (240..=420).contains(&dwell_ms)
                        && cursor.speed < 0.28
                        && now_ms >= self.cooldown_until_ms;

                    if clicked_now || fresh_settle {
                        self.armed_after_travel = false;
                        self.last_trigger_x = cursor.smooth_x;
                        self.last_trigger_y = cursor.smooth_y;
                        self.target_x = cursor.smooth_x;
                        self.target_y = cursor.smooth_y;
                        self.target_zoom = cfg.target_zoom.clamp(1.1, 4.0);
                        self.active_until_ms = now_ms + cfg.hold_duration_ms;
                    } else if cursor.speed > 0.80 {
                        // Fast cross-screen travel immediately zooms out to 1.0x wide view
                        self.target_zoom = 1.0;
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                        self.active_until_ms = 0;
                        self.cooldown_until_ms = now_ms + 450;
                    } else if now_ms < self.active_until_ms {
                        // If user is making small precision movements near the focus point, allow a short extension,
                        // otherwise let the hold timer expire cleanly back to 1.0x!
                        let dx = cursor.smooth_x - self.target_x;
                        let dy = cursor.smooth_y - self.target_y;
                        let dist = (dx * dx + dy * dy).sqrt();
                        if dist > cfg.deadzone_radius {
                            let pull = (dist - cfg.deadzone_radius) / dist;
                            self.target_x += dx * pull * 0.28;
                            self.target_y += dy * pull * 0.28;
                        }
                    } else {
                        // Hold timer expired -> smoothly return to 1.0x wide view!
                        if self.target_zoom > 1.01 {
                            self.cooldown_until_ms = now_ms + 750;
                        }
                        self.target_zoom = 1.0;
                        self.target_x = 0.5;
                        self.target_y = 0.5;
                    }
                }
                AutoZoomMode::TimelineKeyframes => {
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
        // Separate slightly softer stiffness for pan so camera glides like a physical fluid head
        let k_zoom = cfg.spring_stiffness.clamp(20.0, 260.0);
        let c_zoom = 2.0 * k_zoom.sqrt();
        let k_pan = (k_zoom * 0.78).clamp(18.0, 220.0);
        let c_pan = 2.0 * k_pan.sqrt();

        let ax = -k_pan * (self.cam_x - self.target_x) - c_pan * self.vel_x;
        let ay = -k_pan * (self.cam_y - self.target_y) - c_pan * self.vel_y;
        let az = -k_zoom * (self.cam_zoom - self.target_zoom) - c_zoom * self.vel_zoom;

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
