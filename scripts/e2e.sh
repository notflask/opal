#!/usr/bin/env bash
# End-to-end test of the overlay in a headless sway. Writes PNGs to $OUT.
#   scripts/e2e.sh drag      drag a region, capture mid-drag and after, press Enter
set -euo pipefail
cd "$(dirname "$0")/.."
# never let a failed sway start fall through to the real desktop
unset WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE NIRI_SOCKET
OUT=${OUT:-/tmp/opal-e2e}
rm -rf "$OUT"; mkdir -p "$OUT/overlay" "$OUT/shots"
# a colorful background so blur, pixelate and glass have something to show
if command -v magick >/dev/null; then
  magick -size 2560x1440 plasma:fractal -blur 0x2 "$OUT/bg.png"
  echo "output * bg $OUT/bg.png fill" > "$OUT/extra.conf"
fi
export OPAL_SWAY_EXTRA="$OUT/extra.conf"
eval "$(scripts/headless-sway.sh)"
[ -n "${SWAY_PID:-}" ] && [ -n "${WAYLAND_DISPLAY:-}" ] || { echo "headless sway failed to start" >&2; exit 1; }
trap 'kill $SWAY_PID 2>/dev/null || true' EXIT
sleep 2  # let swaybg paint before capturing
OPAL=./target/debug/opal
INJ=./target/debug/examples/inject
snap() { mkdir -p "$OUT/shots/$1"; HOME="$OUT/shots/$1" $OPAL all --no-edit >/dev/null; cp "$OUT/shots/$1/Screenshots/"*.png "$OUT/$1.png"; }

case "${1:-drag}" in
annot)
  (HOME="$OUT/overlay" OPAL_TRACE=1 timeout 40 $OPAL region >"$OUT/overlay.log" 2>&1 &)
  coproc IN { $INJ; }
  say() { echo "$1" >&"${IN[1]}"; sleep 0.2; }
  sleep 1
  say "m 2200 200"; say "down"; say "m 2800 600"; say "m 3300 900"; say "up"
  # arrow (default tool)
  say "m 2300 300"; say "down"; say "m 2500 400"; say "m 2700 450"; say "up"
  # rectangle
  say "tap 19"; say "m 2750 300"; say "down"; say "m 3000 500"; say "up"
  # pen, green
  say "tap 5"; say "tap 25"; say "m 2300 700"; say "down"
  for p in "2350 760" "2400 720" "2450 780" "2500 730" "2550 790"; do say "m $p"; done; say "up"
  # text, blue
  say "tap 6"; say "tap 20"; say "m 2300 550"; say "click"
  say "tap 35"; say "tap 23"; say "tap 1"
  # numbered steps
  say "tap 49"; say "m 2900 700"; say "down"; say "up"; say "m 3000 760"; say "down"; say "up"
  # pixelate
  say "tap 48"; say "m 3050 580"; say "down"; say "m 3250 850"; say "up"
  snap annotated
  # undo twice, redo once
  say "mods 4"; say "tap 44"; say "tap 44"; say "tap 21"; say "mods 0"
  snap undone
  say "tap 28"
  sleep 1
  exec {IN[1]}>&-
  ;;
repeat)
  (HOME="$OUT/overlay" OPAL_TRACE=1 timeout 40 $OPAL region >"$OUT/overlay.log" 2>&1 &)
  coproc IN { $INJ; }
  say() { echo "$1" >&"${IN[1]}"; sleep 0.2; }
  sleep 1
  say "m 2200 200"; say "down"; say "m 3300 900"; say "up"
  say "tap 49"
  for x in 2300 2400 2500 2600 2700; do say "m $x 400"; say "click"; done
  snap steps
  say "mods 4"; say "kdown 44"; sleep 2; say "kup 44"; say "mods 0"
  snap held
  say "tap 1"
  sleep 0.5
  exec {IN[1]}>&-
  ;;
noedit)
  (HOME="$OUT/overlay" timeout 30 $OPAL region --no-edit >"$OUT/overlay.log" 2>&1 &)
  coproc IN { $INJ; }
  say() { echo "$1" >&"${IN[1]}"; sleep 0.2; }
  sleep 1
  say "m 2000 100"; say "down"; say "m 2400 400"; say "up"
  sleep 1
  exec {IN[1]}>&-
  ;;
drag)
  (HOME="$OUT/overlay" timeout 30 $OPAL region >"$OUT/overlay.log" 2>&1 &)
  coproc IN { $INJ; }
  say() { echo "$1" >&"${IN[1]}"; sleep 0.25; }
  sleep 1
  say "m 2300 300"; say "down"; say "m 2600 500"; say "m 3100 800"
  snap dragging
  say "up"
  snap released
  say "tap 28"
  sleep 1
  exec {IN[1]}>&-
  ;;
esac
cat "$OUT/overlay.log"
find "$OUT" -name '*.png' | sort
