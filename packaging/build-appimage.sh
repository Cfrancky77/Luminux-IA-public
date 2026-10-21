#!/usr/bin/env bash
# Construit Luminux_IA-x86_64.AppImage à partir des sources.
# Prérequis : Rust (rustup), et appimagetool dans le PATH ou à côté de ce script
# (https://github.com/AppImage/appimagetool/releases).
# ONNX Runtime avec WebGPU (masques IA sur la carte graphique par Vulkan, processeur en secours) : la
# bibliothèque du paquet PyPI onnxruntime-webgpu 1.27.0 (wheel manylinux x86_64), fichier
# onnxruntime/capi/libonnxruntime.so.1.27.0 ; ORT_LIB pointe dessus.
set -euo pipefail
cd "$(dirname "$0")/.."
ORT_LIB=${ORT_LIB:-packaging/onnxruntime-webgpu/libonnxruntime.so.1.27.0}
TOOL=${APPIMAGETOOL:-$(command -v appimagetool || echo packaging/appimagetool)}

cargo build --release
APPDIR=$(mktemp -d)/Luminux.AppDir
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
cp target/release/luminux "$APPDIR/usr/bin/"
cp packaging/luminux.desktop "$APPDIR/" && cp packaging/luminux.desktop "$APPDIR/usr/share/applications/"
cp packaging/luminux.png "$APPDIR/" && cp packaging/luminux.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/"
cp -L "$ORT_LIB" "$APPDIR/usr/lib/libonnxruntime.so.1"
ln -s usr/bin/luminux "$APPDIR/AppRun"
# Vulkan, X11/Wayland et le pilote GPU viennent du système ; seul ONNX Runtime est embarqué.
ARCH=x86_64 "$TOOL" --appimage-extract-and-run "$APPDIR" Luminux_IA-x86_64.AppImage
echo "Créé : $(pwd)/Luminux_IA-x86_64.AppImage"
