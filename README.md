# opal

Screenshot and zoom tool for Wayland (wlroots-style compositors: Hyprland, Niri, Sway).
Flat, square, pixel-crisp UI that takes its colors from matugen.

```
opal region        drag an area (can span monitors), draw on it, Enter copies + saves
opal output        the focused monitor
opal all           every monitor, stitched by their layout
opal <mode> --no-edit
                   skip the editor: output/all save at once, region saves on release
opal zoom          freeze the screen, zoom and pan it (like tsoding/boomer)
opal config        print the default config.toml
opal config --check [FILE]
                   validate a config file, exit 1 on problems
```

Running `opal region` (or `zoom`) a second time closes the first one, so one key toggles it.

Needs `wlr-screencopy`, `wlr-layer-shell`, `wp-viewporter`. Focused-monitor detection uses
the Hyprland or niri socket. `notify-send` shows the "copied" notification, `fc-match`
(fontconfig) finds the font for the text tool.

## Screenshot keys

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

Shortcuts use the physical key, so they work on any keyboard layout, and all of them can be
rebound in `[keys]`. Text: Enter is a new line, Esc finishes it.

## Zoom keys

| Input | Action |
|---|---|
| wheel, `=` / `-` | zoom toward the pointer |
| left drag | pan (with inertia) |
| `F` | toggle the flashlight: everything dark except a pixel-art circle around the pointer |
| Alt + wheel (or Ctrl + wheel) | size of the flashlight |
| `0` | reset |
| `C` | copy the current view as PNG and quit |
| `S` | save the current view as PNG and quit |
| Esc, `Q`, right click | quit |

Zooming moves the `wp_viewporter` source rectangle, so the compositor scales on the GPU and
Opal does no pixel work (about 8 µs per frame). `filter = "nearest"` renders the visible part
on the CPU instead, for crisp pixels (about 2 ms per frame and two extra screen-sized buffers).

## Config

`~/.config/opal/config.toml` (`$XDG_CONFIG_HOME` is respected), every key optional.
`opal config` prints the whole file with the defaults and comments. Unknown keys and bad
values give a warning on stderr and the default is used.

```toml
[output]
save_dir = "~/Screenshots"
filename = "Screenshot from %Y-%m-%d %H-%M-%S.png"   # strftime
clipboard = true
notify = true
notify_cmd = ["notify-send", "-a", "opal", "-i", "{path}", "Screenshot copied", "{name}"]

[ui]
border = 1      # px
dim = 0.5       # how dark everything outside the selection gets
toolbar = "auto"  # auto | top | bottom

[draw]
tool = "arrow"
color = 0       # index into palette
width = 4
palette = ["#ff3b30", "#ff9500", "#ffd60a", "#30d158", "#0a84ff", "#ffffff", "#1c1c1e"]
font = "sans-serif:bold"   # fontconfig pattern for the text tool, or "builtin"
antialias = true

[keys]
undo = "ctrl+z"
confirm = ["Return", "ctrl+c"]

[zoom]
filter = "linear"        # or "nearest"
flashlight_radius = 200
flashlight_pixel = 8     # size of the squares that make the circle's edge
```

The older top-level `save_dir`, `notify`, `dim` and `color = "red"` keys still work.

### Colors and matugen

The UI uses eight colors named like matugen's Material tokens: `surface`, `on_surface`,
`on_surface_variant`, `outline`, `surface_container_high`, `primary`, `on_primary`, `error`.
They are read, later ones winning, from

1. the built-in dark palette,
2. `~/.config/quickshell/taskbar/colors.css` if it exists (`@define-color` lines),
3. `theme.file` (default `~/.config/opal/colors.toml`; a `.css` file works too),
4. the `[theme]` overrides in `config.toml`.

To let matugen write the file, copy `contrib/matugen/opal-colors.toml` to
`~/.config/matugen/templates/` and add `contrib/matugen/config-snippet.toml` to your matugen
`config.toml`. Colors are read once at startup.

## Hyprland

Add a layer rule so the overlays do not animate:

```lua
hl.layer_rule({ name = "opal", match = { namespace = "^opal" }, no_anim = true })
```

Bind it, for example:

```lua
bind("Print", exec("opal region"))
bind("SUPER + Z", exec("opal zoom"))
```

## Development

```
cargo build
cargo test                                  # unit tests
OPAL_PREVIEW_DIR=/tmp cargo test preview    # renders the toolbar to /tmp/ui-1x.png, ui-2x.png
OPAL_TRACE=1 opal region                    # timings and input events on stderr
```

Dev hooks, only for tests: `OPAL_PRESET=x,y,w,h` starts `region` with that selection,
`OPAL_OUTPUT=DP-2` picks the output `zoom` captures, `OPAL_ZOOM_VIEW=scale,x,y` starts `zoom`
at that view.

`scripts/e2e.sh` drives the overlay in a headless sway (needs sway, swaybg, imagemagick).
`examples/inject.rs` injects pointer, wheel and keyboard events into a compositor; run it
only against a test session or when you accept the cursor moving.

### Compositor notes

Opal builds the overlay out of subsurfaces (dim bars, border, handles, annotation layer,
toolbar), so moving the selection only moves rectangles. Two Hyprland quirks are worked
around: a subsurface that is 1 px thick is drawn 2 px thick (the border uses a 2 px buffer
with a transparent second pixel), and a new `wp_viewport` size on an already mapped
subsurface is ignored unless the buffer is attached again.

## Credits

The built-in pixel font is [Terminus](https://terminus-font.sourceforge.net/) 8x16 by Dimitar
Toshkov Zhekov, under the SIL Open Font License 1.1 (`assets/OFL.txt`).
