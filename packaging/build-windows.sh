#!/bin/bash
# Version Windows portable : dossier « Luminux IA » zippé (luminux.exe + ONNX Runtime WebGPU), sans installation.
# ONNX Runtime : DLL du paquet PyPI onnxruntime-webgpu 1.27.0 (roue win_amd64) ; dxil.dll et dxcompiler.dll
# servent à WebGPU via Direct3D 12. Sur un runner Windows (MSVC) ou en croisé (TARGET=x86_64-pc-windows-gnu).
set -euo pipefail
TARGET=${TARGET:-x86_64-pc-windows-msvc}
OUT=${OUT:-Luminux_IA-windows-x64.zip}
cargo build --release --target "$TARGET"
rm -rf packaging/winwheel "packaging/Luminux IA"
python -m pip download --no-deps --only-binary=:all: --python-version 3.12 --platform win_amd64 -d packaging/winwheel onnxruntime-webgpu==1.27.0
mkdir -p "packaging/Luminux IA"
cp "target/$TARGET/release/luminux.exe" "packaging/Luminux IA/Luminux IA.exe"
python - <<'PY'
import glob, zipfile
z = zipfile.ZipFile(glob.glob("packaging/winwheel/*.whl")[0])
for n in ["onnxruntime.dll", "onnxruntime_providers_shared.dll", "dxil.dll", "dxcompiler.dll"]:
    open("packaging/Luminux IA/" + n, "wb").write(z.read("onnxruntime/capi/" + n))
PY
cp LICENSE "packaging/Luminux IA/LICENSE.txt" 2>/dev/null || true
cp packaging/LISEZ-MOI-windows.txt "packaging/Luminux IA/LISEZ-MOI.txt"
python - "$OUT" <<'PY'
import os, sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_DEFLATED) as z:
    for root, _, files in os.walk("packaging/Luminux IA"):
        for f in files:
            p = os.path.join(root, f)
            z.write(p, os.path.relpath(p, "packaging"))
PY
ls -l "$OUT"
