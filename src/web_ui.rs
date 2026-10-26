use crate::compositor::BackdropTheme;
use crate::encoder::HardwareEncoderBackend;
use crate::state::SharedStudioState;
use crate::zoom_engine::AutoZoomMode;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use image::{codecs::jpeg::JpegEncoder, ColorType};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CursorInteractPayload {
    pub x: f32,
    pub y: f32,
    pub left_click: bool,
    pub right_click: bool,
}

#[derive(Deserialize)]
pub struct ManualZoomPayload {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    pub hold_ms: Option<u64>,
}

#[derive(Deserialize)]
pub struct SettingsUpdatePayload {
    pub auto_zoom_enabled: Option<bool>,
    pub zoom_mode: Option<String>,
    pub target_zoom: Option<f32>,
    pub spring_stiffness: Option<f32>,
    pub hold_duration_ms: Option<u64>,
    pub smoothing_enabled: Option<bool>,
    pub click_ripples: Option<bool>,
    pub motion_trail: Option<bool>,
    pub spotlight_enabled: Option<bool>,
    pub show_keystrokes: Option<bool>,
    pub backdrop: Option<String>,
    pub padding: Option<f32>,
    pub encoder: Option<String>,
}

pub async fn start_web_studio_server(state: SharedStudioState, port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index_html))
        .route("/api/telemetry", get(get_telemetry))
        .route("/api/decompiled", get(get_decompiled_report))
        .route("/api/frame.jpg", get(get_preview_jpeg))
        .route("/api/interact", post(post_cursor_interact))
        .route("/api/manual-zoom", post(post_manual_zoom))
        .route("/api/settings", post(post_settings))
        .route("/api/record/toggle", post(post_toggle_record))
        .route("/api/export-showcase", post(post_export_showcase))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index_html() -> Html<&'static str> {
    Html(STUDIO_WEB_HTML)
}

async fn get_telemetry(State(state): State<SharedStudioState>) -> impl IntoResponse {
    let mut g = state.lock();
    g.tick();
    Json(g.snapshot())
}

async fn get_decompiled_report(State(state): State<SharedStudioState>) -> impl IntoResponse {
    let g = state.lock();
    Json(g.gsr_report.clone())
}

async fn get_preview_jpeg(State(state): State<SharedStudioState>) -> impl IntoResponse {
    let (w, h, rgb) = {
        let mut g = state.lock();
        g.render_web_preview_rgb()
    };

    let mut jpeg_buf = Vec::with_capacity((w * h) as usize / 2);
    let mut enc = JpegEncoder::new_with_quality(&mut jpeg_buf, 82);
    if enc.encode(&rgb, w, h, ColorType::Rgb8.into()).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Vec::new()).into_response();
    }

    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store, no-cache"),
        ],
        jpeg_buf,
    )
        .into_response()
}

async fn post_cursor_interact(
    State(state): State<SharedStudioState>,
    Json(payload): Json<CursorInteractPayload>,
) -> impl IntoResponse {
    let mut g = state.lock();
    g.set_interactive_cursor(
        payload.x,
        payload.y,
        payload.left_click,
        payload.right_click,
    );
    g.tick();
    Json(g.snapshot())
}

async fn post_manual_zoom(
    State(state): State<SharedStudioState>,
    Json(payload): Json<ManualZoomPayload>,
) -> impl IntoResponse {
    let mut g = state.lock();
    let now = g.now_ms();
    g.zoom_engine.trigger_manual_zoom(
        payload.x,
        payload.y,
        payload.zoom,
        now,
        payload.hold_ms.unwrap_or(2400),
    );
    g.cursor_tracker.push_keystroke(
        format!("Zoom {:.1}x @ ({:.2}, {:.2})", payload.zoom, payload.x, payload.y),
        now,
    );
    g.tick();
    Json(g.snapshot())
}

async fn post_settings(
    State(state): State<SharedStudioState>,
    Json(p): Json<SettingsUpdatePayload>,
) -> impl IntoResponse {
    let mut g = state.lock();
    if let Some(v) = p.auto_zoom_enabled {
        g.zoom_cfg.enabled = v;
    }
    if let Some(ref mode) = p.zoom_mode {
        g.zoom_cfg.mode = match mode.as_str() {
            "SmartCursorFollow" => AutoZoomMode::SmartCursorFollow,
            "TimelineKeyframes" => AutoZoomMode::TimelineKeyframes,
            "Manual" => AutoZoomMode::Manual,
            _ => AutoZoomMode::ClickToZoom,
        };
    }
    if let Some(v) = p.target_zoom {
        g.zoom_cfg.target_zoom = v.clamp(1.1, 4.0);
    }
    if let Some(v) = p.spring_stiffness {
        g.zoom_cfg.spring_stiffness = v.clamp(20.0, 300.0);
    }
    if let Some(v) = p.hold_duration_ms {
        g.zoom_cfg.hold_duration_ms = v.clamp(300, 8000);
    }
    if let Some(v) = p.smoothing_enabled {
        g.cursor_cfg.smoothing_enabled = v;
    }
    if let Some(v) = p.click_ripples {
        g.cursor_cfg.click_ripples = v;
    }
    if let Some(v) = p.motion_trail {
        g.cursor_cfg.motion_trail = v;
    }
    if let Some(v) = p.spotlight_enabled {
        g.cursor_cfg.spotlight_enabled = v;
    }
    if let Some(v) = p.show_keystrokes {
        g.cursor_cfg.show_keystrokes = v;
    }
    if let Some(ref bg) = p.backdrop {
        g.style_cfg.backdrop = match bg.as_str() {
            "CyberOcean" => BackdropTheme::CyberOcean,
            "SunsetEmber" => BackdropTheme::SunsetEmber,
            "PureObsidian" => BackdropTheme::PureObsidian,
            _ => BackdropTheme::MidnightAurora,
        };
    }
    if let Some(v) = p.padding {
        g.style_cfg.padding = v.clamp(0.0, 0.15);
    }
    if let Some(ref enc) = p.encoder {
        g.encoder_backend = match enc.as_str() {
            "hevc_nvenc" => HardwareEncoderBackend::NvidiaNvencHevc,
            "av1_nvenc" => HardwareEncoderBackend::NvidiaNvencAv1,
            "h264_vaapi" => HardwareEncoderBackend::AmdVaapiH264,
            "libx264" => HardwareEncoderBackend::SoftwareX264Fast,
            _ => HardwareEncoderBackend::NvidiaNvencH264,
        };
    }
    Json(g.snapshot())
}

async fn post_toggle_record(State(state): State<SharedStudioState>) -> impl IntoResponse {
    let job = {
        let mut g = state.lock();
        let job = g.take_recording();
        if job.is_none() {
            let _ = g.start_recording();
        }
        job
    };
    // Finalize and render off the async runtime and outside the state lock,
    // so the studio keeps serving while the file is written.
    if let Some(job) = job {
        let st = state.clone();
        tokio::task::spawn_blocking(move || {
            match job.complete(&std::sync::atomic::AtomicU32::new(0)) {
                Ok(path) => st.lock().last_saved_file = Some(path.display().to_string()),
                Err(err) => eprintln!("[HyperGSR] Could not save the recording: {err:#}"),
            }
        });
    }
    Json(state.lock().snapshot())
}

async fn post_export_showcase(State(state): State<SharedStudioState>) -> impl IntoResponse {
    let mut g = state.lock();
    let _ = g.render_instant_demo_mp4();
    Json(g.snapshot())
}

const STUDIO_WEB_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8" />
<title>HyperGSR Studio — Rust GPU Screen Recorder & Auto-Zoom Director</title>
<style>
  :root {
    --bg: #0b0e17;
    --panel: #131826;
    --border: #222b40;
    --accent: #38bdf8;
    --danger: #ef4444;
    --success: #22c55e;
    --text: #e2e8f0;
    --muted: #94a3b8;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    background: var(--bg);
    color: var(--text);
    font-family: Inter, system-ui, -apple-system, sans-serif;
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 20px;
    background: #0f1420;
    border-bottom: 1px solid var(--border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
    font-weight: 700;
    font-size: 16px;
  }
  .badge {
    background: #1e293b;
    color: var(--accent);
    padding: 4px 10px;
    border-radius: 6px;
    font-size: 12px;
    font-family: monospace;
  }
  .actions { display: flex; gap: 10px; align-items: center; }
  button {
    background: #1e293b;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 8px 14px;
    border-radius: 8px;
    font-weight: 600;
    cursor: pointer;
    font-size: 13px;
    transition: 0.15s;
  }
  button:hover { border-color: var(--accent); }
  button.primary { background: #0284c7; border-color: #38bdf8; color: #fff; }
  button.danger { background: var(--danger); border-color: #f87171; color: #fff; }
  main {
    display: grid;
    grid-template-columns: 1fr 410px;
    gap: 14px;
    padding: 14px;
    flex: 1;
    overflow: hidden;
  }
  .stage-col {
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow: hidden;
  }
  .preview-wrap {
    position: relative;
    flex: 1;
    background: #07090f;
    border: 1px solid var(--border);
    border-radius: 12px;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    cursor: crosshair;
  }
  #previewImg {
    width: 100%;
    height: 100%;
    object-fit: contain;
    user-select: none;
    pointer-events: none;
  }
  .hud-overlay {
    position: absolute;
    top: 12px;
    left: 12px;
    display: flex;
    gap: 8px;
    pointer-events: none;
  }
  .timeline-card {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px;
  }
  .timeline-track {
    position: relative;
    height: 36px;
    background: #0b0e17;
    border: 1px solid var(--border);
    border-radius: 8px;
    margin-top: 8px;
    display: flex;
    align-items: center;
    padding: 0 10px;
    gap: 12px;
  }
  .kf-pill {
    background: rgba(56, 189, 248, 0.18);
    border: 1px solid var(--accent);
    color: var(--accent);
    padding: 4px 10px;
    border-radius: 6px;
    font-size: 12px;
    cursor: pointer;
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    padding-right: 4px;
  }
  .card {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card h3 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent);
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 13px;
  }
  select, input[type="range"] {
    background: #0b0e17;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 5px 8px;
    border-radius: 6px;
  }
</style>
</head>
<body>
<header>
  <div class="brand">
    <span>⚡ HyperGSR Studio</span>
    <span class="badge" id="statusBadge">Initializing...</span>
    <span class="badge" id="zoomBadge">Zoom: 1.00x</span>
  </div>
  <div class="actions">
    <button id="recBtn" class="danger" onclick="toggleRecord()">● Start Live Recording</button>
    <button class="primary" onclick="exportShowcase()">⚡ Export 60fps Showcase MP4</button>
  </div>
</header>
<main>
  <div class="stage-col">
    <div class="preview-wrap" id="stage" onmousemove="onStageMove(event)" onmousedown="onStageClick(event)">
      <img id="previewImg" src="/api/frame.jpg" alt="Live GPU Compositor Preview" />
      <div class="hud-overlay">
        <span class="badge" id="cursorBadge">Cursor: (0.50, 0.50)</span>
        <span class="badge">Click anywhere on canvas to trigger Spring Auto-Zoom</span>
      </div>
    </div>
    <div class="timeline-card">
      <div class="row">
        <strong>Auto-Zoom Director Presets & Timeline Keyframes</strong>
        <div style="display:flex; gap:8px;">
          <button onclick="triggerZoom(0.28, 0.34, 2.25)">🔍 Code Editor (2.25x)</button>
          <button onclick="triggerZoom(0.75, 0.28, 2.40)">🔍 GPU Telemetry (2.40x)</button>
          <button onclick="triggerZoom(0.68, 0.72, 2.15)">🔍 Timeline Bar (2.15x)</button>
          <button onclick="triggerZoom(0.50, 0.50, 1.00)">🎯 Reset 1.0x</button>
        </div>
      </div>
      <div class="timeline-track" id="kfTrack"></div>
    </div>
  </div>
  <div class="sidebar">
    <div class="card">
      <h3>1. Automatic Zoom-In Physics</h3>
      <label class="row"><span>Enable Auto-Zoom</span><input type="checkbox" id="chkZoom" checked onchange="pushSettings()"/></label>
      <label class="row"><span>Director Mode</span>
        <select id="selMode" onchange="pushSettings()">
          <option value="ClickToZoom">Click-to-Zoom (Spring Target)</option>
          <option value="SmartCursorFollow">Smart Dwell & Velocity Follow</option>
          <option value="TimelineKeyframes">Automated Timeline Keyframes</option>
          <option value="Manual">Manual Camera Lock</option>
        </select>
      </label>
      <label class="row"><span>Target Zoom (<span id="lblZoomVal">2.15</span>x)</span>
        <input type="range" id="rngZoom" min="1.2" max="3.5" step="0.05" value="2.15" oninput="pushSettings()"/>
      </label>
      <label class="row"><span>Spring Stiffness k (<span id="lblStiff">95</span>)</span>
        <input type="range" id="rngStiff" min="30" max="240" step="5" value="95" oninput="pushSettings()"/>
      </label>
    </div>
    <div class="card">
      <h3>2. Cursor Smoothing & FX</h3>
      <label class="row"><span>Critically-Damped Smoothing</span><input type="checkbox" id="chkSmooth" checked onchange="pushSettings()"/></label>
      <label class="row"><span>Click Ripple Waves</span><input type="checkbox" id="chkRipple" checked onchange="pushSettings()"/></label>
      <label class="row"><span>Velocity Motion Trail</span><input type="checkbox" id="chkTrail" checked onchange="pushSettings()"/></label>
      <label class="row"><span>Cursor Spotlight Dimming</span><input type="checkbox" id="chkSpot" onchange="pushSettings()"/></label>
    </div>
    <div class="card">
      <h3>3. Hardware Encoder & Studio Frame</h3>
      <label class="row"><span>Hardware Encoder</span>
        <select id="selEnc" onchange="pushSettings()">
          <option value="h264_nvenc">NVIDIA NVENC H.264 (RTX 3050)</option>
          <option value="hevc_nvenc">NVIDIA NVENC HEVC</option>
          <option value="av1_nvenc">NVIDIA NVENC AV1</option>
          <option value="h264_vaapi">AMD Radeon VAAPI H.264</option>
          <option value="libx264">Software x264 Ultrafast</option>
        </select>
      </label>
      <label class="row"><span>Backdrop Theme</span>
        <select id="selBg" onchange="pushSettings()">
          <option value="MidnightAurora">Midnight Aurora</option>
          <option value="CyberOcean">Cyber Ocean</option>
          <option value="SunsetEmber">Sunset Ember</option>
          <option value="PureObsidian">Pure Obsidian</option>
        </select>
      </label>
    </div>
    <div class="card">
      <h3>4. Decompiled GSR v6.1.4 Inspector</h3>
      <div id="reaSummary" style="font-size:12px; color:var(--muted); line-height:1.45;">Loading binary analysis...</div>
    </div>
  </div>
</main>
<script>
async function refreshLoop() {
  const img = document.getElementById('previewImg');
  img.src = '/api/frame.jpg?t=' + Date.now();
  try {
    const r = await fetch('/api/telemetry');
    const d = await r.json();
    document.getElementById('zoomBadge').textContent = `Zoom: ${d.viewport.zoom.toFixed(2)}x @ (${d.viewport.center_x.toFixed(2)}, ${d.viewport.center_y.toFixed(2)})`;
    document.getElementById('cursorBadge').textContent = `Smooth Cursor: (${d.cursor.smooth_x.toFixed(2)}, ${d.cursor.smooth_y.toFixed(2)})`;
    const recBtn = document.getElementById('recBtn');
    if (d.is_recording) {
      recBtn.textContent = `■ Stop & Save (${d.recorded_frames}f)`;
      document.getElementById('statusBadge').textContent = `● REC [${d.active_encoder}]`;
    } else {
      recBtn.textContent = '● Start Live Recording';
      document.getElementById('statusBadge').textContent = d.last_saved_file ? `Saved: ${d.last_saved_file}` : `READY [${d.active_encoder}]`;
    }
    const kfTrack = document.getElementById('kfTrack');
    kfTrack.innerHTML = d.keyframes.map(k =>
      `<span class="kf-pill" onclick="triggerZoom(${k.target_x}, ${k.target_y}, ${k.zoom_scale})">${(k.start_ms/1000).toFixed(1)}s: ${k.label} (${k.zoom_scale}x)</span>`
    ).join('');
  } catch (e) {}
}
setInterval(refreshLoop, 80);

function stageCoords(ev) {
  const rect = document.getElementById('stage').getBoundingClientRect();
  const x = Math.max(0, Math.min(1, (ev.clientX - rect.left) / rect.width));
  const y = Math.max(0, Math.min(1, (ev.clientY - rect.top) / rect.height));
  return { x, y };
}
function onStageMove(ev) {
  const { x, y } = stageCoords(ev);
  fetch('/api/interact', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({ x, y, left_click: false, right_click: false })
  });
}
function onStageClick(ev) {
  const { x, y } = stageCoords(ev);
  fetch('/api/interact', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({ x, y, left_click: ev.button === 0, right_click: ev.button === 2 })
  });
}
function triggerZoom(x, y, zoom) {
  fetch('/api/manual-zoom', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({ x, y, zoom, hold_ms: 2500 })
  });
}
function pushSettings() {
  const z = parseFloat(document.getElementById('rngZoom').value);
  const s = parseFloat(document.getElementById('rngStiff').value);
  document.getElementById('lblZoomVal').textContent = z.toFixed(2);
  document.getElementById('lblStiff').textContent = s.toFixed(0);
  fetch('/api/settings', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({
      auto_zoom_enabled: document.getElementById('chkZoom').checked,
      zoom_mode: document.getElementById('selMode').value,
      target_zoom: z,
      spring_stiffness: s,
      smoothing_enabled: document.getElementById('chkSmooth').checked,
      click_ripples: document.getElementById('chkRipple').checked,
      motion_trail: document.getElementById('chkTrail').checked,
      spotlight_enabled: document.getElementById('chkSpot').checked,
      encoder: document.getElementById('selEnc').value,
      backdrop: document.getElementById('selBg').value
    })
  });
}
async function toggleRecord() {
  await fetch('/api/record/toggle', { method: 'POST' });
}
async function exportShowcase() {
  document.getElementById('statusBadge').textContent = 'Rendering 60fps Showcase MP4...';
  await fetch('/api/export-showcase', { method: 'POST' });
}
fetch('/api/decompiled').then(r => r.json()).then(d => {
  document.getElementById('reaSummary').innerHTML =
    `<b>Binary:</b> gpu-screen-recorder v${d.gsr_version}<br/>` +
    `<b>GPUs:</b> ${d.gpu_vendor} + ${d.nvidia_gpu_name}<br/>` +
    `<b>Bug Found:</b> Hardcoded /var/lib/flatpak bridge path fixed in HyperGSR.<br/>` +
    `<b>Upgrades:</b> O(1) Cursor Ring Buffer + 2D Spring Smoothing + Auto-Zoom.`;
});
</script>
</body>
</html>
"#;
