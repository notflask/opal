# opal

Screenshot tool for Wayland (wlroots-style compositors: Hyprland, Niri, Sway).
Capture, draw on it, and Enter copies to the clipboard and saves a PNG.
The UI is "liquid glass": blurred, tinted panels that take their colors from
the matugen files the Quickshell taskbar uses (`~/.config/quickshell/taskbar/{colors,glass}.css`).

```
opal region        drag an area (can span monitors)
opal output        the focused monitor
opal all           every monitor, stitched by their layout
opal <mode> --no-edit
                   skip the editor: output/all save at once, region saves on release
```

Needs `wlr-screencopy`, `wlr-layer-shell`, `wp-viewporter`; focused-monitor
detection uses `hyprctl` or `niri msg`.

## Keys

| Key | Action |
|---|---|
| drag | new selection; handles/edges resize it |
| Space + drag | move the selection |
| Enter, Ctrl+C | copy + save, then quit |
| Esc | cancel (while typing text: finish the text) |
| right click | clear the selection (twice: cancel) |
| Ctrl+Z | undo |
| Ctrl+Y, Ctrl+Shift+Z | redo |
| mouse wheel | stroke width |
| A L R E P H T B N | arrow, line, rectangle, ellipse, pen, highlighter, text, blur (pixelate), numbered step |
| 1-7 | color |

Shortcuts use the physical key, so they work on any keyboard layout.
Text: Enter is a new line, Esc finishes it.

## Config

`~/.config/opal/config.toml`, every key optional:

```toml
save_dir = "~/Screenshots"
notify = true
dim = 0.42

[draw]
tool = "arrow"   # arrow line rect ellipse pen highlighter text pixelate step
color = "red"    # red orange yellow green blue white black
width = 4
```

## Hyprland

Add a layer rule so the overlay does not animate:

```lua
hl.layer_rule({ name = "opal", match = { namespace = "^opal$" }, no_anim = true })
```

## Development

```
nix develop
cargo build
scripts/e2e.sh annot     # headless sway + virtual pointer/keyboard, needs sway, swaybg, imagemagick
OPAL_TRACE=1 opal region # timings and input events on stderr
```

`examples/inject.rs` injects pointer and keyboard events (evdev codes) into a
compositor. Run it only against the headless sway, never your real session.
