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
        libgtk-4-dev libgtk4-layer-shell-dev libdrm-dev libx11-dev \
        libpipewire-0.3-dev libportal-dev
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
