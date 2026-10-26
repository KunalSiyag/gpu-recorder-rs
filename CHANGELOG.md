# Changelog

All notable changes to HyperGSR. Versions follow [Semantic Versioning](https://semver.org/).

## 1.3.0 (unreleased)

### Added

- **Editor.** Trim the latest recording (or any video) on a thumbnail timeline and export it as 16:9, 9:16 or 1:1, in MP4, WebM (VP9/Opus) or GIF, with auto-zoom, click ripples and the background switched on or off. The five most recent recordings keep their raw capture in `~/.cache/hypergsr/projects` (6 GB at most) so exports re-render from the original. Open it with **Edit** on the island or `--edit <video>`.
- **Webcam bubble.** Record the webcam beside the screen and show it, mirrored, in a round bubble in a bottom corner, kept in sync on the kernel's monotonic clock.
- **Countdown and Pause.** A 3- or 5-second countdown once the screen share is live, and a Pause button (**Ctrl+Alt+P**); both are cut from the video, audio included. Stop during the countdown cancels.
- **Instant replay.** Keep the last 30 seconds or 2 minutes in a buffer on disk and save them as a clip without stopping.
- **Smooth studio cursor.** Where the portal sends the cursor separately (COSMIC, GNOME, KDE), it is drawn back on a smoothed path at Normal or Large size.
- **Click ripples** in the saved video, with clicks read from `/dev/input` on Wayland (needs the `input` group; keyboards are never opened).
- **Microphone noise reduction**, on by default.
- **Hide bar** hides the island while recording so it is not in the video; a notification offers Stop and Pause.
- Window recordings and multi-monitor setups zoom by the recorded stream's own cursor.
- Screen, window or area capture; desktop audio, microphone or both; presentation backgrounds with padding, rounded corners and shadow; a choice of save folder.
- `--pause-recording`, `--edit`, `--replay`, and `--aspect`, `--format`, `--cut`, `--webcam` for `--render-capture`.

### Fixed

- Recordings no longer end after a few seconds on a still screen: video time follows the wall clock.
- A broken screen share pauses the recording instead of ending it; **Resume** continues in the same video.

### Changed

- The capture helper speaks protocol 6 (`READY` carries the monotonic time of video time zero).
- CI runs `cargo clippy --all-targets -- -D warnings`.

## 1.2.0 (2026-10-17)

Desktop integration: an app menu entry for npm installs, a Flatpak bundle, and an app ID shared by all install methods.

## 1.1.0 (2026-10-17)

More accurate cursor tracking and packages that run outside a source checkout.

## 1.0.0 (2026-10-16)

First release.
