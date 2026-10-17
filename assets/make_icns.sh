#!/bin/bash
# Redraws assets/icon_1024.png from assets/make_icon.swift and rebuilds
# assets/AppIcon.icns (bundle.sh also builds the .icns from the PNG when missing).
set -euo pipefail
cd "$(dirname "$0")"

swift make_icon.swift icon_1024.png
SET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  sips -z $s $s icon_1024.png --out "$SET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) icon_1024.png --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o AppIcon.icns
echo "wrote assets/AppIcon.icns"
