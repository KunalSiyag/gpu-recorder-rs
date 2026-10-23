//! A desktop-wide shortcut that starts and stops recording.
//!
//! Wayland gives apps no global key grabs, and COSMIC has no GlobalShortcuts
//! portal, so the shortcut is a desktop keybinding that runs
//! `hypergsr --toggle-recording`. That command activates the running
//! island's `toggle-record` action over D-Bus.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub const APP_ID: &str = "io.github.KunalSiyag.gpu_recorder_rs";
const APP_OBJECT_PATH: &str = "/io/github/KunalSiyag/gpu_recorder_rs";
/// The combination installed by [`install`], as shown to people.
pub const DEFAULT_LABEL: &str = "Ctrl+Alt+R";
const MARKER: &str = "--toggle-recording";

/// Shell command a desktop keybinding should run.
pub fn toggle_command() -> String {
    if let Ok(id) = std::env::var("FLATPAK_ID") {
        return format!("flatpak run {id} {MARKER}");
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "hypergsr".to_string());
    if exe.contains(' ') {
        format!("'{exe}' {MARKER}")
    } else {
        format!("{exe} {MARKER}")
    }
}

/// Runs `--toggle-recording`: toggles the running island, or starts one that
/// begins recording right away.
pub fn toggle_running_recorder() -> Result<()> {
    use gio::prelude::*;
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .context("cannot reach the D-Bus session bus")?;
    let running = bus
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(APP_ID,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.child_value(0).get::<bool>())
        .unwrap_or(false);
    if running {
        let params: Vec<glib::Variant> = Vec::new();
        let platform = std::collections::HashMap::<String, glib::Variant>::new();
        bus.call_sync(
            Some(APP_ID),
            APP_OBJECT_PATH,
            "org.gtk.Actions",
            "Activate",
            Some(&("toggle-record", params, platform).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .context("the running HyperGSR did not accept the toggle")?;
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .env("HYPERGSR_START_RECORDING", "1")
        .spawn()
        .context("cannot start HyperGSR")?;
    Ok(())
}

fn desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase()
}

fn cosmic_file() -> PathBuf {
    crate::bundled::home_dir().join(".config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
}

/// Whether a HyperGSR keybinding already exists on this desktop.
pub fn installed() -> bool {
    let d = desktop();
    if d.contains("cosmic") {
        return std::fs::read_to_string(cosmic_file()).is_ok_and(|t| t.contains(MARKER));
    }
    if d.contains("gnome") {
        return gnome_get(&format!("{GNOME_KEY}:{GNOME_PATH}"), "command").is_some_and(|c| c.contains(MARKER));
    }
    false
}

/// Adds the Ctrl+Alt+R keybinding to COSMIC or GNOME. Elsewhere, returns an
/// error that tells the user which command to bind.
pub fn install() -> Result<String> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        bail!(
            "the Flatpak cannot change desktop shortcuts; bind {DEFAULT_LABEL} to `{}` in your keyboard settings",
            toggle_command()
        );
    }
    let d = desktop();
    if d.contains("cosmic") {
        install_cosmic()?;
        return Ok(format!("{DEFAULT_LABEL} now starts and stops recording"));
    }
    if d.contains("gnome") {
        install_gnome()?;
        return Ok(format!("{DEFAULT_LABEL} now starts and stops recording"));
    }
    bail!(
        "bind {DEFAULT_LABEL} to `{}` in your desktop's keyboard settings",
        toggle_command()
    )
}

/// COSMIC keeps custom shortcuts as a RON map; the entry is appended before
/// the closing brace, after a one-time backup.
fn install_cosmic() -> Result<()> {
    let path = cosmic_file();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{\n}\n".to_string());
    if text.contains(MARKER) {
        return Ok(());
    }
    if cosmic_has_binding(&text, &["Alt", "Ctrl"], "r") {
        bail!("{DEFAULT_LABEL} is already used by another COSMIC shortcut");
    }
    let Some(end) = text.rfind('}') else {
        bail!("cannot read {}", path.display());
    };
    let command = toggle_command().replace('\\', "\\\\").replace('"', "\\\"");
    let entry = format!(
        "    (\n        modifiers: [\n            Ctrl,\n            Alt,\n        ],\n        key: \"r\",\n        description: Some(\"HyperGSR: start or stop recording\"),\n    ): Spawn(\"{command}\"),\n"
    );
    let head = text[..end].trim_end();
    let sep = if head.ends_with('{') || head.ends_with(',') { "\n" } else { ",\n" };
    let updated = format!("{head}{sep}{entry}{}", &text[end..]);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let backup = path.with_file_name("custom.hypergsr-backup");
    if path.exists() && !backup.exists() {
        std::fs::copy(&path, &backup).context("cannot back up the COSMIC shortcuts")?;
    }
    let tmp = path.with_file_name("custom.hypergsr-tmp");
    std::fs::write(&tmp, updated)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn cosmic_has_binding(text: &str, modifiers: &[&str], key: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find("modifiers:") {
        rest = &rest[start + "modifiers:".len()..];
        let Some(close) = rest.find(']') else { break };
        let mut mods: Vec<&str> = rest[..close]
            .trim_start_matches(|c: char| c.is_whitespace() || c == '[')
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .collect();
        mods.sort_unstable();
        let after = &rest[close..];
        let entry_key = after
            .find("key:")
            .map(|k| after[k + 4..].trim_start())
            .and_then(|k| k.strip_prefix('"'))
            .and_then(|k| k.split('"').next());
        if mods == modifiers && entry_key.is_some_and(|k| k.eq_ignore_ascii_case(key)) {
            return true;
        }
    }
    false
}

const GNOME_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const GNOME_KEY: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const GNOME_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/hypergsr/";

fn gnome_get(schema: &str, key: &str) -> Option<String> {
    let out = Command::new("gsettings").args(["get", schema, key]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn gnome_set(schema: &str, key: &str, value: &str) -> Result<()> {
    let status = Command::new("gsettings").args(["set", schema, key, value]).status()?;
    if !status.success() {
        bail!("gsettings could not set {key}");
    }
    Ok(())
}

fn install_gnome() -> Result<()> {
    let list = gnome_get(GNOME_SCHEMA, "custom-keybindings").context("gsettings is not available")?;
    if !list.contains(GNOME_PATH) {
        let inner = list.trim_start_matches("@as").trim().trim_start_matches('[').trim_end_matches(']').trim();
        let updated = if inner.is_empty() {
            format!("['{GNOME_PATH}']")
        } else {
            format!("[{inner}, '{GNOME_PATH}']")
        };
        gnome_set(GNOME_SCHEMA, "custom-keybindings", &updated)?;
    }
    let schema = format!("{GNOME_KEY}:{GNOME_PATH}");
    gnome_set(&schema, "name", "'HyperGSR: start or stop recording'")?;
    gnome_set(&schema, "command", &format!("'{}'", toggle_command().replace('\'', "")))?;
    gnome_set(&schema, "binding", "'<Control><Alt>r'")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_existing_cosmic_bindings() {
        let text = "{\n    (\n        modifiers: [\n            Ctrl,\n            Alt,\n        ],\n        key: \"R\",\n    ): Spawn(\"x\"),\n    (\n        modifiers: [\n            Super,\n        ],\n        key: \"r\",\n    ): Spawn(\"y\"),\n}\n";
        assert!(cosmic_has_binding(text, &["Alt", "Ctrl"], "r"));
        assert!(cosmic_has_binding(text, &["Super"], "r"));
        assert!(!cosmic_has_binding(text, &["Alt", "Super"], "r"));
    }
}

