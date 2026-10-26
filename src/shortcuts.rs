//! Desktop-wide shortcuts that start, stop, and pause recording.
//!
//! Wayland gives apps no global key grabs, and COSMIC has no GlobalShortcuts
//! portal, so each shortcut is a desktop keybinding that runs
//! `hypergsr --toggle-recording` (or `--pause-recording`). That command
//! activates the running island's action over D-Bus.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub const APP_ID: &str = "io.github.KunalSiyag.gpu_recorder_rs";
const APP_OBJECT_PATH: &str = "/io/github/KunalSiyag/gpu_recorder_rs";
/// The start/stop combination installed by [`install`], as shown to people.
pub const DEFAULT_LABEL: &str = "Ctrl+Alt+R";
pub const PAUSE_LABEL: &str = "Ctrl+Alt+P";

/// One desktop keybinding (always Ctrl+Alt plus `key`).
struct Binding {
    /// Command-line flag the keybinding runs.
    flag: &'static str,
    /// Island action the flag activates.
    action: &'static str,
    key: &'static str,
    label: &'static str,
    description: &'static str,
    /// GNOME custom-keybinding path.
    gnome_path: &'static str,
}

const BINDINGS: &[Binding] = &[
    Binding {
        flag: "--toggle-recording",
        action: "toggle-record",
        key: "r",
        label: DEFAULT_LABEL,
        description: "HyperGSR: start or stop recording",
        gnome_path: "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/hypergsr/",
    },
    Binding {
        flag: "--pause-recording",
        action: "pause-record",
        key: "p",
        label: PAUSE_LABEL,
        description: "HyperGSR: pause or continue recording",
        gnome_path: "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/hypergsr-pause/",
    },
];

/// Shell command the start/stop keybinding runs.
pub fn toggle_command() -> String {
    command_for(BINDINGS[0].flag)
}

fn command_for(flag: &str) -> String {
    if let Ok(id) = std::env::var("FLATPAK_ID") {
        return format!("flatpak run {id} {flag}");
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "hypergsr".to_string());
    if exe.contains(' ') {
        format!("'{exe}' {flag}")
    } else {
        format!("{exe} {flag}")
    }
}

/// Runs a shortcut's command-line `flag` (`--toggle-recording` or
/// `--pause-recording`) against the running island. Start/stop with no
/// island running starts one that begins recording right away.
pub fn activate_running_recorder(flag: &str) -> Result<()> {
    let Some(binding) = BINDINGS.iter().find(|b| b.flag == flag) else {
        bail!("unknown shortcut {flag}");
    };
    if activate_in_running(binding.action, None)? {
        return Ok(());
    }
    if binding.action != "toggle-record" {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .env("HYPERGSR_START_RECORDING", "1")
        .spawn()
        .context("cannot start HyperGSR")?;
    Ok(())
}

/// Activates the running island's `action` over D-Bus. `Ok(false)` when no
/// island is running.
pub fn activate_in_running(action: &str, param: Option<glib::Variant>) -> Result<bool> {
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
    if !running {
        return Ok(false);
    }
    let params: Vec<glib::Variant> = param.into_iter().collect();
    let platform = std::collections::HashMap::<String, glib::Variant>::new();
    bus.call_sync(
        Some(APP_ID),
        APP_OBJECT_PATH,
        "org.gtk.Actions",
        "Activate",
        Some(&(action, params, platform).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
    )
    .with_context(|| format!("the running HyperGSR did not accept {action}"))?;
    Ok(true)
}

fn desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase()
}

fn cosmic_file() -> PathBuf {
    crate::bundled::home_dir().join(".config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
}

/// Whether the start/stop keybinding exists on this desktop.
pub fn installed() -> bool {
    installed_binding(&BINDINGS[0])
}

/// Whether every HyperGSR keybinding exists on this desktop.
pub fn all_installed() -> bool {
    BINDINGS.iter().all(installed_binding)
}

fn installed_binding(binding: &Binding) -> bool {
    let d = desktop();
    if d.contains("cosmic") {
        return std::fs::read_to_string(cosmic_file()).is_ok_and(|t| t.contains(binding.flag));
    }
    if d.contains("gnome") {
        return gnome_get(&format!("{GNOME_KEY}:{}", binding.gnome_path), "command")
            .is_some_and(|c| c.contains(binding.flag));
    }
    false
}

/// Adds the Ctrl+Alt+R (start/stop) and Ctrl+Alt+P (pause) keybindings to
/// COSMIC or GNOME. Elsewhere, returns an error that tells the user which
/// command to bind.
pub fn install() -> Result<String> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        bail!(
            "the Flatpak cannot change desktop shortcuts; bind {DEFAULT_LABEL} to `{}` in your keyboard settings",
            toggle_command()
        );
    }
    let d = desktop();
    let skipped = if d.contains("cosmic") {
        install_cosmic()?
    } else if d.contains("gnome") {
        BINDINGS.iter().try_for_each(install_gnome)?;
        Vec::new()
    } else {
        bail!(
            "bind {DEFAULT_LABEL} to `{}` in your desktop's keyboard settings",
            toggle_command()
        )
    };
    if !skipped.is_empty() {
        return Ok(format!(
            "{DEFAULT_LABEL} starts and stops recording ({} is taken by another shortcut)",
            skipped.join(", ")
        ));
    }
    if d.contains("cosmic") || d.contains("gnome") {
        return Ok(format!("{DEFAULT_LABEL} starts and stops recording, {PAUSE_LABEL} pauses"));
    }
    bail!(
        "bind {DEFAULT_LABEL} to `{}` in your desktop's keyboard settings",
        toggle_command()
    )
}

/// COSMIC keeps custom shortcuts as a RON map; missing entries are
/// appended before the closing brace, after a one-time backup. Returns the
/// labels skipped because another shortcut already uses them.
fn install_cosmic() -> Result<Vec<&'static str>> {
    let path = cosmic_file();
    let mut text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{\n}\n".to_string());
    let mut skipped = Vec::new();
    let mut changed = false;
    for binding in BINDINGS {
        if text.contains(binding.flag) {
            continue;
        }
        if cosmic_has_binding(&text, &["Alt", "Ctrl"], binding.key) {
            if binding.action == "toggle-record" {
                bail!("{DEFAULT_LABEL} is already used by another COSMIC shortcut");
            }
            skipped.push(binding.label);
            continue;
        }
        let Some(end) = text.rfind('}') else {
            bail!("cannot read {}", path.display());
        };
        let command = command_for(binding.flag).replace('\\', "\\\\").replace('"', "\\\"");
        let entry = format!(
            "    (\n        modifiers: [\n            Ctrl,\n            Alt,\n        ],\n        key: \"{}\",\n        description: Some(\"{}\"),\n    ): Spawn(\"{command}\"),\n",
            binding.key, binding.description
        );
        let head = text[..end].trim_end();
        let sep = if head.ends_with('{') || head.ends_with(',') { "\n" } else { ",\n" };
        text = format!("{head}{sep}{entry}{}", &text[end..]);
        changed = true;
    }
    if !changed {
        return Ok(skipped);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let backup = path.with_file_name("custom.hypergsr-backup");
    if path.exists() && !backup.exists() {
        std::fs::copy(&path, &backup).context("cannot back up the COSMIC shortcuts")?;
    }
    let tmp = path.with_file_name("custom.hypergsr-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(skipped)
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

fn install_gnome(binding: &Binding) -> Result<()> {
    let path = binding.gnome_path;
    let list = gnome_get(GNOME_SCHEMA, "custom-keybindings").context("gsettings is not available")?;
    if !list.contains(path) {
        let inner = list.trim_start_matches("@as").trim().trim_start_matches('[').trim_end_matches(']').trim();
        let updated = if inner.is_empty() {
            format!("['{path}']")
        } else {
            format!("[{inner}, '{path}']")
        };
        gnome_set(GNOME_SCHEMA, "custom-keybindings", &updated)?;
    }
    let schema = format!("{GNOME_KEY}:{path}");
    gnome_set(&schema, "name", &format!("'{}'", binding.description))?;
    gnome_set(&schema, "command", &format!("'{}'", command_for(binding.flag).replace('\'', "")))?;
    gnome_set(&schema, "binding", &format!("'<Control><Alt>{}'", binding.key))?;
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

