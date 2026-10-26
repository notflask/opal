# Opal: plan

Four goals, in this order:

1. A flat, square, pixel-crisp design that matches the `minimal` / `mono` desktop style.
2. Lower RAM use, faster response, fewer ways to fail.
3. A `config.toml` that covers everything, so Opal works on any system and with any matugen setup.
4. `opal zoom`: a zoom tool like tsoding/boomer, for Wayland.

Status: **implemented, waiting for review.** Nothing is committed yet. The sections
below are the plan as written; "Results" at the end says what was built, what
was measured, and where it differs from the plan.

---

## Where we start (measured on 2026-10-26)

- Release binary: 4.7 MB, 126 crates.
- Estimated RAM in `opal region` on this machine (eDP-1 1920x1080 + DP-2 2560x1440, scale 1):

  | what | size |
  |---|---|
  | capture pixmaps, converted from shm | 23 MB |
  | dimmed copy of every capture | 23 MB |
  | 2 shm buffers per output (`SlotPool`) | 46 MB |
  | cosmic-text font database (scans every installed font) | several MB, plus startup time |
  | **total** | **about 95 to 110 MB** |

- Every pointer motion re-renders the whole output on the CPU: copies the
  dimmed image, copies the selection, draws the annotations, and blurs the
  glass panels (box blur, 3 passes) every frame.
- At startup Opal spawns `hyprctl` (or `niri msg`) to find the focused output, and `date` to get the timestamp.
- The headless e2e test (`scripts/e2e.sh`) needs `sway` and `swaybg`, and neither is installed on this machine.
- The repo has no license file. This matters for Phase 3, which embeds a font (see "Risks").

---

## Phase 0: test harness and baseline numbers

The user installs sway (needs root): `sudo emerge -av gui-wm/sway gui-apps/swaybg`.

Tasks:

1. Run the existing `scripts/e2e.sh drag` and `annot` scenarios. Keep the output PNGs as the "before" reference.
2. Add an `rss` scenario. It runs `opal region` under `/usr/bin/time -v`, drags a selection, draws 20 shapes, and presses Enter. It records:
   - max RSS;
   - the `Shmem` line from `/proc/<pid>/status` mid-session, since shm buffers do not all show up in RSS.
3. Add timing points behind `OPAL_TRACE=1`:
   - process start until the first frame on each output;
   - Enter until the clipboard is ready;
   - per motion event: handler time and bytes touched.
4. Write the numbers into a "Measurements" table in this file. Every later phase adds a row.

Done when the baseline row exists and both existing scenarios pass.

## Phase 1: let the compositor do the compositing (the biggest win)

Opal stops drawing full frames. Each output's overlay becomes a small tree
of surfaces. The compositor already composites them on the GPU, so Opal does
not have to.

| layer | how | cost when the selection moves |
|---|---|---|
| background | the screencopy `wl_buffer`, attached directly: no copy, no format conversion. `y_invert` becomes `set_buffer_transform(flipped_180)` | none |
| dim | 4 subsurfaces around the selection. Each is a 1x1 ARGB buffer, stretched by `wp_viewporter` | 4 x (`set_position` + `set_destination`) |
| border | 4 subsurfaces of 1 px each, solid 1x1 buffer | same |
| handles | 8 small square solid subsurfaces (4 when the selection is small) | same |
| toolbar, size label, hint | small buffers of their own, re-rendered only when their content changes | none, unless the content changes |
| annotations | one ARGB buffer per output, the size of the selection on that output. Allocated only when you draw the first shape. Redrawn only inside the damaged rectangle; pen strokes are drawn incrementally | only while you draw |

Rules:

- Every subsurface is in sync mode, so each update is one atomic commit of the parent layer surface. Commits are throttled by frame callbacks, as today.
- Every rectangle is clipped to its own output before it is placed. Otherwise, a subsurface that sticks out of one output's overlay would render on the neighbouring monitor as well and dim it twice.
- An empty rectangle hides its subsurface by attaching a null buffer. `set_destination` with 0 is a protocol error.
- One 1x1 buffer per distinct color, shared by every subsurface that uses that color.
- Pixels are read on the CPU only when needed (crop, pixelate, export), straight from the mmap'd capture, converting only that region.
- A capture format that is not in the compositor's `wl_shm` format list gets converted once into `Xrgb8888`. This is the old path, kept only as a fallback.

Files:

| file | change |
|---|---|
| `capture.rs` | `Shot` keeps the `RawPool`, `wl_buffer`, format, stride and transform instead of a `Pixmap`. Add `Shot::read_rgba(rect) -> Pixmap`, which converts only `rect` |
| new `surface.rs` | `Solid` (a shared 1x1 buffer per color), `Quad` (subsurface + viewport + current rect, skips no-op updates), `Panel` (a subsurface with its own small double-buffered shm buffer) |
| `overlay.rs` | input handling and state stay. `render()` and `Target` are replaced by `layout()`, which moves quads and re-renders only the panels whose content changed |
| `annot.rs` | rendering targets the annotation buffer, with an offset transform. Pixelate reads through `Shot::read_rgba` |
| `output.rs` | `crop` uses `read_rgba` per output |

Expected result:

- RAM: the captures (23 MB here) plus the annotation buffer, about 30 MB in total.
- Moving or resizing the selection costs almost no CPU.

Done when:

- The e2e PNGs of the drag and annot scenarios match the "before" ones in content. Only the UI style is allowed to differ.
- Max RSS is at most 40 MB.
- During a drag, the motion handler takes under 0.2 ms on average.

## Phase 2: fewer dependencies, faster startup, fewer failures

**Text.** Remove `cosmic-text`.

- UI text uses the built-in pixel font (Phase 3).
- Annotation text: the first time the text tool is used, find the font file with `fc-match -f '%{file}' <draw.font>` and render it with `fontdue`.
- A missing glyph triggers `fc-match ':charset=<hex>'` for a fallback font. The result is cached per font file.
- No shaping. Latin, Cyrillic and symbols are fine without it. Arabic and Indic scripts would render unshaped; that is accepted.

**Focused output.** New file `ipc.rs` replaces `focus.rs`:

- Hyprland: write `j/monitors` to `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`.
- niri: write `"FocusedOutput"` to `$NIRI_SOCKET`.
- Both with a 100 ms timeout. On failure, fall back to the output under the pointer.
- `serde_json` stays, only to parse these replies.

**Timestamp.** Add `libc`: `localtime_r` + `strftime` with `output.filename` as the pattern. This removes the `date` process.

**UI drawing.** Integer rectangle fills and bitmap blits written directly into the panel buffers. `tiny-skia` stays only for annotation shapes.

**Finishing.**

- Encode the PNG once (compression `Fast`).
- Then hand it to the clipboard child and write the file in parallel.
- Print the path last (`output.print_path`).

**Robustness.**

| case | behaviour |
|---|---|
| second invocation | lock `$XDG_RUNTIME_DIR/opal.lock` (flock). If a first instance is open, the second sends it SIGUSR1 (the pid is stored in the lock file) and exits; the first cancels |
| output unplugged while open | `output_destroyed` cancels cleanly |
| layer surface closed | cancel (already done) |
| no keyboard focus granted | keep working with the pointer; Esc is unavailable, right click still cancels |
| missing protocol | one-line error naming it, exit code 2 |
| screencopy `Failed` | retry once, then exit with an error |
| `unwrap()` on compositor-controlled data | replace with errors |

**Release profile.** `lto = "fat"`, `panic = "abort"`, `codegen-units = 1`, `strip = true`.

Done when:

- `cargo tree` shows no `cosmic-text`, `fontdb`, `swash` or `rustybuzz`.
- Time to first frame is under 60 ms (measure the baseline first).
- No child process is spawned on the common path (no text tool, no notification).

## Phase 3: design

The style follows `quickshell/minibar`, `quickshell/mono`, `rofi/flat.rasi` and `mako-flat`.

**Tokens** (names as in matugen's `colors.css`):

| token | use | built-in fallback |
|---|---|---|
| `surface` | panel fill | `#131313` |
| `on_surface` | text, icons | `#e2e2e2` |
| `on_surface_variant` | inactive icons | `#c6c6c6` |
| `outline` | panel border, dividers | `#919191` |
| `surface_container_high` | hover | `#2a2a2a` |
| `primary` | selection border, handles, active tool, confirm | `#ffffff` |
| `on_primary` | icon on an active tool | `#1b1b1b` |
| `error` | cancel on hover | `#ffb4ab` |

**Shapes.**

- No blur, no shadows, no gradients, no rounded corners. All of it is integer pixel rectangles.
- Panels: `surface` fill with a 1 px `outline` border. The border width is `ui.border` logical px, rounded to whole physical px.
- Selection: a 1 px `primary` border. Handles are squares of 6x6 logical px, filled with `primary` and outlined 1 px in `surface`, so they stay visible on white.
- Dim: `#000000` at `ui.dim` alpha.

**Toolbar.**

- One row, 32 px high. Buttons are 28x28 with 2 px gaps and 1 px `outline` dividers between groups.
- Active tool: `primary` fill, `on_primary` icon. Hover: `surface_container_high`.
- Icons: 12x12 pixel-art bitmaps (1 bit per pixel, embedded), drawn at integer scale `max(1, round(k))`.
- Color swatches: 14x14 squares. The selected one gets a 1 px `on_surface` outline with a 1 px gap.
- Width buttons: 2, 4 and 8 px squares.

**Text.**

- Embedded bitmap font: Terminus 8x16 (`ter-u16n`, SIL OFL 1.1). It covers ASCII, Latin-1, Latin Extended-A, Cyrillic, `×` and `·`.
- `build.rs` turns the vendored BDF into a sorted table of glyphs plus a 16-byte bitmap per glyph. That is about 600 glyphs, around 10 KB in the binary.
- Rendering is a blit at integer scale, with no anti-aliasing.
- `ui.font = "<fontconfig name>"` switches UI text to fontdue instead.

**Size label and hint.**

- Size label: `2560x1440`, sitting on top of the selection's top-left corner, flush with the border.
- Hint: `drag to select · esc to cancel`, bottom center, lower-case like the bar.

**Annotations.** They stay anti-aliased by default. `draw.antialias = false` makes them crisp, through tiny-skia's `anti_alias = false`. The rect tool keeps square corners.

Files: `glass.rs` and `text.rs` are deleted. Added: `ui.rs` (rects, blits, toolbar, labels), `font/pixel.rs` + `build.rs` + `assets/ter-u16n.bdf` + `assets/OFL.txt`, and `font/ttf.rs` (fontdue). `theme.rs` is rewritten for the tokens above.

Done when:

- The e2e screenshots show the flat style.
- Grepping the UI code finds no `rrect`, `LinearGradient` or blur call.

## Phase 4: config.toml

`~/.config/opal/config.toml` (`$XDG_CONFIG_HOME` is respected). Every key is
optional and the defaults are shown below.

- An unknown key gives a warning on stderr that names the key and the line. Opal still starts.
- A wrong type gives a warning, and that key falls back to its default.
- `opal config` prints the full default file with comments. `opal config --check` validates the file and exits non-zero on errors.
- Old keys (`save_dir`, `notify`, `dim`, `[draw].color` as a name) keep working, mapped to the new ones.

```toml
[output]
save = true
save_dir = "~/Screenshots"
filename = "Screenshot from %Y-%m-%d %H-%M-%S.png"   # strftime
clipboard = true
notify = true
notify_cmd = ["notify-send", "-a", "opal", "-i", "{path}", "Screenshot copied", "{name}"]
print_path = true          # print the saved path on stdout

[ui]
font = "builtin"           # or a fontconfig name for UI text
border = 1                 # px
dim = 0.5                  # 0..1
hint = true
size_label = true
toolbar = "auto"           # auto | top | bottom

[theme]
# matugen output: a TOML file Opal ships a template for, or any
# @define-color CSS file (e.g. ~/.config/quickshell/taskbar/colors.css)
file = "~/.config/opal/colors.toml"
# per-key overrides win over the file
# surface = "#131313"   on_surface = "#e2e2e2"   on_surface_variant = "#c6c6c6"
# outline = "#919191"   surface_container_high = "#2a2a2a"
# primary = "#ffffff"   on_primary = "#1b1b1b"   error = "#ffb4ab"

[draw]
tool = "arrow"             # arrow line rect ellipse pen highlighter text pixelate step
color = 0                  # index into palette
width = 4
palette = ["#ff3b30", "#ff9500", "#ffd60a", "#30d158", "#0a84ff", "#ffffff", "#1c1c1e"]
font = "monospace"         # fontconfig name for the text tool
antialias = true
pixelate_block = 10

[keys]
# physical keys (evdev names), so they work on any layout
arrow = "a"   line = "l"   rect = "r"   ellipse = "e"   pen = "p"
highlighter = "h"   text = "t"   pixelate = "b"   step = "n"
confirm = ["Return", "ctrl+c"]   cancel = "Escape"
undo = "ctrl+z"   redo = ["ctrl+y", "ctrl+shift+z"]

[zoom]
# see Phase 5
```

**Theme lookup order:**

1. The `[theme]` overrides.
2. `theme.file`. Format by extension: `.toml` or `.css` (`@define-color`).
3. `~/.config/quickshell/taskbar/colors.css`, if it exists (what this machine uses today).
4. The built-in mono colors.

**matugen.** The repo ships `contrib/matugen/opal-colors.toml`, a template that uses `{{colors.<token>.default.hex}}` for each token. It also ships this snippet for matugen's `config.toml`:

```toml
[templates.opal]
input_path  = "~/.config/matugen/templates/opal-colors.toml"
output_path = "~/.config/opal/colors.toml"
```

Theme and config are read once at startup, because the overlay lives for only a few seconds. No file watching.

Files: `config.rs` is rewritten (serde with `deny_unknown_fields` off, plus a manual unknown-key pass over `toml::Table` for the warnings). `theme.rs` loads TOML and CSS. The README gets a full config reference.

Done when:

- `opal config | opal config --check -` passes. In other words, the printed default file validates as correct.
- Every key in the default file changes behaviour in an e2e or unit test.

## Phase 5: `opal zoom`

### Frozen mode (`opal zoom`), like boomer

- **Capture:** the output under the cursor. Ask the Hyprland or niri IPC for the cursor position; otherwise use the focused output.
- **Window:** one fullscreen layer-shell surface on that output, layer `overlay`, exclusive keyboard, namespace `opal-zoom`.
- **Zoom and pan:** the frozen screencopy buffer is attached once. Each frame only sets the `wp_viewporter` **source rectangle** (fixed point, 1/256 px precision). The **destination** is the output's logical size. The compositor scales on the GPU, so Opal does no pixel work.

Viewporter rules (from the protocol) and what Opal does about them:

| rule | what Opal does |
|---|---|
| the source must lie inside the buffer, or the client gets the `out_of_buffer` error | clamp the source rectangle inside the buffer before every commit |
| width and height must be > 0 | always true, since zoom ≥ 1 |
| zooming out (scale < 1) would need a source bigger than the buffer | `zoom.min` is ≥ 1.0. Boomer's `min_scale` < 1 is not supported |
| the destination is whole logical pixels | always the output size, so it holds |

**Motion.** Boomer's model:

- Each frame: `scale += scale_velocity * dt` and `position += velocity * dt`.
- Both velocities decay by `exp(-friction * dt)`.
- Zoom keeps the point under the cursor fixed.
- The loop runs on frame callbacks only while some velocity is above epsilon. When idle, Opal does not wake up at all.

**Flashlight.**

- A subsurface holding a mask of size 2 x the output: `#000` at `flashlight_dim` alpha, with a round hole of radius `flashlight_radius` in the center. It is rendered once.
- `set_position` keeps the hole centered on the pointer.
- Ctrl+wheel changes the radius at once through the viewport destination (a slightly soft edge). The mask is re-rendered crisp 150 ms after the last change.

**Filter.**

- `filter = "linear"` (default): viewporter scaling as above. No extra RAM.
- `filter = "nearest"`: the CPU renders the visible part into an output-sized buffer from a 2-buffer pool:
  - x lookup table per frame;
  - one row computed per distinct source row, then copied for the duplicate rows.
  - Cost: about 2 to 4 ms per moving frame at 1440p, and 2 x 14.7 MB of RAM while zooming.
  - Above `nearest_from` (default 2.0) the CPU path is used; below it, linear.

**Keys** (boomer defaults):

| input | action |
|---|---|
| wheel, `=` / `-` | zoom toward the cursor |
| left drag | pan, with inertia after release |
| `F` | toggle the flashlight |
| Ctrl + wheel | flashlight radius |
| `0` | reset |
| `C` | copy the current view to the clipboard (PNG, at the visible scale) |
| `S` | save the current view as PNG (same naming as screenshots) |
| Esc, `Q`, right click | quit |

Boomer's `m` (mirror) and `r` (reload config) are left out.

### Live mode (`opal zoom --live`), Hyprland only

Hyprland 0.56.2 has `cursor:zoom_factor` (read-only check on this machine:
`hyprctl getoption cursor:zoom_factor` gives `1.0`), plus the `keyword` and
`eval` (Lua) socket commands.

- **Setting the zoom:** try `eval hl.config({cursor={zoom_factor=X}})` first, because this machine uses the Lua config. If that is rejected, fall back to `keyword cursor:zoom_factor X`. Which one works must be tested in the headless Hyprland or by the user, never on the live session without asking.
- **Input:** a transparent fullscreen layer surface catches the wheel, Esc, and `0`. Hyprland's zoom follows the cursor by itself. Clicks are swallowed while the mode is active. `cursor:zoom_rigid` decides how the view follows the cursor.
- **On exit:** restore the old `zoom_factor`, read at start with `getoption`. On SIGTERM and SIGINT too.
- **One-shot form for binds, with no overlay:** `opal zoom --live in|out|reset`.
- **Not Hyprland:** a clear error ("live zoom needs Hyprland").

### Config

```toml
[zoom]
min = 1.0
max = 32.0
scroll_speed = 1.5        # boomer's scroll_speed
drag_friction = 6.0       # boomer's drag_friction
scale_friction = 4.0      # boomer's scale_friction
smooth = true             # false: jump straight to the target, no inertia
filter = "linear"         # linear (GPU) | nearest (CPU)
nearest_from = 2.0
flashlight = false        # start with the flashlight on
flashlight_radius = 200
flashlight_dim = 0.75
live_step = 0.5           # zoom_factor change per notch in --live
```

Files:

- new `zoom.rs` (frozen) and `zoom_live.rs`;
- `main.rs` gets the `zoom` subcommand;
- `capture.rs` gains `capture_output(name)`;
- e2e gets a `zoom` scenario: wheel x5, drag, and `F`, with checks that it opens and closes and that RSS ≤ capture + 5 MB.

Done when:

- Zooming and panning on the 1440p output show no dropped frames.
- An idle zoom session uses 0% CPU (no wakeups in `strace -c`).
- RSS with `linear` is about the capture size.

---

## Risks

- **Font license.** Terminus is under the OFL. It can be embedded, but the license text must ship (`assets/OFL.txt`, and the README says so). Opal has no license of its own yet. The user should pick one; MIT would be the default.
- **Fractional scale.** Subsurface positions and sizes are whole logical px. At scale 1.25, a 1-logical-px border is 1.25 physical px and may look soft. Fix: use `wp_fractional_scale_v1` and size the border subsurfaces so they come out as whole physical pixels. This does not matter on this machine (scale 1).
- **Attaching the capture buffer directly.** If a compositor rejects a screencopy format as a surface buffer, Opal uses the one-time conversion fallback.
- **The headless sway uses the pixman renderer.** It supports viewporter scaling and subsurfaces. Hyprland-specific behaviour, like the layer rule and live zoom, still needs one manual check by the user.
- **Hyprland `keyword` under the Lua config.** It may be ignored. `eval` is the planned path; untested.

## Order of work

1. Phase 0: harness and baseline.
2. Phase 1: subsurfaces. The architecture change, and most of the RAM win.
3. Phase 2: dependencies and robustness.
4. Phase 3: design.
5. Phase 4: config.
6. Phase 5: zoom, frozen first, then `--live`.

Each phase ends with its "done when" checks plus a row in the measurement
table, and stops for review. Nothing is committed until the user says so.

## Measurements

Measured on the dev machine (eDP-1 1920x1080 + DP-2 2560x1440, scale 1), release builds,
Hyprland 0.56.2.

| | before | after |
|---|---|---|
| `opal region` process RSS | 78.4 MB (47.7 anon + 26.6 shm) | 5.6 MB |
| system memory taken while open (MemAvailable) | about 100 MB | about 28 MB (5.6 + 22 MB capture shm) |
| capture to first frame | 55 ms | 19 ms |
| per-frame cost while dragging | full 4480x1440 redraw | 7 to 120 µs (rectangles only) |
| binary | 4.77 MB | 2.18 MB |
| crates | 126 | 100 |
| `opal output --no-edit`, wall / peak RSS | not measured (it copies to the clipboard) | 62 ms / 46.6 MB |
| `opal all --no-edit`, wall / peak RSS | not measured | 148 ms / 78.8 MB |
| `opal zoom`, linear: RSS / extra shm / frame | n/a | 5.9 MB / 14 MB / 8 µs |
| `opal zoom`, nearest: RSS / extra shm / frame | n/a | 43 MB / 42 MB / 2.2 ms median, 9 ms max |

"After" RSS counts only what Opal touches; the capture memory the compositor wrote is the
22 MB (both monitors) or 14 MB (one monitor) of shared memory next to it.

## Open questions

All four are answered (below). Decided by default (say so if you want something else):

- Annotations stay anti-aliased (`draw.antialias`).
- A second `opal` started while one is open cancels the first.
- Frozen zoom captures only the output under the cursor.
- Old config keys keep working.
- Nothing is committed until the user says so.
- Still open for the user: a license for Opal (needed because a font is embedded).

## Answers

Answered 2026-10-26:

- **Q1, UI font:** built-in pixel bitmap font (Latin and Cyrillic, integer scale, no AA). `ui.font` can switch to a fontconfig font.
- **Q2, zoom filter:** both. `linear` (GPU, viewporter) is the default; `filter = "nearest"` turns on the CPU path.
- **Q3, zoom mode:** frozen like boomer, plus `opal zoom --live` on Hyprland, which drives `cursor:zoom_factor` over IPC.
- **Q4, testing:** install sway and swaybg for the headless e2e scripts. (Sway could not be installed; the user later allowed testing on the live Hyprland session, see Results.)

## Results

Built as planned, with these differences:

- **Testing:** sway could not be installed (it needs root), so the headless e2e scripts were
  not run. The user then allowed testing on their live Hyprland session: overlays were
  launched there, a virtual pointer drove them, `grim` captured the result, and the pixels
  were checked (border exact to the pixel, dim ratios, flashlight ratios, nearest zoom
  identical to a point-resampled baseline). There are 29 unit tests.
- **Hyprland quirks found and worked around** (details in the README): a 1 px subsurface is
  drawn 2 px thick, and a changed viewport size on a mapped subsurface is ignored unless the
  buffer is attached again. A subsurface also misbehaves when its parent has a viewport
  source rectangle, so the zoomed picture is a child subsurface and the parent is a plain
  transparent surface.
- **Flashlight:** only a toggle inside `opal zoom` (`F`), as asked; no separate spotlight
  command. Its edge is made of squares (`flashlight_pixel`, default 8). Alt or Ctrl + wheel
  sizes it.
- **`ui.font` was dropped:** UI text always uses the built-in pixel font. `draw.font` still
  selects the font of the text tool.
- **Capture and overlay share one Wayland connection**, which is what lets the capture buffer
  be the background without a copy. `opal output --no-edit` captures only the focused monitor.
- **`config --check`** names the key of a problem, not the line number.
- **Not done:** pixel snapping of the border on fractional scales (no such monitor here),
  the `[keys]` table does not cover the zoom keys.
- **`opal zoom --live` was removed again** (the Hyprland `cursor:zoom_factor` mode). Hyprland pans
  that zoom with the pointer only: no drag, no flashlight, no way for Opal to know where the
  pointer is in the zoomed picture. Only the frozen `opal zoom` remains.
