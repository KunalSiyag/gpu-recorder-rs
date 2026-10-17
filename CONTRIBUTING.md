# Contributing to HyperGSR (`gpu-recorder-rs`) 🍴

First off, thank you for considering contributing to **HyperGSR**! Our goal is to build the fastest, cleanest, native Rust alternative to macOS **Screen Studio** for Linux Wayland and X11 desktops.

Whether you want to add a custom Dynamic Island theme, a new spring camera preset, a new gesture trigger, or a GLSL post-processing shader, **forking and hacking takes less than 5 minutes**.

---

## ⚡ Quick Fork & Build Workflow

1. **Fork the repository** on GitHub: [Fork `KunalSiyag/gpu-recorder-rs`](https://github.com/KunalSiyag/gpu-recorder-rs/fork)
2. **Clone your fork locally**:
   ```bash
   git clone https://github.com/<your-username>/gpu-recorder-rs.git
   cd gpu-recorder-rs
   ```
3. **Build & run the test suite**:
   ```bash
   cargo test
   cargo build --release
   ./target/release/gpu-recorder-rs
   ```

---

## 🛠️ 4 Easy Ways to Hack & Contribute

### 1. 🎨 Add a New Dynamic Island Theme (`src/gui.rs`)
Currently, HyperGSR ships with **Obsidian Dark (`🌙`)** and **Pearl Light (`☀️`)** in `load_dynamic_island_css()` inside `src/gui.rs` (light-mode rules are scoped under `.theme-light`).
- **Great PR ideas**: Add **Catppuccin Mocha**, **TokyoNight**, **Nord**, or **Gruvbox Dark** as a new theme class in `load_dynamic_island_css()` in `src/gui.rs`, next to `.theme-light`.

### 2. 🎮 Add a New Zoom Combo Gesture (`src/zoom_engine.rs`)
Self-registering and preset zoom combinations live in `ZoomComboBinding` inside `src/zoom_engine.rs`; raw input and shake detection are in `src/cursor.rs`.
- **Great PR ideas**:
  - Circle-the-cursor gesture detector (draw a small circle with the pointer to zoom in!)
  - Screen hot-corner zoom trigger
  - Custom `evdev` touchpad pinch-to-zoom reader

### 3. 🎥 Add Camera Easing / Spring Presets (`src/zoom_engine.rs`)
The 240 Hz sub-stepped spring camera lives in `src/zoom_engine.rs` (`ZoomEngine::step`); the post-recording trajectory planner is `build_smooth_60fps_camera_frames` in `src/encoder.rs`.
- **Great PR ideas**:
  - Add an optional slight under-damped bounce preset ($\zeta = 0.82$) for playful product promos
  - Add a **Wallpaper Frame & Rounded Shadow Padding** pass in `src/encoder.rs`

### 4. ⌨️ Keystroke Visualizer HUD (`src/gui.rs` / `src/encoder.rs`)
Render a sleek pill at the bottom of the recording whenever shortcuts like `Ctrl + S` or `Super + Enter` are pressed.

---

## ✅ Pull Request Checklist

- [ ] `cargo test` passes with 0 failures.
- [ ] `cargo build --release` compiles cleanly without warnings.
- [ ] If your PR changes the UI, attach a screenshot or short MP4/GIF in the PR description!
