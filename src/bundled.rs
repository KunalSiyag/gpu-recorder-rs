//! Runtime files compiled into the binary so installs without a source checkout
//! (`cargo install`) still find the capture helper and UI icons.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Built by `build.rs`; empty when the helper could not be compiled.
static PW_CAPTURE_HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hypergsr-pw-capture"));

const ICONS: &[(&str, &[u8])] = &[
    (
        "hypergsr-logo.svg",
        include_bytes!("../assets/hypergsr-logo.svg"),
    ),
    (
        "icon-autozoom.svg",
        include_bytes!("../assets/icon-autozoom.svg"),
    ),
    ("icon-gpu.svg", include_bytes!("../assets/icon-gpu.svg")),
    (
        "icon-record.svg",
        include_bytes!("../assets/icon-record.svg"),
    ),
    ("icon-stop.svg", include_bytes!("../assets/icon-stop.svg")),
    ("icon-sun.svg", include_bytes!("../assets/icon-sun.svg")),
    ("icon-theme.svg", include_bytes!("../assets/icon-theme.svg")),
];

/// Symbolic UI icons, drawn as filled shapes so GTK recolors them to the
/// widget's text color in every theme and state.
const SYMBOLIC_ICONS: &[(&str, &[u8])] = &[
    (
        "hgsr-area-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-area-symbolic.svg"),
    ),
    (
        "hgsr-audio-both-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-audio-both-symbolic.svg"),
    ),
    (
        "hgsr-audio-mic-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-audio-mic-symbolic.svg"),
    ),
    (
        "hgsr-audio-off-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-audio-off-symbolic.svg"),
    ),
    (
        "hgsr-audio-system-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-audio-system-symbolic.svg"),
    ),
    (
        "hgsr-camera-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-camera-symbolic.svg"),
    ),
    (
        "hgsr-check-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-check-symbolic.svg"),
    ),
    (
        "hgsr-choose-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-choose-symbolic.svg"),
    ),
    (
        "hgsr-close-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-close-symbolic.svg"),
    ),
    (
        "hgsr-ctrl-click-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-ctrl-click-symbolic.svg"),
    ),
    (
        "hgsr-cursor-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-cursor-symbolic.svg"),
    ),
    (
        "hgsr-double-click-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-double-click-symbolic.svg"),
    ),
    (
        "hgsr-export-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-export-symbolic.svg"),
    ),
    (
        "hgsr-folder-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-folder-symbolic.svg"),
    ),
    (
        "hgsr-hide-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-hide-symbolic.svg"),
    ),
    (
        "hgsr-keyboard-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-keyboard-symbolic.svg"),
    ),
    (
        "hgsr-lock-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-lock-symbolic.svg"),
    ),
    (
        "hgsr-moon-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-moon-symbolic.svg"),
    ),
    (
        "hgsr-mouse-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-mouse-symbolic.svg"),
    ),
    (
        "hgsr-pause-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-pause-symbolic.svg"),
    ),
    (
        "hgsr-play-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-play-symbolic.svg"),
    ),
    (
        "hgsr-pulse-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-pulse-symbolic.svg"),
    ),
    (
        "hgsr-record-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-record-symbolic.svg"),
    ),
    (
        "hgsr-replay-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-replay-symbolic.svg"),
    ),
    (
        "hgsr-screen-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-screen-symbolic.svg"),
    ),
    (
        "hgsr-shake-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-shake-symbolic.svg"),
    ),
    (
        "hgsr-shortcut-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-shortcut-symbolic.svg"),
    ),
    (
        "hgsr-sun-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-sun-symbolic.svg"),
    ),
    (
        "hgsr-timer-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-timer-symbolic.svg"),
    ),
    (
        "hgsr-toggle-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-toggle-symbolic.svg"),
    ),
    (
        "hgsr-trim-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-trim-symbolic.svg"),
    ),
    (
        "hgsr-webcam-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-webcam-symbolic.svg"),
    ),
    (
        "hgsr-wide-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-wide-symbolic.svg"),
    ),
    (
        "hgsr-window-symbolic.svg",
        include_bytes!("../assets/icons/hgsr-window-symbolic.svg"),
    ),
];

/// Inter (SIL Open Font License, see assets/fonts/Inter-LICENSE.txt),
/// subset to Latin.
static UI_FONT: &[u8] = include_bytes!("../assets/fonts/InterVariable.ttf");

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

/// XDG base directory, ignoring relative values as the spec requires. Flatpak
/// points these at `~/.var/app/<id>/`, which is writable inside the sandbox.
fn xdg_base(value: Option<std::ffi::OsString>, home: &Path, fallback: &str) -> PathBuf {
    value
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(fallback))
}

pub fn config_dir() -> PathBuf {
    xdg_base(std::env::var_os("XDG_CONFIG_HOME"), &home_dir(), ".config").join("hypergsr")
}

pub fn cache_dir() -> PathBuf {
    xdg_base(std::env::var_os("XDG_CACHE_HOME"), &home_dir(), ".cache").join("hypergsr")
}

fn data_dir() -> PathBuf {
    xdg_base(
        std::env::var_os("XDG_DATA_HOME"),
        &home_dir(),
        ".local/share",
    )
    .join("hypergsr")
    .join(env!("CARGO_PKG_VERSION"))
}

fn materialize(relative: &Path, bytes: &[u8], mode: u32) -> Option<PathBuf> {
    materialize_in(&data_dir(), relative, bytes, mode)
}

fn materialize_in(base: &Path, relative: &Path, bytes: &[u8], mode: u32) -> Option<PathBuf> {
    if bytes.is_empty() {
        return None;
    }
    let path = base.join(relative);
    // Development builds keep the version, so compare contents, not just size.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64)
        && std::fs::read(&path).is_ok_and(|existing| existing == bytes)
    {
        return Some(path);
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).ok()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

pub fn pw_capture_helper() -> Option<PathBuf> {
    materialize(Path::new("hypergsr-pw-capture"), PW_CAPTURE_HELPER, 0o755)
}

/// Directory holding the symbolic icons, for `IconTheme::add_search_path`.
pub fn symbolic_icon_dir() -> Option<PathBuf> {
    let mut dir = None;
    for (name, bytes) in SYMBOLIC_ICONS {
        let path = materialize(&Path::new("icons").join(name), bytes, 0o644)?;
        dir = path.parent().map(Path::to_path_buf);
    }
    dir
}

pub fn ui_font() -> Option<PathBuf> {
    materialize(Path::new("fonts/InterVariable.ttf"), UI_FONT, 0o644)
}

pub fn icon(name: &str) -> Option<PathBuf> {
    let (_, bytes) = ICONS.iter().find(|(n, _)| *n == name)?;
    materialize(&Path::new("assets").join(name), bytes, 0o644)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_executable_once_and_reuses_it() {
        let base = std::env::temp_dir().join(format!("hypergsr-bundled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let path = materialize_in(&base, Path::new("bin/helper"), b"#!/bin/sh\n", 0o755)
            .expect("extracted");
        assert_eq!(std::fs::read(&path).unwrap(), b"#!/bin/sh\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let again = materialize_in(&base, Path::new("bin/helper"), b"#!/bin/sh\n", 0o755);
        assert_eq!(again.as_deref(), Some(path.as_path()));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn xdg_base_ignores_missing_and_relative_values() {
        let home = Path::new("/home/me");
        assert_eq!(
            xdg_base(Some("/home/me/.var/app/x/config".into()), home, ".config"),
            PathBuf::from("/home/me/.var/app/x/config")
        );
        assert_eq!(
            xdg_base(Some("relative".into()), home, ".config"),
            PathBuf::from("/home/me/.config")
        );
        assert_eq!(
            xdg_base(None, home, ".local/share"),
            PathBuf::from("/home/me/.local/share")
        );
    }

    #[test]
    fn empty_payload_is_not_extracted() {
        let base = std::env::temp_dir();
        assert!(materialize_in(&base, Path::new("never-written"), b"", 0o644).is_none());
    }

    #[test]
    fn every_ui_icon_is_embedded() {
        for (name, bytes) in ICONS {
            assert!(
                bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml"),
                "{name}"
            );
        }
    }
}
