# Reverse Engineering Analysis (REA): `com.dec05eba.gpu_screen_recorder` (v6.1.4)

## 1. Installed Binary Topology on This System

The existing GPU screen recorder on this machine (`Pop!_OS`, Linux `7.1.5-76070105-generic`, COSMIC Wayland `wayland-1` + XWayland `:1`) is installed as a user Flatpak at:
`~/.local/share/flatpak/app/com.dec05eba.gpu_screen_recorder/x86_64/stable/6f1120bd2356fcd67543acc4556e329d6b36e525decdb9adae329b821dded04e/files/bin/`

| Binary | ELF Type & Linkage | Role Discovered via Symbol & String Analysis |
| :--- | :--- | :--- |
| `gpu-screen-recorder` | 64-bit PIE, dynamically linked (`libva`, `libva-drm`, `libpipewire-0.3`, `libpulse`, `libX11`, `libXcomposite`, `libXfixes`, `libXdamage`, `libwayland-egl`), static `libdrm` + `libavcodec`/`libavformat` | Core capture, OpenGL ES 3.0 color conversion (`RGB -> NV12/P010`), hardware encoding (`NVENC`/`VAAPI`/`Vulkan`), and circular replay buffer engine. |
| `gsr-kms-server` | 64-bit statically linked ELF | Privileged KMS daemon that calls `drmSetClientCap(DRM_CLIENT_CAP_UNIVERSAL_PLANES)` and `DRM_CLIENT_CAP_ATOMIC`, enumerates DRM planes (`drmModeGetPlaneResources`), exports framebuffer handles to DMA-BUF FDs via `drmPrimeHandleToFD`, and passes them over a Unix domain socket (`%s/.gsr-kms-socket-%s`). |
| `gpu-screen-recorder-gtk` | 64-bit PIE (`libgtk-3.so.0`, `libayatana-appindicator3.so.1`) | Simple GTK3 wrapper that shells out to `gpu-screen-recorder --info`, `--list-audio-devices`, and spawns CLI recording processes. |
| `gsr-ui` | 64-bit PIE | X11/XWayland ShadowPlay-style fullscreen overlay launched via global hotkeys. |
| `gsr-wayland-bridge` / `kms-server-proxy` | 64-bit statically linked helpers | Sandbox escape helpers for Flatpak. |

### Bug #1 Discovered During Binary Analysis: Hardcoded `/var/lib/flatpak` Path in `.rodata`
At `.rodata` (`WaylandHostBridge`, `0xda809`–`0xda86c`) in `gpu-screen-recorder`, the binary hardcodes system-wide Flatpak paths:
- `/var/lib/flatpak/app/com.dec05eba.gpu_screen_recorder/current/active/files/bin/gsr-wayland-bridge`
- `/var/lib/flatpak/app/com.dec05eba.gpu_screen_recorder/current/active/files/bin/kms-server-proxy`

Because the package is installed in `--user` Flatpak storage (`~/.local/share/flatpak/...`), invoking `gpu-screen-recorder` on Wayland fails with:
```text
Portal call failed: Failed to start command: Failed to execute child process “/var/lib/flatpak/app/com.dec05eba.gpu_screen_recorder/current/active/files/bin/gsr-wayland-bridge” (No such file or directory)
gsr error: WaylandHostBridge: gsr-wayland-bridge did not return a wayland fd
```
**Resolution in `HyperGSR`**: `bin/flatpak-spawn` intercepts `flatpak-spawn --host` calls from inside the sandbox and rewrites `/var/lib/flatpak/...` to `$HOME/.local/share/flatpak/...` before invoking `/usr/bin/flatpak-spawn`.

---

## 2. Decompiled Internal Pipeline of `gpu-screen-recorder`

### A. Capture Subsystems (`gsr_capture_*`)
1. **KMS Direct Plane Capture (`gsr_capture_kms`)**:
   - Connects to `gsr-kms-server` via Unix socket.
   - Imports the DRM framebuffer plane FD into EGL using `eglCreateImageKHR` (`EGL_LINUX_DMA_BUF_EXT`), binding it to `GL_TEXTURE_EXTERNAL_OES` (or `GL_TEXTURE_2D` fallback via `gsr_capture_kms_create_egl_image_with_fallback`).
2. **Wayland Desktop Portal + PipeWire (`gsr_capture_portal`)**:
   - Talks to `org.freedesktop.portal.ScreenCast` over `libdbus-1`.
   - Dequeues buffers via `pw_stream_dequeue_buffer@plt` (`0x55440`), negotiating GPU DMA-BUF modifiers (`0x20000044051ba01` on AMD Radeon `card1`) queried via `eglQueryDmaBufModifiersEXT`.
3. **NVIDIA NvFBC (`gsr_capture_nvfbc`) & XComposite (`gsr_capture_xcomposite`)**:
   - Dynamically `dlopen`s `libnvidia-fbc.so.1` on X11 or uses `XCompositeRedirectWindow` (`0x56270`) + `XDamageSubtract` to track dirty regions.

### B. Decompiled Cursor Handling (`0xd055c` – `0xd064d`) — Why It Lacks Cursor Auto-Zoom
Disassembly of `gsr_cursor` (`0xd055c`) reveals why `gpu-screen-recorder` has no cursor tracking effects or automatic zoom:
```asm
   d0567: call   56940 <XFixesSelectCursorInput@plt>
   d056c: mov    0x8(%rbx),%rdi
   d0570: call   56280 <XFixesGetCursorImage@plt>
   d0575: lea    0x31(%rbx),%rdx
   d057f: call   d0190 <gsr_cursor_upload_texture>
```
And the extracted GLSL ES 3.0 vertex shader used to composite both screen and cursor quads:
```glsl
#version 300 es
in vec2 pos;
in vec2 texcoords;
out vec2 texcoords_out;
uniform vec2 offset;
uniform float rotation;
uniform mat2 rotation_matrix;
void main()
{
  texcoords_out = vec2(texcoords.x - 0.5, texcoords.y - 0.5) * rotation_matrix + vec2(0.5, 0.5);
  gl_Position = (vec4(offset.x, offset.y, 0.0, 0.0) + vec4(pos.x, pos.y, 0.0, 1.0))
                * vec4(0.5, 0.5, 1.0, 1.0) - vec4(0.5, 0.5, 0.0, 0.0);
}
```
**Limitations identified in `gpu-screen-recorder`**:
1. **XWayland Blindness on Wayland Desktops**: `XQueryPointer` / `XFixesGetCursorImage` never updates when the cursor hovers over native Wayland windows.
2. **No Camera Viewport Matrix**: The vertex shader only translates by `offset` and rotates around `(0.5, 0.5)`. There is no zoom scale, focal target, or spring-damper camera interpolation.
3. **No Cursor Telemetry Ring Buffer**: Cursor coordinates are read and immediately discarded after drawing the quad. Velocity, acceleration, dwell clusters, and click timestamps are never recorded.

### C. Decompiled Color Conversion & Hardware Encoding (`0xb1c43` + Fragment Shaders)
Instead of downloading frames to CPU RAM (`glReadPixels`), `gpu-screen-recorder` allocates hardware surfaces (`NV12` or `P010`) via `av_hwframe_get_buffer`, exports them via:
- **VAAPI (`0xb1c53`)**: `vaExportSurfaceHandle(..., VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2, ...)` followed by `vaSyncSurface` (`0xb1c66`)
- **NVENC**: `cuGraphicsGLRegisterImage` (`libcuda.so.1`)
- **Vulkan**: `vkGetMemoryFdKHR`

It binds plane 0 (`Y`, `R8`) and plane 1 (`UV`, `RG88` at half resolution) as two OpenGL framebuffers and runs two fragment passes:
```glsl
// Y pass
vec4 pixel = texture(tex1, texcoords_out);
FragColor.x = (RGBtoYUV * vec4(pixel.rgb, 1.0)).x * scale;

// UV pass
vec4 pixel = texture(tex1, texcoords_out);
FragColor.xy = (RGBtoYUV * vec4(pixel.rgb, 1.0)).yz;
```

---

## 3. How `gpu-recorder-rs` (HyperGSR Dynamic Island) Upgrades This Architecture

1. **Real-Time DRM Hardware Cursor Plane Polling (`DrmCursorReader` in `src/cursor.rs`)**:
   - Opens `/dev/dri/card1` via `libdrm.so.2` (`drmSetClientCap` for `DRM_CLIENT_CAP_UNIVERSAL_PLANES=2` and `DRM_CLIENT_CAP_ATOMIC=3`) without root privileges.
   - Locates Hardware Cursor Plane `#92` (`type == DRM_PLANE_TYPE_CURSOR`) and reads `CRTC_X`, `CRTC_Y`, and `FB_ID` at 60 Hz across all native Wayland and X11 windows.
   - Applies a **Critically Damped 2D Spring Filter** (`smooth_x`, `smooth_y`) in a lock-free O(1) ring buffer.
2. **Spring-Damper Auto-Zoom Camera Engine (`src/zoom_engine.rs`)**:
   - Implements a physically simulated virtual camera `(cam_x, cam_y, zoom)` governed by $\ddot{x} = -k(x - x_{\text{target}}) - 2\sqrt{k}\dot{x}$.
   - Supports **Smart Follow** (auto-zooms on cursor dwell/interaction and pulls out to `1.0×` on fast travel), **Click Lock**, and **Cinema Tour** with automatic viewport edge-clamping.
3. **Native Wayland Layer-Shell Dynamic Island (`src/gui.rs`)**:
   - Docks at the top center notch via `zwlr_layer_shell_v1` (`libgtk4-layer-shell.so.0`) with 32-bit RGBA alpha-transparent rounded corners, Bento studio cards, a live 16:9 Cairo cursor-camera viewfinder, and instant **Obsidian Dark** / **Pearl Light** theme switching.
4. **Zero-Copy Capture + Hardware NVENC Auto-Zoom Pass (`src/encoder.rs`)**:
   - Captures `1920x1080 @ 60 FPS` desktop video via PipeWire DMA-BUF + `h264_vaapi` while logging 60 Hz zoom telemetry, then runs a hardware `h264_nvenc` pass on stop to bake the smooth spring-zoom trajectory into the final MP4.
