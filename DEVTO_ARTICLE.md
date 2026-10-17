---
title: "Cursor-following auto-zoom on Linux Wayland: reading the cursor from the GPU in Rust"
published: false
description: "Wayland won't tell apps where the cursor is. Here's how HyperGSR reads it from the DRM hardware cursor plane without root, and drives a critically damped spring camera for Screen Studio-style auto-zoom."
tags: rust, linux, opensource, showdev
cover_image: https://kunalsiyag.github.io/gpu-recorder-rs/screenshots/devto-cover.png
---

Screen Studio on macOS made one idea popular: while you record, the video quietly zooms in on whatever you're working on, then glides back out when you move away. On Linux Wayland, that one feature is surprisingly hard. The reason has nothing to do with video. **Wayland won't tell an app where the cursor is.**

This post covers how I got around that in [HyperGSR](https://github.com/KunalSiyag/gpu-recorder-rs), an MIT-licensed screen recorder written in Rust. It covers the trick (read the cursor from the GPU's hardware cursor plane), the camera (a critically damped spring), and what it costs (about 56 MB of RAM at 1080p60).

You can try the camera in your browser before reading any code: the [HyperGSR landing page](https://kunalsiyag.github.io/gpu-recorder-rs/#workbench) runs the same spring model.

![HyperGSR landing page in dark theme: "The camera follows your cursor. On Wayland, in 56 MB." with an install command and a spring camera diagram](https://kunalsiyag.github.io/gpu-recorder-rs/screenshots/landing-hero-dark.png)

{% github KunalSiyag/gpu-recorder-rs %}

## The problem: there is no global cursor on Wayland

On X11 you call `XQueryPointer` and get global coordinates. Wayland deliberately doesn't offer that. A client only learns where the pointer is while the pointer is over the client's own surface. That's a good security property, and it's the whole reason auto-zoom recorders are rare on Linux.

The usual workarounds each break somewhere:

| Approach | Why it falls short |
|---|---|
| `XQueryPointer` through XWayland | Only sees XWayland windows. Worse, a synchronous X11 request can block while a native Wayland window has focus, which froze my GTK UI. |
| Read `/dev/input/event*` | Needs root or the `input` group, and you get raw deltas rather than the position the compositor drew, so pointer acceleration makes it drift. |
| Compositor-specific IPC | Works on one compositor (Hyprland, Sway), not the rest. |

I started by reverse-engineering `gpu-screen-recorder` 6.1.4 to see how an existing GPU recorder handles this. It uses X11 for the cursor too. Along the way I found a hardcoded `/var/lib/flatpak` path that breaks user-level Flatpak installs. The notes are in [RE_ANALYSIS.md](https://github.com/KunalSiyag/gpu-recorder-rs/blob/main/RE_ANALYSIS.md).

## The trick: ask the display hardware

Every modern GPU has a small **hardware cursor plane**: a dedicated overlay the display controller composites on top of the frame. The compositor has to tell the kernel where to draw it, and those values are exposed as DRM plane properties: `CRTC_X`, `CRTC_Y`, and `FB_ID` (the cursor image).

Reading those properties doesn't require becoming DRM master, and it doesn't change any modes. The logged-in desktop user can already open `/dev/dri/card*`. So HyperGSR loads `libdrm`, turns on universal planes and atomic properties, finds the cursor plane (plane `#92` on my machine), and reads it:

```rust
// src/cursor.rs — DrmCursorReader::query_cursor()
for j in 0..pcount {
    let prop_id = *(*props).props.add(j);
    let pval = *(*props).prop_values.add(j);
    let pr = (self.get_prop)(self.fd, prop_id);
    if !pr.is_null() {
        let name_bytes =
            std::slice::from_raw_parts((*pr).name.as_ptr() as *const u8, 32);
        if name_bytes.starts_with(b"FB_ID\0") {
            fb_id = pval;
        } else if name_bytes.starts_with(b"CRTC_X\0") {
            crtc_x = Some((pval as u32) as i32);
        } else if name_bytes.starts_with(b"CRTC_Y\0") {
            crtc_y = Some((pval as u32) as i32);
        }
        (self.free_prop)(pr);
    }
}
```

A non-zero `FB_ID` means the cursor is visible, and a *change* in `FB_ID` means the cursor shape changed (arrow to text beam, for example). That's a free signal that you've moved onto something interactive. The GUI polls this every 16 ms, which works out to about 60 Hz.

Because XWayland can block, the tracker never even opens an X11 connection when the DRM plane is available:

```rust
// src/cursor.rs — CursorTracker::new()
let drm_cursor = DrmCursorReader::try_open();
let x11_conn = if drm_cursor.is_some() {
    None
} else {
    X11CursorConn::try_open()
};
```

**Caveats, honestly:**

- If your compositor draws the cursor in software (some VMs, some drivers), there is no plane to read. HyperGSR then falls back to `XQueryPointer`. That's exact on Xorg, but on Wayland it only follows the cursor over XWayland windows.
- I develop and test on COSMIC with a hybrid AMD + NVIDIA laptop. GNOME, KDE Plasma, Hyprland and Sway should behave the same, since this is kernel-level, but I haven't verified them. Reports are welcome.

## The camera: a spring with exactly enough damping

Knowing where the cursor is gets you halfway. The other half is moving the virtual camera so it feels filmed rather than scripted. Linear interpolation looks robotic, and an underdamped spring wobbles. The sweet spot is a **critically damped spring**, where the damping coefficient is `2√k`. It reaches the target as fast as possible without overshooting.

The camera has three springs: pan X, pan Y, and zoom. Pan runs at 78% of the zoom stiffness, so the frame drifts slightly behind the zoom like a real fluid head:

```rust
// src/zoom_engine.rs — VirtualCamera::step()
let k_zoom = cfg.spring_stiffness.clamp(20.0, 260.0);
let c_zoom = 2.0 * k_zoom.sqrt();
let k_pan = (k_zoom * 0.78).clamp(18.0, 220.0);
let c_pan = 2.0 * k_pan.sqrt();

let ax = -k_pan * (self.cam_x - self.target_x) - c_pan * self.vel_x;
let ay = -k_pan * (self.cam_y - self.target_y) - c_pan * self.vel_y;
let az = -k_zoom * (self.cam_zoom - self.target_zoom) - c_zoom * self.vel_zoom;

self.vel_x += ax * dt;
self.cam_x += self.vel_x * dt;
// ...same for y and zoom, then clamp the centre so the crop
// never samples outside the [0, 1] frame
```

If you want the math behind this, Ryan Juckett's [Damped Springs](https://www.ryanjuckett.com/damped-springs/) and Daniel Holden's [Spring-It-On](https://theorangeduck.com/page/spring-roll-call) are the two write-ups I keep going back to.

### Deciding *when* to zoom

The spring decides how the camera moves. A separate rule decides where it's going. Zooming whenever the cursor stops sounds right, but it feels terrible: you pause to think and the video lurches in. What worked:

1. **Edge-triggered settle.** Zoom in once, when the cursor has been still (speed under 0.28 screen widths per second) for between 240 and 420 ms. Sitting still longer does not keep re-triggering.
2. **Re-arm only after travel.** Another settle zoom is only allowed after the cursor has moved more than 14% of the screen, or moved fast.
3. **Hold, then release.** Stay zoomed for 1650 ms, following the cursor with a small deadzone so tiny movements don't pan, then spring back to 1.0×.

```rust
// src/zoom_engine.rs
let fresh_settle = self.armed_after_travel
    && (240..=420).contains(&dwell_ms)
    && cursor.speed < 0.28
    && now_ms >= self.cooldown_until_ms;
```

Here's what it looks like in a recording:

![Auto-zoom demo: the recorded frame zooms toward the cursor when it settles and pulls back when it moves away](https://kunalsiyag.github.io/gpu-recorder-rs/demo-autozoom.gif)

And here it is running in the browser workbench, caught mid-zoom at 1.98× (left: full frame with the crop box, right: what ends up in the video):

![Spring camera workbench in dark theme, zooming in at 1.98x, with live cursor, camera, zoom, velocity and phase telemetry](https://kunalsiyag.github.io/gpu-recorder-rs/screenshots/workbench-autozoom-dark.png)

You can also zoom by hand. In the studio, **Record Combo…** captures any mouse button, modifier+click, cursor shake, or key, and binds it to toggle lock, hold to zoom, or a 1.8× → 2.5× step. Here the camera is locked at 2.00× on an Export button:

![Spring camera workbench in light theme, locked at 2.00x on the Export MP4 button](https://kunalsiyag.github.io/gpu-recorder-rs/screenshots/workbench-locked-light.png)

## Capture: the portal, a small C helper, and NVENC

Frames come from the standard ScreenCast portal (`xdg-desktop-portal`), which hands over a PipeWire stream. A standalone C helper (`src/pw_portal_capture.c`) negotiates DMA-BUF or MemFd buffers, maps each one, and pipes raw frames to FFmpeg, which encodes with NVENC (or libx264 if NVENC isn't available):

```c
// src/pw_portal_capture.c
"ffmpeg -y -hide_banner -loglevel error "
"-f rawvideo -pix_fmt %s -video_size %ux%u -framerate %d -i pipe:0 "
"-c:v h264_nvenc -preset p4 -tune hq -rc vbr -cq 19 -b:v 0 -pix_fmt yuv420p -movflags +faststart \"%s\" "
"|| ffmpeg ... -c:v libx264 -preset veryfast -crf 19 ..."
```

Two details made this pleasant to use:

- **The portal restore token is saved**, so you pick a screen once instead of on every recording.
- **The helper is a separate process**, so a portal or PipeWire hiccup can't take the UI down with it.

To be clear about the cost: this path copies each frame once in system memory. It isn't zero-copy. Importing the DMA-BUF straight into the encoder would remove that copy, and that's a great contribution if you know VAAPI or NVENC surface import.

## The UI: a recording island that isn't a window

The controls live in a small "island" at the top of the screen, built with GTK4 and `gtk4-layer-shell`. A layer-shell surface (`zwlr_layer_shell_v1`) is anchored to the output rather than managed like a window, so it doesn't steal focus or show up in the taskbar. If the compositor doesn't support layer-shell, it opens as a normal window instead.

![HyperGSR studio drawer below the recording island, with encoder, zoom and combo settings](https://kunalsiyag.github.io/gpu-recorder-rs/island_dark_studio.png)

## Numbers

Measured during a 1080p60 recording on a hybrid AMD Radeon + NVIDIA RTX 3050 laptop, COSMIC, NVENC preset P4:

| | HyperGSR | OBS Studio | Electron-based studio recorders |
|---|---|---|---|
| Resident memory | **56 MB** | 340 MB | 650+ MB |
| CPU | under 1.5% | — | — |

OBS and gpu-screen-recorder are excellent general-purpose recorders, and this isn't a replacement for them. HyperGSR does one thing they don't do out of the box: a camera that follows the cursor on Wayland. Your numbers will vary with GPU, compositor and resolution; `gpu-recorder-rs --report` prints the encoders and cursor planes it finds on your machine.

![Comparison table: cursor-following auto-zoom, global Wayland cursor without root, custom zoom combos, hardware encoding, memory and license across HyperGSR, gpu-screen-recorder, OBS Studio and Electron recorders](https://kunalsiyag.github.io/gpu-recorder-rs/screenshots/compare-dark.png)

## Building the landing page

I wanted the site to feel like an instrument, not a SaaS template, so I built it on a reference design system (Graphite from Refero) with strict rules: monochrome buttons, one accent color reserved for links, no emoji, a light/dark/system theme switch, and real code instead of illustrations. The full token set and the QA log are in [DESIGN.md](https://github.com/KunalSiyag/gpu-recorder-rs/blob/main/docs/DESIGN.md).

![Architecture section of the landing page in light theme: cursor, camera and capture modules with code excerpts](https://kunalsiyag.github.io/gpu-recorder-rs/screenshots/architecture-light.png)

It works on phones too:

![HyperGSR landing page on a 390px mobile screen in dark and light themes](https://kunalsiyag.github.io/gpu-recorder-rs/screenshots/mobile-hero-pair.png)

## What I'd do differently

- **Start from the cursor, not the encoder.** I spent the first days on capture, but the cursor source decided the whole architecture. If you're building anything cursor-aware on Wayland, check `drm_info` for a cursor plane first.
- **Tune the trigger before the spring.** Almost every "this feels wrong" report came from *when* the camera zoomed, not *how* it moved.
- **Measure the copy.** "GPU recorder" invites the assumption of zero-copy. Profile your capture path and say what it actually does.

## Try it, or help out

```bash
git clone https://github.com/KunalSiyag/gpu-recorder-rs
cd gpu-recorder-rs && ./install.sh
./target/release/gpu-recorder-rs
```

There's also a [prebuilt x86_64 binary in the v1.0.0 release](https://github.com/KunalSiyag/gpu-recorder-rs/releases/latest).

Each open issue names the file to start in:

- [#1 Add Catppuccin Mocha, Tokyo Night and Nord themes to the island](https://github.com/KunalSiyag/gpu-recorder-rs/issues/1) (good first issue, `src/gui.rs`)
- [#2 Show pressed keys as an on-screen pill in the recorded MP4](https://github.com/KunalSiyag/gpu-recorder-rs/issues/2) (`src/encoder.rs`)
- [#3 Add wallpaper padding and a drop-shadow frame to exports](https://github.com/KunalSiyag/gpu-recorder-rs/issues/3) (good first issue, `src/encoder.rs`)

If you've solved the Wayland cursor problem a different way, I'd like to hear it in the comments. [mathisdev7](https://dev.to/mathisdev7) has written about building a Screen Studio alternative for Linux with Rust and wgpu, and about the state of Linux screen recording in 2026; worth reading next to this one.

**Links:** [Landing page and live spring camera](https://kunalsiyag.github.io/gpu-recorder-rs/) · [Source on GitHub](https://github.com/KunalSiyag/gpu-recorder-rs) · [Reverse-engineering notes](https://github.com/KunalSiyag/gpu-recorder-rs/blob/main/RE_ANALYSIS.md)
