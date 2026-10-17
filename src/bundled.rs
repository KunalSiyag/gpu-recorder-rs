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

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(".local/share"))
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
    // The directory is versioned, so a file of the right size is this build's copy.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64) {
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
