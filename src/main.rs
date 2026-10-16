pub mod capture;
pub mod compositor;
pub mod cursor;
pub mod decompiled_gsr;
pub mod encoder;
pub mod gui;
pub mod shaders;
pub mod state;
pub mod web_ui;
pub mod zoom_engine;

use crate::state::{SharedStudioState, StudioState};
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

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Ensure GTK4 initializes on native Wayland with libgtk4-layer-shell preloaded
    // so the Dynamic Island docks natively at the top notch with 32-bit RGBA transparent
    // rounded corners and zero XWayland hangs.
    let is_cli_mode = args.iter().any(|a| {
        a == "--report"
            || a == "--export-demo"
            || a == "--record-seconds"
            || a == "--web"
            || a == "--web-only"
    });
    if !is_cli_mode && std::env::var("HYPERGSR_LAYER_SHELL").as_deref() != Ok("1") {
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe()?;
        let layer_so = "/usr/local/lib/x86_64-linux-gnu/libgtk4-layer-shell.so.0";
        let mut cmd = std::process::Command::new(exe);
        cmd.args(&args[1..])
            .env("HYPERGSR_LAYER_SHELL", "1")
            .env("GDK_BACKEND", "wayland")
            .env("GSK_RENDERER", "cairo");
        if std::path::Path::new(layer_so).exists() {
            cmd.env("LD_PRELOAD", layer_so);
        }
        let err = cmd.exec();
        anyhow::bail!("Failed to exec with layer-shell: {err}");
    }

    let state: SharedStudioState = Arc::new(Mutex::new(StudioState::new(1280, 720)));
    let web_port: u16 = 7878;

    // Mode 0: --record-seconds <secs> (Zero-copy PipeWire DMA-BUF + GPU recording test)
    if let Some(idx) = args.iter().position(|a| a == "--record-seconds") {
        let secs: f32 = args
            .get(idx + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(3.0);
        print_banner(&state);
        let out_path = state.lock().start_recording()?;
        println!(
            "● Recording live desktop via Zero-Copy PipeWire DMA-BUF + GPU for {secs:.1}s -> {}",
            out_path.display()
        );
        let steps = (secs * 25.0) as usize;
        for _ in 0..steps {
            state.lock().tick();
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        if let Some(saved) = state.lock().stop_recording()? {
            println!("✔ Saved Zero-Copy GPU recording -> {}", saved.display());
        }
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
        let mut zoom_cfg = ZoomConfig::default();
        zoom_cfg.mode = AutoZoomMode::ClickToZoom;
        zoom_cfg.target_zoom = 2.25;

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
    fn test_decompiled_gsr_probe_and_shaders() {
        let report = decompiled_gsr::GsrDecompiledReport::probe_system();
        assert_eq!(report.gsr_version, "6.1.4");
        assert!(report.wayland_bridge_bug_detected);
        assert!(shaders::DECOMPILED_GSR_NV12_SHADER.contains("samplerExternalOES"));
        assert!(shaders::HYPERGSR_FRAGMENT_SHADER_ES3.contains("u_ripple_0"));
    }
}
