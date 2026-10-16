---
title: "How We Built 'Screen Studio for Linux Wayland' in 56MB of Native Rust (Polling DRM Cursor Plane #92 Without Root)"
published: false
description: "Reverse-engineering Linux GPU screen recording, solving rootless Wayland global cursor tracking via libdrm Plane #92, and building a 240Hz spring-physics auto-zoom camera + Wayland Layer-Shell Dynamic Island UI."
tags: rust, linux, opensource, gamedev
cover_image: https://kunalsiyag.github.io/gpu-recorder-rs/og-banner.png
canonical_url: https://kunalsiyag.github.io/gpu-recorder-rs/
---

If you have ever watched a product demo recorded on macOS with **Screen Studio**, you immediately notice two things:
1. The camera **smoothly zooms in on the cursor** whenever the presenter clicks or focuses on a UI element, then glides back out to `1.0×` wide view when traveling across the screen.
2. There is no clunky recording window cluttering the desktop.

On **Linux Wayland** (`COSMIC`, `Hyprland`, `GNOME`, `KDE Plasma`, `Sway`), we have awesome raw GPU encoders like `gpu-screen-recorder` and `OBS`, but **zero native Screen Studio alternatives** with automatic cursor zoom and a non-intrusive Dynamic Island HUD.

So we built **[HyperGSR (`gpu-recorder-rs`)](https://github.com/KunalSiyag/gpu-recorder-rs)** in native Rust + C — running in **~56 MB of RAM** and **<1.5% CPU** at 1080p60.

{% github KunalSiyag/gpu-recorder-rs %}

---

## 🕵️ Step 1: Reverse-Engineering Why Wayland Cursor Auto-Zoom Is So Hard

Before writing a line of Rust, we disassembled `com.dec05eba.gpu_screen_recorder` (`v6.1.4`) to inspect how existing Linux GPU recorders capture frames and poll the cursor (full disassembly notes are in [`RE_ANALYSIS.md`](https://github.com/KunalSiyag/gpu-recorder-rs/blob/main/RE_ANALYSIS.md)).

We ran into **two classic Linux Wayland walls**:

### Wall #1: Wayland Hides Global Cursor Coordinates by Design
In `gpu-screen-recorder` (`gsr_cursor` at `0xd055c`), cursor tracking relies on X11 `XFixesGetCursorImage` and `XQueryPointer`.
On a modern Wayland session:
- Wayland security intentionally isolates windows so a client app cannot query global `(x, y)` pointer coordinates outside its own surface.
- Even worse, calling synchronous `XQueryPointer` against rootless `Xwayland` (`:1`) while a native Wayland window is focused **blocks the main thread in `poll_schedule_timeout`**, freezing your GTK4 UI!

### Wall #2: Reading `/dev/input/event*` (`evdev`) Requires `root` or `input` Group
Many scripts work around Wayland by reading raw mouse deltas from `/dev/input/event*`. Not only does that require `sudo` (or adding the user to the privileged `input` group), raw mouse deltas drift out of sync with the compositor's pointer acceleration curve!

---

## 💡 The Breakthrough: Polling DRM Hardware Cursor Plane `#92` Without Root

Every modern Linux desktop compositor (`cosmic-comp`, `Hyprland`, `mutter`, `kwin_wayland`, `sway`) composites the hardware mouse cursor by updating the GPU's **DRM Universal Cursor Plane** on `/dev/dri/card*`.

And here is the magic: any logged-in desktop user already has read/ioctl access to `/dev/dri/card*` via `seatd` / `systemd-logind` (`ACL` permissions on `/dev/dri/card1`) **with zero root privileges**!

In [`src/cursor.rs`](https://github.com/KunalSiyag/gpu-recorder-rs/blob/main/src/cursor.rs), `DrmCursorReader` dynamically loads `libdrm.so.2` via FFI, enables Universal & Atomic plane capabilities, and inspects the hardware cursor plane (`DRM_PLANE_TYPE_CURSOR = 2`, Plane `#92` on our AMD + NVIDIA hybrid system):

```rust
// Enable universal planes (2) + atomic properties (3) on /dev/dri/card*
(drm_set_client_cap)(fd, 2, 1); // DRM_CLIENT_CAP_UNIVERSAL_PLANES
(drm_set_client_cap)(fd, 3, 1); // DRM_CLIENT_CAP_ATOMIC

// Poll CRTC_X, CRTC_Y, and FB_ID on the Hardware Cursor Plane at 60Hz:
let props = (self.get_props)(self.fd, self.plane_id, DRM_MODE_OBJECT_PLANE);
```

This gives HyperGSR **three superpowers on any Wayland compositor**:
1. **Exact Global `(CRTC_X, CRTC_Y)` Coordinates**: Matches the compositor's exact pixel position across every native Wayland and X11 window.
2. **Cursor Shape Change Detection (`FB_ID`)**: Whenever you hover over a clickable button, link, or text input, the compositor swaps the cursor framebuffer (`FB_ID`). We detect that change in real time!
3. **Zero XWayland Deadlocks**: Because we never touch `XQueryPointer` when `DrmCursorReader` is active, our GTK4 Layer-Shell UI ticks at a rock-solid 60 Hz (`~56 MB` RSS).

---

## 🎥 Step 2: 240Hz Critically-Damped Spring Camera & Bounded Trajectory Planner

Early prototypes of auto-zoom recorders suffer from two annoying bugs:
1. **Motion Sickness / Wobble**: Every tiny mouse wiggle pans the camera.
2. **Stuck Zoom**: The camera zooms in to `2.0×` and stays zoomed in forever.

In [`src/zoom_engine.rs`](https://github.com/KunalSiyag/gpu-recorder-rs/blob/main/src/zoom_engine.rs), we implemented a two-layer camera system:

### 1. Bounded Screen Studio Trajectory Planner (`build_camera_trajectory`)
Instead of staying zoomed in continuously, the planner detects discrete focus moments (clicks, custom gesture combos, or stationary dwell clusters where velocity drops below `0.14 uv/s` after traveling across the screen), holds the zoom for `1.65s`, and **automatically pulls back to `1.0×` wide view** whenever the cursor makes a fast cross-screen move (`speed > 1.15 uv/s`).

### 2. 240Hz Sub-Stepped Critically-Damped Spring ODE ($\zeta = 1.0$)
To guarantee zero overshoot oscillation and $C^2$ continuous acceleration, we integrate the 2D camera center and zoom factor at 240 Hz (4 sub-steps per 60 FPS frame):

$$\ddot{\mathbf{x}} = -k(\mathbf{x} - \mathbf{x}_{\text{target}}) - 2\sqrt{k}\,\dot{\mathbf{x}}$$

```rust
let damp = 2.0 * k.sqrt(); // Exact critical damping (zeta = 1.0)
let sub_steps = 4;
let sub_dt = dt / (sub_steps as f32);

for _ in 0..sub_steps {
    let ax = -k * (self.cam_x - self.target_x) - damp * self.vel_x;
    let ay = -k * (self.cam_y - self.target_y) - damp * self.vel_y;
    let az = -k * (self.cam_zoom - self.target_zoom) - damp * self.vel_z;
    self.vel_x += ax * sub_dt;
    self.vel_y += ay * sub_dt;
    self.vel_z += az * sub_dt;
    self.cam_x += self.vel_x * sub_dt;
    self.cam_y += self.vel_y * sub_dt;
    self.cam_zoom = (self.cam_zoom + self.vel_z * sub_dt).clamp(1.0, 4.0);
}
```

---

## 🎮 Step 3: Self-Registering Custom Zoom Combos (`🎯 Record Combo…`)

Sometimes you want automatic zoom; other times you want 100% manual control during a live coding talk.

We added a **Self-Registering Gesture & Key Combo Recorder** in the Dynamic Island Studio drawer:
- Click **`🎯 Record Combo…`** and perform any action within 5 seconds:
  - **Shake the cursor** rapidly (`Shake / MidClick`)
  - **Double-Click**, **Middle-Click**, **Right-Click**, or **Mouse Side Buttons (`Mouse4` / `Mouse5`)**
  - **`Ctrl + Alt`**, **`Ctrl + Shift`**, **`Super + Z`**
  - **Scroll the Mouse Wheel** (`1.0×` – `3.2×` live magnification depth)
- Choose between **`Toggle Lock`**, **`Hold Combo`**, or **`Cycle 1×/2×/2.8×`**.

---

## 🏝️ Step 4: Native Wayland Layer-Shell Dynamic Island (`zwlr_layer_shell_v1`)

Instead of a floating X11/GTK window that gets captured in your own screen recording or tiled awkwardly by Hyprland/COSMIC, HyperGSR preloads `libgtk4-layer-shell.so.0` and anchors a 32-bit RGBA transparent capsule to the top-center of your monitor (`GTK_LAYER_SHELL_LAYER_TOP`).

It features:
- Live **16:9 Virtual Camera Radar** + **Spring S-Curve Telemetry**
- One-click **Obsidian Dark (`🌙`)** and **Pearl Frosted Light (`☀️`)** themes
- Standalone **PipeWire + XDG Desktop Portal + NVENC/VAAPI** recording (`src/pw_portal_capture.c`) with persistent `restore_token` reuse so you aren't spammed by permission dialogs on every recording.

---

## 🚀 Try It in 15 Seconds & Join the Project!

```bash
git clone https://github.com/KunalSiyag/gpu-recorder-rs.git
cd gpu-recorder-rs
./install.sh
```

We have open **`good first issue`** tickets for adding **Catppuccin / TokyoNight / Nord themes**, **on-screen keystroke HUD pills**, and **macOS-style wallpaper frames**:

👉 **[⭐ Star & 🍴 Fork `KunalSiyag/gpu-recorder-rs` on GitHub](https://github.com/KunalSiyag/gpu-recorder-rs)**!
