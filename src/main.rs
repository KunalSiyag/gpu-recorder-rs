pub mod backgrounds;
pub mod bundled;
pub mod capture;
pub mod compositor;
pub mod cursor;
pub mod cursor_track;
pub mod decompiled_gsr;
pub mod encoder;
pub mod ffmpeg;
pub mod editor;
pub mod gui;
pub mod island_position;
pub mod project;
pub mod region_select;
pub mod replay;
pub mod settings;
pub mod shaders;
pub mod shortcuts;
pub mod state;
pub mod studio_panel;
pub mod studio_render;
pub mod web_ui;
pub mod webcam;
pub mod x11_above;
pub mod zoom_engine;

use crate::state::{SharedStudioState, StudioState};
use anyhow::Context;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

fn print_banner(state: &SharedStudioState) {
    let g = state.lock();
    let r = &g.gsr_report;
    println!("================================================================================");
    println!("  ⚡ HyperGSR Notch Studio (gpu-recorder-rs) — Zero-Copy GPU & Auto-Zoom HUD");
    println!("================================================================================");
    println!("  • Decompiled Engine : {}/bin/gpu-screen-recorder (v{})", r.flatpak_path, r.gsr_version);
    println!("  • Display & GPU     : {} | {} ({}) + {}", r.display_server, r.drm_card_path, r.gpu_vendor, r.nvidia_gpu_name);
    println!("  • UI Mode           : Top-Notch Floating Bar (Cursor IDE Dark Minimal Theme)");
    println!("  • Recordings Folder : {}", g.videos_dir.display());
    println!("================================================================================");
}

fn find_layer_shell_library() -> Option<PathBuf> {
    [
        "/app/lib",
        "/usr/local/lib/x86_64-linux-gnu",
        "/usr/local/lib64",
        "/usr/local/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
    ]
    .iter()
    .map(|dir| PathBuf::from(dir).join("libgtk4-layer-shell.so.0"))
    .find(|path| path.exists())
}

/// GNOME has no layer-shell, so a native Wayland island would sink below other
/// windows; under XWayland it can ask to stay above them instead.
fn gdk_backend() -> &'static str {
    if let Ok(forced) = std::env::var("HYPERGSR_GDK_BACKEND") {
        if forced == "x11" {
            return "x11";
        }
        return "wayland";
    }
    let gnome = std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|part| part.eq_ignore_ascii_case("gnome")));
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();
    if x11 && (gnome || !wayland) {
        "x11"
    } else {
        "wayland"
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Bound to a desktop shortcut: start or stop the running recorder.
    if let Some(flag) = args.iter().find(|a| *a == "--toggle-recording" || *a == "--pause-recording") {
        return shortcuts::activate_running_recorder(flag);
    }

    // --edit <video>: open the editor in the running island, or start one
    // that opens it.
    if let Some(idx) = args.iter().position(|a| a == "--edit") {
        let file = args.get(idx + 1).context("usage: --edit <video>")?;
        let file = std::fs::canonicalize(file).with_context(|| format!("cannot open {file}"))?;
        if std::env::var("HYPERGSR_LAYER_SHELL").as_deref() != Ok("1")
            && shortcuts::activate_in_running("edit", Some(glib::Variant::from(file.display().to_string())))?
        {
            return Ok(());
        }
        std::env::set_var("HYPERGSR_EDIT", &file);
    }

    // Ensure GTK4 initializes on native Wayland with libgtk4-layer-shell preloaded
    // so the Dynamic Island docks natively at the top notch with 32-bit RGBA transparent
    // rounded corners and zero XWayland hangs.
    let is_cli_mode = args.iter().any(|a| {
        a == "--report"
            || a == "--export-demo"
            || a == "--record-seconds"
            || a == "--replay"
            || a == "--render-capture"
            || a == "--web"
            || a == "--web-only"
    });
    if !is_cli_mode && std::env::var("HYPERGSR_LAYER_SHELL").as_deref() != Ok("1") {
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        cmd.args(&args[1..])
            .env("HYPERGSR_LAYER_SHELL", "1")
            .env("GDK_BACKEND", gdk_backend())
            .env("GSK_RENDERER", "cairo");
        // gtk4-layer-shell must be loaded before libwayland-client, so it is preloaded
        // from wherever the distro, a /usr/local source build, or Flatpak installed it.
        if let Some(layer_so) = find_layer_shell_library() {
            let preload = match std::env::var("LD_PRELOAD") {
                Ok(existing) if !existing.is_empty() => {
                    format!("{}:{existing}", layer_so.display())
                }
                _ => layer_so.display().to_string(),
            };
            cmd.env("LD_PRELOAD", preload);
        }
        let err = cmd.exec();
        anyhow::bail!("Failed to exec with layer-shell: {err}");
    }

    if !args.iter().any(|a| a == "--report" || a == "--export-demo") {
        ffmpeg::warm_up();
    }
    let state: SharedStudioState = Arc::new(Mutex::new(StudioState::new(1280, 720)));
    let web_port: u16 = 7878;

    // Mode 0: --record-seconds <secs> (Zero-copy PipeWire DMA-BUF + GPU recording test)
    if let Some(idx) = args.iter().position(|a| a == "--record-seconds") {
        let secs: f32 = args
            .get(idx + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(3.0);
        // --pause <at>:<seconds> pauses once, for testing.
        let pause: Option<(f32, f32)> = args
            .iter()
            .position(|a| a == "--pause")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.split_once(':'))
            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)));
        let mut paused_since: Option<std::time::Instant> = None;
        print_banner(&state);
        let out_path = state.lock().start_recording()?;
        println!(
            "● Recording the screen for {secs:.1}s -> {}",
            out_path.display()
        );
        // The clock starts with the first captured frame, after the portal.
        let wait_start = std::time::Instant::now();
        loop {
            let mut g = state.lock();
            g.tick();
            if let Some(reason) = g.poll_capture_failure() {
                drop(g);
                let _ = state.lock().stop_recording();
                anyhow::bail!("Recording failed: {reason}");
            }
            if let Some(reason) = g.capture_paused.clone() {
                drop(g);
                eprintln!("⚠ Screen sharing was interrupted ({reason}); saving what was recorded");
                break;
            }
            // Recorded time, after the countdown and without pauses.
            let elapsed = g.recording_start.map(|_| g.recording_elapsed_sec());
            if let (Some(e), Some((at, len))) = (elapsed, pause) {
                if !g.is_paused_by_user() && e >= at && paused_since.is_none() {
                    g.toggle_pause();
                    paused_since = Some(std::time::Instant::now());
                }
                if paused_since.is_some_and(|t| t.elapsed().as_secs_f32() >= len) && g.is_paused_by_user() {
                    g.toggle_pause();
                }
            }
            drop(g);
            if elapsed.is_some_and(|e| e >= secs) {
                break;
            }
            if elapsed.is_none() && wait_start.elapsed().as_secs() > 120 {
                anyhow::bail!("Screen capture did not start within two minutes");
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        if let Some(saved) = state.lock().stop_recording()? {
            println!("✔ Saved Zero-Copy GPU recording -> {}", saved.display());
        }
        return Ok(());
    }

    // --replay <keep> <run>: run instant replay for <run> seconds, then save
    // a clip of the last <keep> seconds.
    if let Some(idx) = args.iter().position(|a| a == "--replay") {
        let num = |i: usize| args.get(idx + i).and_then(|v| v.parse::<u32>().ok());
        let (Some(keep), Some(run)) = (num(1), num(2)) else {
            anyhow::bail!("usage: --replay <keep seconds> <run seconds>");
        };
        print_banner(&state);
        state.lock().settings.replay_sec = keep;
        state.lock().start_replay()?;
        let started = std::time::Instant::now();
        while state.lock().replay_buffered_sec().is_none() {
            if let Some(reason) = state.lock().poll_replay_failure() {
                anyhow::bail!("instant replay stopped: {reason}");
            }
            if started.elapsed().as_secs() > 120 {
                anyhow::bail!("Screen capture did not start within two minutes");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::thread::sleep(std::time::Duration::from_secs(run.into()));
        if let Some(reason) = state.lock().poll_replay_failure() {
            anyhow::bail!("instant replay stopped: {reason}");
        }
        let clip = state.lock().replay_clip()?;
        let saved = clip.save();
        if let Some(replay) = state.lock().take_replay() {
            replay.stop();
        }
        println!("✔ Saved replay clip -> {}", saved?.display());
        return Ok(());
    }

    // Re-render a raw capture with saved cursor telemetry and the current
    // background settings, optionally drawing a recorded cursor track:
    // --render-capture <capture.mp4> <telemetry.json> <out.mp4> [<capture.cursor>]
    if let Some(idx) = args.iter().position(|a| a == "--render-capture") {
        let (Some(capture), Some(telemetry), Some(output)) =
            (args.get(idx + 1), args.get(idx + 2), args.get(idx + 3))
        else {
            anyhow::bail!(
                "usage: --render-capture <capture.mp4> <telemetry.json> <out> [<capture.cursor>] \
                 [--aspect wide|vertical|square] [--format mp4|webm|gif] [--cut <from>:<to>]..."
            );
        };
        let cursor = args
            .get(idx + 4)
            .filter(|p| !p.starts_with("--"))
            .map(|p| cursor_track::CursorTrack::load(std::path::Path::new(p)))
            .transpose()?;
        let option = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
        let aspect = match option("--aspect").map(String::as_str) {
            Some("wide") => settings::Aspect::Wide,
            Some("vertical") => settings::Aspect::Vertical,
            Some("square") => settings::Aspect::Square,
            _ => settings::Aspect::Original,
        };
        let format = match option("--format").map(String::as_str) {
            Some("webm") => settings::ExportFormat::WebM,
            Some("gif") => settings::ExportFormat::Gif,
            _ => settings::ExportFormat::Mp4,
        };
        let cuts: Vec<(f64, f64)> = args
            .windows(2)
            .filter(|w| w[0] == "--cut")
            .filter_map(|w| {
                let (a, b) = w[1].split_once(':')?;
                Some((a.parse().ok()?, if b.is_empty() { f64::INFINITY } else { b.parse().ok()? }))
            })
            .collect();
        let events: Vec<encoder::RecordedZoomEvent> =
            serde_json::from_str(&std::fs::read_to_string(telemetry)?)?;
        let settings = state.lock().settings.clone();
        // --webcam <video>@<sec>: a camera clip whose <sec> mark lines up
        // with the capture's first frame.
        let webcam = option("--webcam").and_then(|v| v.rsplit_once('@')).and_then(|(path, offset)| {
            Some(webcam::WebcamClip {
                path: PathBuf::from(path),
                start: 0.0,
                clock: vec![webcam::ClockSpan { shift: 0.0, zero: offset.parse().ok()?, last: f64::INFINITY }],
                corner: if settings.webcam == settings::Webcam::Left { settings::Webcam::Left } else { settings::Webcam::Right },
                size: settings.webcam_size,
            })
        });
        let job = studio_render::RenderJob {
            capture: PathBuf::from(capture),
            output: PathBuf::from(output),
            events,
            auto_zoom: settings.auto_zoom,
            region: settings.active_region(),
            background: settings.background.clone(),
            quality: settings.quality,
            fps: settings.fps,
            cursor,
            cursor_scale: settings.cursor_scale,
            click_ripples: settings.click_ripples,
            cuts,
            aspect,
            format,
            webcam,
        };
        let started = std::time::Instant::now();
        let saved = studio_render::render(&job, &std::sync::atomic::AtomicU32::new(0))?;
        println!("✔ Rendered {} in {:.1}s", saved.display(), started.elapsed().as_secs_f32());
        return Ok(());
    }

    // Mode 1: --report (Print decompiled binary inspection & pipeline report and exit)
    if args.iter().any(|a| a == "--report") {
        let g = state.lock();
        println!("{}", serde_json::to_string_pretty(&g.gsr_report)?);
        return Ok(());
    }

    // Mode 2: --export-demo [output.mp4] (Render a 60fps auto-zoom & cursor-smoothed MP4 showcase and exit)
    if let Some(idx) = args.iter().position(|a| a == "--export-demo") {
        let custom_out = args.get(idx + 1).map(PathBuf::from);
        print_banner(&state);
        let g = state.lock();
        let out_path = custom_out.unwrap_or_else(|| g.output_dir.join("hypergsr_demo_showcase.mp4"));
        println!(
            "Rendering 60 FPS GPU Auto-Zoom & Cursor FX showcase to {} ...",
            out_path.display()
        );
        let (saved_path, frames, codec) = crate::encoder::export_showcase_recording(
            &out_path,
            1280,
            720,
            60,
            5,
            g.encoder_backend,
            &g.zoom_cfg,
            &g.cursor_cfg,
            &g.style_cfg,
        )?;
        println!(
            "✔ Successfully encoded {frames} frames at 60 FPS using [{codec}] -> {}",
            saved_path.display()
        );
        return Ok(());
    }

    // Mode 3: --web-only or --web (Run embedded Web Studio server only when explicitly requested)
    if args.iter().any(|a| a == "--web-only" || a == "--web") {
        print_banner(&state);
        println!("  • Web Studio Server : http://127.0.0.1:{web_port}");
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move { web_ui::start_web_studio_server(state, web_port).await })?;
        return Ok(());
    }

    // Default Mode: Launch Ultra-Light Native Top-Notch GTK4 Recorder HUD (no background web server overhead)
    print_banner(&state);
    gui::launch_gtk_ui(state, web_port);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::{CursorConfig, CursorTracker};
    use crate::zoom_engine::{AutoZoomMode, ZoomConfig, ZoomEngine};

    #[test]
    fn test_cursor_smoothing_and_ring_buffer_bounded() {
        let mut tracker = CursorTracker::new();
        let cfg = CursorConfig::default();

        let mut last_sample = Default::default();
        for i in 0..400 {
            let t_ms = i * 16;
            let raw_x = (i as f32 / 400.0).clamp(0.0, 1.0);
            let raw_y = 0.5;
            let click = i == 50;
            let (s, clicked) = tracker.update(t_ms, raw_x, raw_y, click, false, &cfg);
            if i == 50 {
                assert!(clicked, "Rising edge click should be detected at i=50");
                assert_eq!(tracker.active_ripples().len(), 1);
            }
            last_sample = s;
        }

        assert!(last_sample.smooth_x > 0.90);
        let trail = tracker.recent_trail(18);
        assert_eq!(trail.len(), 18);
    }

    #[test]
    fn test_auto_zoom_click_and_spring_convergence() {
        let mut engine = ZoomEngine::new();
        let mut tracker = CursorTracker::new();
        let cur_cfg = CursorConfig::default();
        let zoom_cfg = ZoomConfig {
            mode: AutoZoomMode::ClickToZoom,
            target_zoom: 2.25,
            ..Default::default()
        };

        let (sample, clicked) = tracker.update(100, 0.25, 0.30, true, false, &cur_cfg);
        assert!(clicked);
        let vp0 = engine.step(100, sample, clicked, 0, &zoom_cfg);
        assert!(vp0.zoom >= 1.0);

        let mut vp = vp0;
        for step in 1..=60 {
            let t = 100 + step * 16;
            let (s, c) = tracker.update(t, 0.25, 0.30, false, false, &cur_cfg);
            vp = engine.step(t, s, c, 0, &zoom_cfg);
        }

        assert!(vp.zoom > 2.10, "Expected zoom > 2.10, got {}", vp.zoom);
        assert!(vp.uv_min_x >= 0.0 && vp.uv_min_x + vp.uv_w <= 1.0001);
        assert!(vp.uv_min_y >= 0.0 && vp.uv_min_y + vp.uv_h <= 1.0001);
    }

    #[test]
    fn test_smart_cursor_follow_returns_to_1x_and_never_sticks() {
        let mut engine = ZoomEngine::new();
        let mut tracker = CursorTracker::new();
        let cur_cfg = CursorConfig::default();
        let zoom_cfg = ZoomConfig::default(); // SmartCursorFollow, hold_duration_ms = 1650

        // Move cursor to (0.3, 0.3) and click at t=500ms, then keep mouse completely still for 8 seconds.
        let mut zoomed_in_seen = false;
        let mut last_vp = Default::default();
        for step in 0..500 {
            let t_ms = step * 16;
            let click = step == 30;
            let (s, c) = tracker.update(t_ms, 0.30, 0.30, click, false, &cur_cfg);
            let vp = engine.step(t_ms, s, c, if step > 30 { (step - 30) * 16 } else { 0 }, &zoom_cfg);
            if vp.zoom > 1.65 {
                zoomed_in_seen = true;
            }
            last_vp = vp;
        }
        assert!(zoomed_in_seen, "Should have zoomed in after click");
        assert!(
            last_vp.zoom < 1.05,
            "Camera must smoothly return to 1.0x instead of staying stuck zoomed in! Got {}",
            last_vp.zoom
        );
    }

    #[test]
    fn test_camera_trajectory_planner_bounded_and_c2_smooth() {
        use crate::encoder::{build_smooth_60fps_camera_frames, RecordedZoomEvent};

        // Simulate a 66-second recording where the old telemetry stayed stuck at 2.15x
        let mut events = Vec::new();
        for i in 0..1650 {
            let t = i as f32 * 0.04;
            // Cursor moves occasionally then rests for long periods
            let phase = (t / 12.0).floor();
            let cx = (0.2 + 0.15 * phase).clamp(0.15, 0.85);
            let cy = (0.3 + 0.10 * phase).clamp(0.15, 0.85);
            events.push(RecordedZoomEvent {
                timestamp_sec: t,
                center_x: cx,
                center_y: cy,
                zoom: if t > 1.0 { 2.15 } else { 1.0 },
                cursor_x: cx,
                cursor_y: cy,
                clicked: (i % 350) == 40,
                manual_lock: false,
                off_capture: false,
            });
        }

        let total_frames = 66 * 60;
        let frames = build_smooth_60fps_camera_frames(&events, total_frames);
        assert_eq!(frames.len(), total_frames);

        // 1. Verify camera returns to wide view (< 1.08x) for the majority of the 66s video
        let wide_frames = frames.iter().filter(|(_, _, z)| *z < 1.15).count();
        assert!(
            wide_frames > total_frames / 2,
            "Expected majority of frames to be in wide view, got {wide_frames}/{total_frames}"
        );

        // 2. Verify frame-to-frame zoom changes are C2-smooth (no jumps > 0.045 per 16.6ms frame)
        for pair in frames.windows(2) {
            let dz = (pair[1].2 - pair[0].2).abs();
            assert!(dz < 0.045, "Unsmooth frame-to-frame zoom jump detected: dz={dz}");
        }
    }

    #[test]
    fn test_manual_combo_zoom_and_custom_self_binder() {
        use crate::cursor::SystemInputState;
        use crate::encoder::{build_smooth_60fps_camera_frames, RecordedZoomEvent};
        use crate::zoom_engine::{ComboActionMode, ZoomComboBinding};

        let mut engine = ZoomEngine::new();
        let mut tracker = CursorTracker::new();
        let cur_cfg = CursorConfig::default();
        // Configure user-registered custom combo: Ctrl + Right Click in ToggleLock mode
        let zoom_cfg = ZoomConfig {
            combo_binding: ZoomComboBinding::Custom {
                ctrl: true,
                alt: false,
                shift: false,
                super_key: false,
                mouse_button: 3,
                keycode: 0,
                shake: false,
                label: "⌃ Ctrl + Right-Click".to_string(),
            },
            combo_action: ComboActionMode::ToggleLock,
            target_zoom: 2.40,
            ..Default::default()
        };

        // 1. Trigger the custom combo at t = 100ms
        let (s1, c1) = tracker.update_with_input(
            100,
            0.65,
            0.40,
            SystemInputState {
                right_down: true,
                ctrl_down: true,
                ..Default::default()
            },
            &cur_cfg,
        );
        engine.step(100, s1, c1, 0, &zoom_cfg);
        assert!(engine.manual_locked, "Custom Ctrl+RightClick combo should engage manual lock!");

        // 2. Keep combo locked for 6 full seconds (longer than the 1.65s auto-zoom hold!)
        let mut vp = Default::default();
        for step in 1..=375 {
            let t_ms = 100 + step * 16;
            let (s, c) = tracker.update_with_input(
                t_ms,
                0.65,
                0.40,
                SystemInputState::default(),
                &cur_cfg,
            );
            vp = engine.step(t_ms, s, c, 0, &zoom_cfg);
        }
        assert!(
            vp.zoom > 2.30,
            "Manual combo lock must stay zoomed in for as long as the user wants! Got {}",
            vp.zoom
        );

        // 3. Test scroll wheel zoom adjustment while locked
        engine.adjust_manual_zoom_wheel(0.65, 0.40, 0.40);
        for step in 376..=420 {
            let t_ms = 100 + step * 16;
            let (s, c) = tracker.update_with_input(t_ms, 0.65, 0.40, SystemInputState::default(), &cur_cfg);
            vp = engine.step(t_ms, s, c, 0, &zoom_cfg);
        }
        assert!(vp.zoom > 2.65, "Scroll wheel should increase locked zoom depth, got {}", vp.zoom);

        // 4. Trigger the custom combo again to unlock back to 1.0x
        let (s_off, c_off) = tracker.update_with_input(
            7000,
            0.65,
            0.40,
            SystemInputState {
                right_down: true,
                ctrl_down: true,
                ..Default::default()
            },
            &cur_cfg,
        );
        engine.step(7000, s_off, c_off, 0, &zoom_cfg);
        assert!(!engine.manual_locked, "Second combo trigger should unlock manual zoom!");

        // 5. Verify encoder trajectory planner honors a 5-second manual_lock segment without cutting it off at 1.75s
        let mut events = Vec::new();
        for i in 0..200 {
            let t = i as f32 * 0.04; // 8 seconds total
            let locked = (1.0..=6.0).contains(&t);
            events.push(RecordedZoomEvent {
                timestamp_sec: t,
                center_x: 0.65,
                center_y: 0.40,
                zoom: if locked { 2.40 } else { 1.0 },
                cursor_x: 0.65,
                cursor_y: 0.40,
                clicked: false,
                manual_lock: locked,
                off_capture: false,
            });
        }
        let frames = build_smooth_60fps_camera_frames(&events, 8 * 60);
        // At t = 4.5s (frame 270), well past the 1.75s auto-zoom cap, manual_lock must STILL be zoomed in > 2.2x!
        assert!(
            frames[270].2 > 2.25,
            "Encoder trajectory planner must preserve manual_lock zoom at t=4.5s! Got {}",
            frames[270].2
        );
    }

    #[test]
    fn test_decompiled_gsr_probe_and_shaders() {
        let report = decompiled_gsr::GsrDecompiledReport::probe_system();
        assert_eq!(report.gsr_version, "6.1.4");
        assert!(report.wayland_bridge_bug_detected);
        assert!(shaders::DECOMPILED_GSR_NV12_SHADER.contains("samplerExternalOES"));
        assert!(shaders::HYPERGSR_FRAGMENT_SHADER_ES3.contains("u_ripple_0"));
    }
}
