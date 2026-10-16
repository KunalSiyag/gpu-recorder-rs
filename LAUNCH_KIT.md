# 🚀 HyperGSR (`gpu-recorder-rs`) — Viral Launch & Distribution Kit

Use the ready-to-post templates below to share **HyperGSR** across the highest-converting developer communities.

---

## 1. Hacker News (`Show HN`)
- **Submit URL**: `https://github.com/KunalSiyag/gpu-recorder-rs`
- **Title**:
  `Show HN: HyperGSR – Screen Studio for Linux Wayland in 56MB of native Rust (DRM cursor plane #92 + 240Hz spring auto-zoom)`
- **First Comment (post immediately after submitting)**:
  > Hey HN! I wanted macOS Screen Studio's smooth cursor auto-zoom and Dynamic Island HUD on Linux Wayland (running COSMIC / Hyprland / GNOME on a hybrid AMD + NVIDIA laptop), without 650MB of Electron overhead.
  >
  > When I disassembled `gpu-screen-recorder` v6.1.4 to see how existing Linux GPU recorders handle cursor tracking (`RE_ANALYSIS.md` in the repo), I hit two big walls on Wayland:
  > 1. Standard X11 `XQueryPointer` / `XFixes` cursor polling either goes blind over native Wayland surfaces or blocks the UI thread on rootless XWayland.
  > 2. Wayland security intentionally hides global pointer coordinates from client apps.
  >
  > To solve this without requiring root (`sudo`) or compositor-specific patches, HyperGSR opens `/dev/dri/card*` via `libdrm`, enables `DRM_CLIENT_CAP_UNIVERSAL_PLANES` + `DRM_CLIENT_CAP_ATOMIC`, and polls Hardware Cursor Plane `#92` (`CRTC_X`, `CRTC_Y`, `FB_ID`) at 60Hz. That gives us exact global cursor coordinates + cursor bitmap changes (e.g. pointer -> hand when hovering a button) across every Wayland & X11 app.
  >
  > Those coordinates feed a 240Hz sub-stepped critically-damped 2D spring camera (`x'' = -k(x - x_t) - 2*sqrt(k)*x'`) and a bounded Screen Studio trajectory planner, plus a native `zwlr_layer_shell_v1` Dynamic Island bar with self-registering custom zoom combos (shake cursor, double-click, middle-click, `Ctrl+Alt`, mouse wheel zoom).
  >
  > It runs in ~56MB RSS RAM and <1.5% CPU. Everything is MIT-licensed — would love your feedback, stars, and PRs!

---

## 2. Reddit — `r/rust`
- **Title**:
  `I built a native Rust alternative to macOS Screen Studio for Linux Wayland (~56MB RAM, rootless DRM cursor plane tracking, 240Hz spring camera, GTK4 Layer-Shell Dynamic Island)`
- **Link**: `https://github.com/KunalSiyag/gpu-recorder-rs`

---

## 3. Reddit — `r/linux` & `r/unixporn`
- **Title**:
  `[OC] HyperGSR: Screen Studio-style Auto-Zoom Screen Recorder & Dynamic Island HUD for Linux Wayland (56MB RAM, Native Rust + PipeWire + NVENC/VAAPI)`
- **Link**: `https://github.com/KunalSiyag/gpu-recorder-rs`

---

## 4. X / Twitter Thread
**Tweet 1:**
I wanted macOS Screen Studio on Linux Wayland — without 650MB of Electron bloat.

So we reverse-engineered GPU screen recording on Linux and built **HyperGSR** in native Rust 🦀:
• ~56 MB RAM / <1.5% CPU
• Rootless Wayland cursor tracking via DRM Plane #92
• 240Hz Spring-Physics Auto-Zoom + Custom Gesture Combos
• Wayland Layer-Shell Dynamic Island UI (Dark & Light)

100% Open Source (MIT) 👇
https://github.com/KunalSiyag/gpu-recorder-rs
