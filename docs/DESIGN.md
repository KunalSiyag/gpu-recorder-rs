# HyperGSR (`gpu-recorder-rs`) Design System

HyperGSR is a native Rust GPU screen recorder for Linux Wayland and X11 with
rootless DRM Hardware Cursor Plane `#92` tracking, a 240 Hz critically-damped
spring auto-zoom camera, self-registering zoom gesture combinations, and a
Wayland Layer-Shell (`zwlr_layer_shell_v1`) Dynamic Island HUD.

This file is the source of truth for the public marketing site and interactive
workbench (`docs/index.html`) as well as repository visual assets (`docs/og-banner.png`,
`assets/hero-banner.svg`, `assets/benchmark-card.svg`). If a choice is not covered
here, follow the closest rule; do not invent a new color, font size, border radius,
or shadow.

## Brief

- **What:** Open-source developer tool landing page, interactive 240 Hz spring
  camera workbench, real 60 FPS video/screenshot showcase, architecture matrix,
  and contributor onboarding hub.
- **Who:** Linux systems engineers, Wayland compositor users (COSMIC, Hyprland,
  GNOME, KDE Plasma, Sway), developer content creators, and Rust contributors.
- **Goal:** Prove technical credibility in 3 seconds, let visitors test the
  240 Hz critically-damped spring camera directly in the browser, and convert
  visitors into GitHub stars, forks, binary downloads, and pull requests.
- **Tone:** Precise, technical, calm, instrument-grade. Built like a systems
  profiler and camera optical bench rather than a generic SaaS template.
- **Main risk:** Looking like AI-generated marketing slop (indigo/violet gradients,
  emoji bullet walls, three-card feature grids, dark-only forced theme, or invented
  claims). Every number on the page comes from measured system telemetry (`56 MB`
  RSS, `<1.5%` CPU, `DRM Plane #92` at `60 Hz`, `240 Hz` ODE solver, `NVENC P4`).
- **Remember:** The hollow spectral aperture lattice in the hero, the live 240 Hz
  interactive spring camera workbench, and the dense Linear-style contributor
  issue rows.

## Research (Refero styles)

Styles fetched and analyzed from `styles.refero.design`, paired with the
Refero design skill (`referodesign/refero_skill`) and `ossium.in`:

| Source | Took from it |
|---|---|
| **Graphite** (`style/fd36bae9-dbf4-4b0f-a48b-e8c6f92edca3`) — *Luminous terminal lattice* | **Primary.** `#0a0a0a` Obsidian Canvas, `#000000` True Black code/media backdrops, `#171717` Charcoal Surface, `#262626` Graphite Edge 1px hairlines, `#464646` Steel Border on selected controls, `#fafafa` Cloud White / `#a1a1a1` Fog / `#808080` Ash text hierarchy, monochrome Pale Conversion Button (`#e5e5e5` fill, `#171717` text, `8px` radius, inset top highlight), Charcoal Secondary Button (`#262626` fill, `#464646` border), Signal Orange (`#ff8833`) strictly for inline forward links and directional arrows, Solar Spectrum (`#ffb931 → #ffffff → #2e73fc`) strictly for hollow technical linework, and `10px` maximum radius on panels and product demo frames (`rgba(101,129,128,0.15) 0 0 2px 2px inset`). |
| **Linear** (`style/90ce5883-bb24-4466-93f7-801cd617b0d1`) — *Midnight precision instrument* | Compact 4px base grid, dense hairline-separated comparison and issue rows, `tabular-nums` on all metrics and telemetry counters, `4px`-radius monospace tags for file paths (`src/cursor.rs`), and low font weights (`400`–`600`, never `800`/`900`). Not taken: its acid-lime primary button or iris-violet badge fills. |
| **ossium.in** | Dashed blueprint frames (`1px dashed var(--frame-line)`) with `+` corner markers around marketing blocks, hairline-divided cells inside frames instead of floating card grids, centered pill section labels with fading side rules, and interactive feature tabs under the product demo frame. |
| **Dovetail** | Faint `48px` blueprint grid (`.grid-bg`) in the hero and workbench canvas, masked softly toward the edges. |
| **Resend** (`style/0d914ef0-fa84-4c60-a9aa-cef0b5eb6e5d`) | `6px` status dot + uppercase monospace label (`11px`, `0.07em` tracking) for hardware and pipeline telemetry states. |
| **Vercel** (`style/f24daf3a-d43f-4dec-85a9-8ac1d5148a03`) | `Geist Sans` + `Geist Mono` typography pairing, paper-first `#fafafa` light theme canvas, and terminal command bar with one-click copy feedback. |
| **Refero skill** (`referodesign/refero_skill`) | `anti-ai-slop.md`: zero indigo/violet, system-first theme with explicit `Light / Dark / System` control, no card-grid defaults, no emoji icons, strict token role discipline. `typography.md`: ≤8 font sizes, `0.07em` tracking on ALL CAPS, `-0.03em` on display headings, `text-wrap: balance` on headings, `tabular-nums` on numbers, curly quotes and proper ellipses (`…`). `craft-details.md`: `:focus-visible` rings, explicit `width`/`height` on images, semantic buttons/links, `scroll-margin-top` on anchors. `motion.md`: explicit properties on transitions (never `transition: all`), `120ms` fast / `200ms` default tokens, and full `prefers-reduced-motion` support. `copywriting.md`: facts beat adjectives, verb + object button labels, zero hype words. |

## Reference lock

- **Primary:** Graphite (`fd36bae9-dbf4-4b0f-a48b-e8c6f92edca3`).
- **Preserve:** Near-black `#0a0a0a` canvas (in dark mode) and `#fafafa` paper canvas
  (in light mode) with a quiet neutral ladder; `1px` `#262626` / `#ebebeb` hairline
  borders instead of heavy drop shadows; monochrome Pale Primary button and Charcoal
  Secondary button; `Geist Sans` reads, `Geist Mono` stamps (eyebrows, telemetry,
  CLI commands, Rust symbols); hollow rotated rounded-square aperture lattice with
  Graphite's Solar Spectrum stroke as the sole abstract ornament.
- **Borrow only:** Ossium dashed blueprint frames with `+` corner markers and
  centered section labels; Dovetail `48px` grid; Resend status dots; Linear dense
  comparison and issue rows.
- **Role rules:**
  - `--accent` (`#3fb950` dark / `#297a3a` light, terminal green) = live telemetry
    dots, verified `✓` capabilities in the comparison matrix, and the active spring
    phase readout. Never a button fill, card background, or syntax colour.
  - `--link-accent` (`#ff8833` dark / `#c2410c` light, Graphite Signal Orange) =
    forward inline links and their directional arrows (`Inspect src/cursor.rs ↗`,
    `Open Issue #1 ↗`, `View release notes →`). Prose links stay neutral.
  - `--spectrum` (`linear-gradient(90deg, #ffb931 15%, #ffffff 30%, #ffffff 40%, #2e73fc 50%)`) =
    hollow aperture linework in the hero and the 1px top signal rule on the interactive
    workbench frame. Never a surface fill, button fill, or text gradient.
  - Conversion buttons remain strictly monochrome (`--primary` pale fill `#e5e5e5`
    with `#171717` text in dark mode; `#171717` fill with `#ffffff` text in light mode).
- **Media:** Code-native and measured product evidence only:
  1. Interactive 240 Hz critically-damped spring camera simulator with live ODE
     telemetry (`X`, `Y`, `Zoom`, `Velocity`, `Phase`) and preset triggers.
  2. Real 1080p60 recorded MP4 showcase (`hypergsr-promo-1080p60.mp4`) and real
     Wayland Layer-Shell GTK4 screenshots (`island_dark_studio.png`,
     `island_light_studio.png`, `island_compact_bar.png`) inside Graphite's `10px`
     Product Demo Frame.
  3. Real Rust and C implementation snippets on `#000000` (`--code-bg`) surfaces.
- **Reject:** Indigo/violet (`#6366f1`, `#8b5cf6`, `#c084fc`), rainbow button fills,
  gradient headline text, emoji icons (`⭐`, `🍴`, `🦀`, `🎮`, `🎬`, `📊`), floating
  rounded card grids, `transition: all`, and unverified marketing adjectives.

## Decision ledger

| Decision | Source | Why |
|---|---|---|
| Theme follows `prefers-color-scheme` by default with an explicit `Light / Dark / System` segmented control in the navigation shell | Graphite + Refero `anti-ai-slop.md` (#3) | Respects the visitor's OS preference before first paint while preserving Graphite's Obsidian identity in dark mode and Vercel's crisp paper canvas in light mode. |
| `Geist Sans` + `Geist Mono` (`Inter` / `JetBrains Mono` fallback), weights `400`, `500`, `600` only | Vercel + Graphite + Refero `typography.md` | Graphite caps display headings at weight `500`–`600` with tight leading (`1.02`–`1.06`) and negative tracking (`-0.03em`); `Geist Mono` stamps hardware registers (`PLANE #92`, `CRTC_X`), CLI commands, and tabular metrics. |
| Pale primary button (`#e5e5e5` / `#171717`) and Charcoal secondary button (`#262626` / `#464646`), `8px` radius | Graphite (`fd36bae9`) | Graphite keeps conversion controls monochrome so color in the interface always carries technical meaning. |
| Forward links and directional arrows (`→`, `↗`) use Signal Orange (`#ff8833` dark / `#c2410c` light) | Graphite (`fd36bae9`) | Graphite's sole warm reading cue; guides the eye to source files, GitHub issues, and release artifacts without adding colored buttons. |
| Panels, media frames, inputs, and code blocks capped at `10px` radius; buttons at `8px`; tags/kbd at `4px` | Graphite + Linear | Matches Graphite's strict Do/Don't radius ceiling (`10px` max on frames and cards). |
| Architecture and FAQ blocks sit inside Ossium dashed blueprint frames (`1px dashed var(--frame-line)`) with `+` corner markers | `ossium.in` + Refero `anti-ai-slop.md` (#2: no card-grid defaults) | Divides technical modules with architectural hairlines instead of three-across rounded SaaS cards. |
| Comparison matrix and open contributor issues use Linear dense rows with `tabular-nums` | Linear (`90ce5883`) | Engineers scan aligned rows and tabular numbers (`56 MB` vs `650 MB`) faster than promotional cards. |
| All icons are inline `16px` monochrome SVGs (`1.5px` stroke, `currentColor`) or typographic symbols (`→`, `↗`, `✓`, `—`) | Refero `icons.md` + `anti-ai-slop.md` (#5: no emoji icons) | Eliminates emoji clutter and keeps stroke weight unified with `14px/500` UI labels. |
| Hero pairs a left-aligned 500-weight display statement and CLI installer with a hollow rotated rounded-square **Luminous Aperture Lattice** SVG | Graphite (`fd36bae9` asymmetric hero + Luminous Polygon Graphic) | Translates Graphite's hollow spectral geometry into HyperGSR's optical camera crop and DRM cursor plane reticle. |
| Product workbench combines the live 240 Hz interactive spring simulator and real 60 FPS video/screenshot tabs inside one framed module | Graphite (`Product Demo Frame` + `Demo Feature Tab`) | Lets visitors test the spring physics live and immediately inspect the real GTK4 Wayland Layer-Shell UI and recorded MP4 output in the same frame. |

## Tokens

All tokens live in `docs/index.html` `:root` (Light) and `[data-theme="dark"]` (Dark).
A synchronous `<head>` script resolves `localStorage.getItem('hypergsr-theme')`
or `window.matchMedia('(prefers-color-scheme: dark)')` before first paint so
there is zero theme flash.

### Color

| Token | Light (`:root`) | Dark (`[data-theme="dark"]`) | Role |
|---|---|---|---|
| `--bg` | `#fafafa` | `#0a0a0a` | Page canvas (Paper / Obsidian Canvas) |
| `--code-bg` | `#f3f3f3` | `#000000` | Terminal commands, Rust/C code surfaces, deep media backdrops (True Black) |
| `--surface` | `#ffffff` | `#111111` | Interactive panels, workbench shell, table surface |
| `--surface-2` | `#f5f5f5` | `#171717` | Announcement bar, elevated controls, row hover (Charcoal Surface) |
| `--surface-3` | `#ebebeb` | `#262626` | Secondary button fill, active chip background (Graphite Edge) |
| `--line` | `#e5e5e5` | `#262626` | 1px structural hairlines, row dividers, card borders |
| `--line-strong` | `#cccccc` | `#464646` | Selected control border, secondary button border (Steel Border) |
| `--frame-line` | `#d4d4d4` | `#262626` | Dashed blueprint frame lines (`1px dashed`) |
| `--fg` | `#171717` | `#fafafa` | Primary headings, high-emphasis values (Cloud White) |
| `--fg-2` | `#404040` | `#d4d4d4` | Body prose, technical descriptions |
| `--fg-3` | `#595959` | `#a1a1a1` | Navigation links, captions, secondary labels (Fog) |
| `--fg-4` | `#707070` | `#808080` | Tertiary metadata, inactive icons, table `—` marks (Ash, ≥ 4.5:1) |
| `--primary` | `#171717` | `#e5e5e5` | Pale Conversion Button fill (Paper White in dark, near-black in light) |
| `--on-primary` | `#ffffff` | `#171717` | Text and icon stroke on `--primary` |
| `--accent` | `#297a3a` | `#3fb950` | Live status dots, verified `✓` marks, active spring telemetry |
| `--link-accent` | `#c2410c` | `#ff8833` | Forward inline links and directional arrows (`→`, `↗`) (Signal Orange) |
| `--danger` | `#dc2626` | `#ef4444` | Recording indicator dot only |

### Typography

`Geist`, `Inter`, `system-ui` for reading and UI (`--font-sans`); `Geist Mono`,
`JetBrains Mono`, `ui-monospace` for hardware registers, CLI commands, and
metrics (`--font-mono`).

| Role | Size / Line | Weight | Tracking | Notes |
|---|---|---|---|---|
| `eyebrow` | `11px / 1.5` | `500` | `0.07em` | Uppercase monospace section & telemetry stamps |
| `meta` | `12px / 1.45` | `400`–`500` | `0.01em` | Monospace file tags, shortcuts, coordinates |
| `caption` | `13px / 1.5` | `400` | `0.01em` | Table labels, footnotes, secondary descriptions |
| `ui` | `14px / 1.43` | `500` | `0` | Navigation links, buttons, tab controls |
| `body` | `16px / 1.55` | `400` | `0` | Primary prose (`max-width: 64ch`) |
| `subheading` | `20px / 1.35` | `500` | `-0.015em` | Module headings inside blueprint frames |
| `section-heading` | `clamp(28px, 3.2vw, 36px) / 1.12` | `500` | `-0.025em` | Section titles (`text-wrap: balance`) |
| `display` | `clamp(38px, 4.8vw, 58px) / 1.03` | `500` | `-0.032em` | Hero headline (`text-wrap: balance`) |

### Space, Radius & Depth

- **Grid:** `4px` base (`4`, `8`, `12`, `16`, `24`, `32`, `48`, `64`, `96`, `128px`).
- **Max width:** `1152px` (Graphite spec) with `24px` horizontal padding (`16px` on `<640px`).
- **Radius:** `--r-sm: 4px` (tags, kbd, demo tabs); `--r: 8px` (buttons);
  `--r-lg: 10px` (panels, demo frame, nav shell, terminal bar — never above `10px`);
  `--r-full: 9999px` (status pills and section labels only).
- **Depth:** `1px solid var(--line)` hairline borders. Product demo frame uses
  Graphite's inset ring (`rgba(101, 129, 128, 0.15) 0 0 2px 2px inset, rgba(32, 47, 46, 0.15) 0 0 4px 4px inset`).

## Visual QA (2026-10-17)

Run after rebuilding `docs/index.html`, `docs/og-banner.png`, `assets/hero-banner.svg`
and `assets/benchmark-card.svg`, following the Refero `visual-workflow.md` loop.

```text
Source truth: Graphite (fd36bae9, primary), Linear (90ce5883), Resend (0d914ef0), ossium.in frames, reference lock above
Evidence: headless Chrome full-page captures at 1440 wide (dark + light) and a true 390 px
          viewport via iframe (headless Chrome clamps windows to 500 px); a real Chrome
          session driven over the DevTools protocol for workbench input
Final result: passed, no open P0–P2
```

| Severity | Finding | Fix |
|---|---|---|
| P1 | Single-column grids used `1fr`, so the long install command set a min-content width and pushed the hero past the viewport on narrow screens | All collapsed grids use `minmax(0, 1fr)`; announcement text truncates with an ellipsis |
| P1 | Zoomed camera preview was blurry: `will-change: transform` froze the scene raster at 1x | Removed `will-change` from `.scene`; preview re-rasterizes at the zoom level |
| P2 | Code highlighting used Signal Orange for numbers and terminal green for strings, diluting both roles | Syntax is monochrome (weight, `--fg-3`, italic comments) |
| P2 | Live GitHub counts rendered "0" beside the Star button | Counts and the stats strip stay hidden until the repo has at least 5 stars |
| P2 | Step 01 clone URL was clipped inside its code block | Split onto two lines with a shell continuation |
| P2 | Issues panel header said "good first issues" while #2 is labelled help wanted | Header reads "Open issues" |
| P2 | FAQ claimed GNOME/KDE/Hyprland/Sway were tested and that hybrid GPUs split capture and encode | Copy now says COSMIC is tested, others should work, and describes the real X11 and x264 fallbacks found in `src/cursor.rs` and `src/encoder.rs` |
| P3 | Hero-to-workbench gap was 208 px | Hero bottom padding reduced to 72 px |

Workbench behaviour, measured in a real Chrome session (1440×900):

| Step | Result |
|---|---|
| Move across the stage, then stop | Phase `Tracking`, then `Zooming in` to 1.77× at ~750 ms, centre edge-clamped at 0.72 |
| Wait past the 1650 ms hold | `Releasing`, back to 1.00× by ~3.5 s |
| Click | Pulse to 2.36× toward the click point |
| Lock / Reset | Holds 2.00× at the cursor / returns to 1.00× at 0.50, 0.50 |
| Solver rate | 239–240 steps/s; no console errors |

Not covered: Safari and Firefox rendering, and real touch devices.
