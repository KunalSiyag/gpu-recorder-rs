use std::path::{Path, PathBuf};
use std::process::Command;

/// Compiles the PipeWire/portal capture helper so `src/bundled.rs` can embed it.
/// A missing toolchain only disables the zero-copy path, so failures are warnings.
fn main() {
    println!("cargo:rerun-if-changed=src/pw_portal_capture.c");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=HYPERGSR_SKIP_HELPER");

    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("hypergsr-pw-capture");
    let result = if std::env::var_os("HYPERGSR_SKIP_HELPER").is_some() {
        Err("HYPERGSR_SKIP_HELPER is set".to_string())
    } else {
        build_helper(&out)
    };
    if let Err(reason) = result {
        std::fs::write(&out, b"").expect("write empty helper placeholder");
        println!(
            "cargo:warning=hypergsr-pw-capture was not embedded ({reason}); \
             install gcc, pkg-config, PipeWire, GLib and libportal development files for zero-copy capture"
        );
    }
}

fn build_helper(out: &Path) -> Result<(), String> {
    let flags = Command::new("pkg-config")
        .args(["--cflags", "--libs", "libpipewire-0.3", "gio-2.0"])
        .output()
        .map_err(|e| format!("pkg-config: {e}"))?;
    if !flags.status.success() {
        return Err(String::from_utf8_lossy(&flags.stderr).trim().to_string());
    }
    let flags = String::from_utf8_lossy(&flags.stdout).into_owned();
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    // The helper declares libportal's ABI itself, so only the shared library is needed.
    let status = Command::new(&cc)
        .args(["-O2", "-Wall", "src/pw_portal_capture.c", "-o"])
        .arg(out)
        .args(flags.split_whitespace())
        .args(["-l:libportal.so.1", "-lpthread"])
        .status()
        .map_err(|e| format!("{cc}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cc} exited with {status}"))
    }
}
