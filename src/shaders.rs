/// Original decompiled GLSL ES 3.0 vertex shader extracted from `gpu-screen-recorder` (v6.1.4)
/// at `.rodata` offset in `~/.local/share/flatpak/app/com.dec05eba.gpu_screen_recorder/.../bin/gpu-screen-recorder`.
pub const DECOMPILED_GSR_VERTEX_SHADER: &str = r#"#version 300 es
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
"#;

/// Original decompiled GLSL ES 3.0 NV12 Y/UV color conversion shader from `gpu-screen-recorder`.
pub const DECOMPILED_GSR_NV12_SHADER: &str = r#"#version 300 es
#extension GL_OES_EGL_image_external : enable
#extension GL_OES_EGL_image_external_essl3 : require
precision highp float;
in vec2 texcoords_out;
uniform samplerExternalOES tex1;
out vec4 FragColor;
void main()
{
  vec4 pixel = texture(tex1, texcoords_out);
  FragColor.x = (RGBtoYUV * vec4(pixel.rgb, 1.0)).x * scale;
  FragColor.xy = (RGBtoYUV * vec4(pixel.rgb, 1.0)).yz;
}
"#;

/// Upgraded HyperGSR GLSL ES 3.0 Vertex Shader with Spring-Damper Virtual Camera UV Transform.
pub const HYPERGSR_VERTEX_SHADER_ES3: &str = r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;

uniform vec4 u_camera_rect; // (uv_min_x, uv_min_y, uv_w, uv_h)
out vec2 v_canvas_uv;
out vec2 v_zoomed_uv;

void main() {
    v_canvas_uv = a_uv;
    v_zoomed_uv = u_camera_rect.xy + a_uv * u_camera_rect.zw;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

/// Upgraded HyperGSR GLSL ES 3.0 Fragment Shader:
/// Single-pass GPU composition of:
/// 1. Gradient Studio Backdrop + Rounded Desktop Bezel + Drop Shadow
/// 2. Spring-Zoomed Screen Texture with Directional Pan Motion Blur
/// 3. Cursor Spotlight Dimming Vignette
/// 4. Expanding Click Ripple Waves & Smoothed Cursor Indicator
pub const HYPERGSR_FRAGMENT_SHADER_ES3: &str = r#"#version 300 es
precision highp float;

in vec2 v_canvas_uv;
in vec2 v_zoomed_uv;
out vec4 FragColor;

uniform sampler2D u_screen_tex;
uniform vec4 u_camera_rect;     // (uv_min_x, uv_min_y, uv_w, uv_h)
uniform vec2 u_pan_velocity;    // (pan_vel_x, pan_vel_y)
uniform vec2 u_cursor_pos;      // (smooth_x, smooth_y) in screen UV space
uniform float u_cursor_scale;
uniform float u_frame_padding;  // e.g. 0.05
uniform float u_corner_radius;  // e.g. 0.025
uniform float u_spotlight_dim;  // 0.0 = off, >0.0 = dim outside cursor halo
uniform vec4 u_ripple_0;        // (x, y, progress 0..1, active 0/1)

float sdRoundedBox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    // 1. Studio gradient background
    vec3 bg_top = vec3(0.07, 0.09, 0.16);
    vec3 bg_bot = vec3(0.16, 0.11, 0.26);
    vec3 bg = mix(bg_top, bg_bot, v_canvas_uv.y + 0.25 * v_canvas_uv.x);

    // 2. Rounded desktop frame SDF
    vec2 half_ext = vec2(0.5 - u_frame_padding);
    float sdf = sdRoundedBox(v_canvas_uv - vec2(0.5), half_ext, u_corner_radius);
    if (sdf > 0.0) {
        float shadow = exp(-sdf * 42.0) * 0.55;
        FragColor = vec4(bg * (1.0 - shadow), 1.0);
        return;
    }

    // Map inner frame UV to zoomed screen UV
    vec2 inner_uv = (v_canvas_uv - vec2(u_frame_padding)) / (1.0 - 2.0 * u_frame_padding);
    vec2 sample_uv = u_camera_rect.xy + inner_uv * u_camera_rect.zw;

    // 3. 3-tap directional motion blur during fast zoom/pan transitions
    vec2 blur_step = u_pan_velocity * 0.008;
    vec3 col = 0.5 * texture(u_screen_tex, sample_uv).rgb
             + 0.25 * texture(u_screen_tex, clamp(sample_uv - blur_step, 0.0, 1.0)).rgb
             + 0.25 * texture(u_screen_tex, clamp(sample_uv + blur_step, 0.0, 1.0)).rgb;

    // 4. Cursor spotlight halo
    float dist_to_cursor = length((sample_uv - u_cursor_pos) * vec2(1.777, 1.0));
    if (u_spotlight_dim > 0.01) {
        float spot = smoothstep(0.14, 0.06, dist_to_cursor);
        col *= mix(1.0 - u_spotlight_dim, 1.0, spot);
    }

    // 5. Click ripple wave
    if (u_ripple_0.w > 0.5) {
        float r_dist = length((sample_uv - u_ripple_0.xy) * vec2(1.777, 1.0));
        float target_r = u_ripple_0.z * 0.085;
        float ring = exp(-abs(r_dist - target_r) * 260.0) * (1.0 - u_ripple_0.z);
        col = mix(col, vec3(0.24, 0.78, 1.0), ring * 0.85);
    }

    FragColor = vec4(col, 1.0);
}
"#;
