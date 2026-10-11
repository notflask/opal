#!/usr/bin/env bash
# Start a headless sway with two outputs laid out like the dev machine
# (1920x1080 at 0,0 and 2560x1440 at 1920,0).
# Prints `export` lines:  eval "$(scripts/headless-sway.sh)"
# Stop it with:           kill $SWAY_PID
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
before=$(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$' || true)
conf=$(mktemp /tmp/opal-sway-conf.XXXXXX)
cat "$here/headless-sway.conf" ${OPAL_SWAY_EXTRA:+"$OPAL_SWAY_EXTRA"} >"$conf"
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
  WAYLAND_DISPLAY= sway -c "$conf" >/tmp/opal-sway.log 2>&1 &
pid=$!
s=
for _ in $(seq 100); do
  s=$(comm -13 <(echo "$before" | sort) <(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$' | sort) | head -1)
  [ -n "$s" ] && break
  sleep 0.1
done
[ -n "$s" ] || { echo "sway did not start, see /tmp/opal-sway.log" >&2; exit 1; }
sleep 0.3
ipc=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*."$pid".sock)
export WAYLAND_DISPLAY=$s SWAYSOCK=$ipc
swaymsg create_output >/dev/null
swaymsg output HEADLESS-1 mode 1920x1080 pos 0 0 >/dev/null
swaymsg output HEADLESS-2 mode 2560x1440 pos 1920 0 >/dev/null
echo "export WAYLAND_DISPLAY=$s SWAYSOCK=$ipc SWAY_PID=$pid"
