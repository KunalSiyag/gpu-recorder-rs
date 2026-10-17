#!/usr/bin/env bash
set -euo pipefail

echo "================================================================================"
echo "  ⚡ HyperGSR Studio (gpu-recorder-rs) — One-Command Installer"
echo "================================================================================"

if command -v apt-get >/dev/null 2>&1; then
    echo "📦 Detected Debian/Ubuntu/Pop!_OS/COSMIC — checking system dependencies..."
    sudo apt-get update -qq
    sudo apt-get install -y \
        build-essential pkg-config clang ffmpeg \
        libgtk-4-dev libdrm-dev libx11-dev \
        libpipewire-0.3-dev libportal-dev
    if apt-cache show libgtk4-layer-shell-dev >/dev/null 2>&1; then
        sudo apt-get install -y libgtk4-layer-shell-dev
    elif ! pkg-config --exists gtk4-layer-shell-0; then
        # Not packaged before Ubuntu 25.10; build it into /usr/local like CI does.
        echo "📦 libgtk4-layer-shell-dev not in apt — building gtk4-layer-shell v1.3.0 from source..."
        sudo apt-get install -y git meson ninja-build libwayland-dev libwayland-bin wayland-protocols
        gls_dir="$(mktemp -d)"
        git clone --depth 1 --branch v1.3.0 https://github.com/wmww/gtk4-layer-shell.git "$gls_dir"
        meson setup "$gls_dir/build" "$gls_dir" \
            -Dexamples=false -Ddocs=false -Dtests=false -Dintrospection=false -Dvapi=false
        ninja -C "$gls_dir/build"
        sudo ninja -C "$gls_dir/build" install
        sudo ldconfig
        rm -rf "$gls_dir"
    fi
elif command -v dnf >/dev/null 2>&1; then
    echo "📦 Detected Fedora — checking system dependencies..."
    sudo dnf install -y \
        gcc clang pkgconf-pkg-config ffmpeg \
        gtk4-devel gtk4-layer-shell-devel libdrm-devel libX11-devel \
        pipewire-devel libportal-devel
elif command -v pacman >/dev/null 2>&1; then
    echo "📦 Detected Arch Linux — checking system dependencies..."
    sudo pacman -S --needed --noconfirm \
        base-devel clang pkgconf ffmpeg \
        gtk4 gtk4-layer-shell libdrm libx11 pipewire libportal
fi

echo "🔨 Compiling native PipeWire + XDG Desktop Portal ScreenCast helper..."
mkdir -p bin
gcc -O2 -Wall src/pw_portal_capture.c -o bin/hypergsr-pw-capture \
    $(pkg-config --cflags --libs libportal libpipewire-0.3 gio-2.0) -lpthread

echo "🦀 Building HyperGSR Studio release binary..."
cargo build --release

echo "================================================================================"
echo "✔ Build complete! Launching HyperGSR Dynamic Island Studio..."
echo "  Run anytime with: ./target/release/gpu-recorder-rs"
echo "================================================================================"
exec ./target/release/gpu-recorder-rs
