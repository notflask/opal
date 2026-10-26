//! `~/.config/opal/config.toml`. Every key is optional. A bad value or an
//! unknown key produces a warning and the default is used; Opal still starts.

use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::{annot::Tool, theme::{Rgb, TOKENS}};

pub const DEFAULT_TOML: &str = r##"# Opal config. Every key is optional; these are the defaults.

[output]
save = true
save_dir = "~/Screenshots"
filename = "Screenshot from %Y-%m-%d %H-%M-%S.png"   # strftime
clipboard = true
notify = true
# {path} is the saved file, {name} its file name
notify_cmd = ["notify-send", "-a", "opal", "-i", "{path}", "Screenshot copied", "{name}"]
print_path = true          # print the saved path on stdout

[ui]
border = 1                 # px
dim = 0.5                  # 0..1, how dark everything outside the selection gets
hint = true                # "drag to select" text before the first drag
size_label = true          # 2560x1440 above the selection
toolbar = "auto"           # auto | top | bottom

[theme]
# Colors: matugen writes this file (see contrib/matugen). A quickshell
# colors.css with @define-color lines works too (any file ending in .css).
file = "~/.config/opal/colors.toml"
# Per-key overrides win over the file:
# surface = "#131313"
# on_surface = "#e2e2e2"
# on_surface_variant = "#c6c6c6"
# outline = "#919191"
# surface_container_high = "#2a2a2a"
# primary = "#ffffff"
# on_primary = "#1b1b1b"
# error = "#ffb4ab"

[draw]
tool = "arrow"             # arrow line rect ellipse pen highlighter text pixelate step
color = 0                  # index into palette
width = 4                  # px
palette = ["#ff3b30", "#ff9500", "#ffd60a", "#30d158", "#0a84ff", "#ffffff", "#1c1c1e"]
font = "sans-serif:bold"   # fontconfig pattern for the text tool, or "builtin"
antialias = true
pixelate_block = 10        # px

[keys]
# Physical keys (evdev names), so they work on any keyboard layout.
# Modifiers: ctrl, shift, alt. A list binds several chords.
arrow = "a"
line = "l"
rect = "r"
ellipse = "e"
pen = "p"
highlighter = "h"
text = "t"
pixelate = "b"
step = "n"
confirm = ["Return", "ctrl+c"]
cancel = "Escape"
undo = "ctrl+z"
redo = ["ctrl+y", "ctrl+shift+z"]

[zoom]
min = 1.0
max = 32.0
scroll_speed = 1.5         # zoom per wheel notch
drag_friction = 6.0        # how fast panning slows down after a drag
scale_friction = 4.0       # how fast zooming slows down after scrolling
smooth = true              # false: no inertia
filter = "linear"          # linear (GPU) | nearest (CPU, crisp pixels)
nearest_from = 2.0         # with filter = "nearest": zoom from which it applies
flashlight = false         # start with the flashlight on
flashlight_radius = 200
flashlight_dim = 0.75
flashlight_pixel = 8       # edge of the light circle is made of squares this big (px); 1 = fine
"##;

#[derive(Clone, Debug)]
pub struct Output {
    pub save: bool,
    pub save_dir: String,
    pub filename: String,
    pub clipboard: bool,
    pub notify: bool,
    pub notify_cmd: Vec<String>,
    pub print_path: bool,
}

#[derive(Clone, Debug)]
pub struct Ui {
    pub border: u32,
    pub dim: f32,
    pub hint: bool,
    pub size_label: bool,
    pub toolbar: String,
}

#[derive(Clone, Debug, Default)]
pub struct ThemeCfg {
    pub file: Option<String>,
    pub tokens: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct Draw {
    pub tool: Tool,
    pub color: usize,
    pub width: f32,
    pub palette: Vec<[u8; 3]>,
    pub font: String,
    pub antialias: bool,
    pub pixelate_block: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    Tool(Tool),
    Confirm,
    Cancel,
    Undo,
    Redo,
}

/// One key combination, matched on the physical key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub codes: Vec<u32>,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    pub fn matches(&self, code: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
        self.codes.contains(&code) && self.ctrl == ctrl && self.shift == shift && self.alt == alt
    }
}

#[derive(Clone, Debug)]
pub struct Keys(pub Vec<(Cmd, Vec<Chord>)>);

impl Keys {
    pub fn lookup(&self, code: u32, ctrl: bool, shift: bool, alt: bool) -> Option<Cmd> {
        self.0
            .iter()
            .find(|(_, cs)| cs.iter().any(|c| c.matches(code, ctrl, shift, alt)))
            .map(|(cmd, _)| *cmd)
    }
}

#[derive(Clone, Debug)]
pub struct Zoom {
    pub min: f32,
    pub max: f32,
    pub scroll_speed: f32,
    pub drag_friction: f32,
    pub scale_friction: f32,
    pub smooth: bool,
    pub nearest: bool,
    pub nearest_from: f32,
    pub flashlight: bool,
    pub flashlight_radius: f32,
    pub flashlight_dim: f32,
    pub flashlight_pixel: f32,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub output: Output,
    pub ui: Ui,
    pub theme: ThemeCfg,
    pub draw: Draw,
    pub keys: Keys,
    pub zoom: Zoom,
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    config_dir().join("opal/config.toml")
}

/// `~/x` -> `$HOME/x`.
pub fn expand(p: &str) -> PathBuf {
    // `~/.config/...` follows XDG_CONFIG_HOME
    if let Some(rest) = p.strip_prefix("~/.config/") {
        return config_dir().join(rest);
    }
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Config {
    /// The user's config, or the defaults. Warnings go to stderr.
    pub fn load() -> Self {
        let path = config_path();
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default() };
        let (cfg, warnings) = Self::parse(&text);
        for w in warnings {
            eprintln!("opal: {}: {w}", path.display());
        }
        cfg
    }

    /// Parse `text` on top of the defaults. Returns every problem found.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut w = Vec::new();
        let mut table = match text.parse::<toml::Table>() {
            Ok(t) => t,
            Err(e) => {
                w.push(format!("not valid TOML, using defaults: {}", e.message()));
                toml::Table::new()
            }
        };
        migrate_legacy(&mut table);
        let mut c = Self::builtin();
        c.apply(&table, &mut w);
        (c, w)
    }

    /// Values that do not come from parsing `DEFAULT_TOML` (which would recurse).
    fn builtin() -> Self {
        Self {
            output: Output {
                save: true,
                save_dir: "~/Screenshots".into(),
                filename: "Screenshot from %Y-%m-%d %H-%M-%S.png".into(),
                clipboard: true,
                notify: true,
                notify_cmd: ["notify-send", "-a", "opal", "-i", "{path}", "Screenshot copied", "{name}"]
                    .map(String::from)
                    .to_vec(),
                print_path: true,
            },
            ui: Ui {
                border: 1,
                dim: 0.5,
                hint: true,
                size_label: true,
                toolbar: "auto".into(),
            },
            theme: ThemeCfg { file: Some("~/.config/opal/colors.toml".into()), tokens: Vec::new() },
            draw: Draw {
                tool: Tool::Arrow,
                color: 0,
                width: 4.0,
                palette: DEFAULT_PALETTE.to_vec(),
                font: "sans-serif:bold".into(),
                antialias: true,
                pixelate_block: 10.0,
            },
            keys: Keys(default_keys()),
            zoom: Zoom {
                min: 1.0,
                max: 32.0,
                scroll_speed: 1.5,
                drag_friction: 6.0,
                scale_friction: 4.0,
                smooth: true,
                nearest: false,
                nearest_from: 2.0,
                flashlight: false,
                flashlight_radius: 200.0,
                flashlight_dim: 0.75,
                flashlight_pixel: 8.0,
            },
        }
    }

    fn apply(&mut self, t: &toml::Table, w: &mut Vec<String>) {
        check_keys(t, &["output", "ui", "theme", "draw", "keys", "zoom"], "", w);

        if let Some(s) = section(t, "output", w) {
            check_keys(s, &["save", "save_dir", "filename", "clipboard", "notify", "notify_cmd", "print_path"], "output.", w);
            let o = &mut self.output;
            take(s, "save", "output.", &mut o.save, w);
            take(s, "save_dir", "output.", &mut o.save_dir, w);
            take(s, "filename", "output.", &mut o.filename, w);
            take(s, "clipboard", "output.", &mut o.clipboard, w);
            take(s, "notify", "output.", &mut o.notify, w);
            take(s, "notify_cmd", "output.", &mut o.notify_cmd, w);
            take(s, "print_path", "output.", &mut o.print_path, w);
        }

        if let Some(s) = section(t, "ui", w) {
            check_keys(s, &["border", "dim", "hint", "size_label", "toolbar"], "ui.", w);
            let u = &mut self.ui;
            take(s, "border", "ui.", &mut u.border, w);
            take(s, "dim", "ui.", &mut u.dim, w);
            take(s, "hint", "ui.", &mut u.hint, w);
            take(s, "size_label", "ui.", &mut u.size_label, w);
            take(s, "toolbar", "ui.", &mut u.toolbar, w);
            u.border = u.border.clamp(1, 8);
            u.dim = u.dim.clamp(0.0, 0.95);
            if !["auto", "top", "bottom"].contains(&u.toolbar.as_str()) {
                w.push(format!("ui.toolbar: {:?} is not auto, top or bottom", u.toolbar));
                u.toolbar = "auto".into();
            }
        }

        if let Some(s) = section(t, "theme", w) {
            let mut known = vec!["file"];
            known.extend(TOKENS);
            check_keys(s, &known, "theme.", w);
            take_opt(s, "file", "theme.", &mut self.theme.file, w);
            for tok in TOKENS {
                if let Some(v) = s.get(tok) {
                    match v.as_str().filter(|v| Rgb::parse(v).is_some()) {
                        Some(v) => self.theme.tokens.push((tok.to_owned(), v.to_owned())),
                        None => w.push(format!("theme.{tok}: expected a color like \"#rrggbb\"")),
                    }
                }
            }
        }

        if let Some(s) = section(t, "draw", w) {
            check_keys(s, &["tool", "color", "width", "palette", "font", "antialias", "pixelate_block"], "draw.", w);
            let d = &mut self.draw;
            let mut name = String::new();
            if take(s, "tool", "draw.", &mut name, w) {
                match Tool::from_name(&name) {
                    Some(tool) => d.tool = tool,
                    None => w.push(format!("draw.tool: unknown tool {name:?}")),
                }
            }
            let mut palette: Vec<String> = Vec::new();
            if take(s, "palette", "draw.", &mut palette, w) {
                let parsed: Vec<_> = palette.iter().filter_map(|c| Rgb::parse(c)).collect();
                if parsed.len() != palette.len() || parsed.is_empty() {
                    w.push("draw.palette: expected a non-empty list of \"#rrggbb\" colors".into());
                } else {
                    d.palette = parsed.iter().map(|c| [c.0, c.1, c.2]).collect();
                }
            }
            take(s, "width", "draw.", &mut d.width, w);
            take(s, "font", "draw.", &mut d.font, w);
            take(s, "antialias", "draw.", &mut d.antialias, w);
            take(s, "pixelate_block", "draw.", &mut d.pixelate_block, w);
            d.width = d.width.clamp(1.0, 24.0);
            d.pixelate_block = d.pixelate_block.clamp(2.0, 64.0);
            match s.get("color") {
                Some(toml::Value::Integer(i)) if (*i as usize) < d.palette.len() && *i >= 0 => {
                    d.color = *i as usize
                }
                Some(toml::Value::Integer(i)) => {
                    w.push(format!("draw.color: {i} is outside the palette (0..{})", d.palette.len() - 1))
                }
                Some(toml::Value::String(c)) => match Rgb::parse(c) {
                    Some(rgb) => {
                        let rgb = [rgb.0, rgb.1, rgb.2];
                        d.color = d.palette.iter().position(|p| *p == rgb).unwrap_or_else(|| {
                            d.palette.push(rgb);
                            d.palette.len() - 1
                        });
                    }
                    None => w.push(format!("draw.color: {c:?} is neither an index nor a \"#rrggbb\" color")),
                },
                Some(_) => w.push("draw.color: expected an index into palette".into()),
                None => {}
            }
        }

        if let Some(s) = section(t, "keys", w) {
            let names: Vec<&str> = KEY_CMDS.iter().map(|(n, _)| *n).collect();
            check_keys(s, &names, "keys.", w);
            for (name, cmd) in KEY_CMDS {
                let Some(v) = s.get(*name) else { continue };
                let list: Vec<String> = match v {
                    toml::Value::String(one) => vec![one.clone()],
                    other => match other.clone().try_into() {
                        Ok(l) => l,
                        Err(_) => {
                            w.push(format!("keys.{name}: expected a string or a list of strings"));
                            continue;
                        }
                    },
                };
                let mut chords = Vec::new();
                for c in &list {
                    match parse_chord(c) {
                        Some(ch) => chords.push(ch),
                        None => w.push(format!("keys.{name}: cannot read {c:?}")),
                    }
                }
                if let Some(slot) = self.keys.0.iter_mut().find(|(c, _)| c == cmd) {
                    slot.1 = chords;
                }
            }
        }

        if let Some(s) = section(t, "zoom", w) {
            check_keys(
                s,
                &["min", "max", "scroll_speed", "drag_friction", "scale_friction", "smooth", "filter", "nearest_from", "flashlight", "flashlight_radius", "flashlight_dim", "flashlight_pixel"],
                "zoom.",
                w,
            );
            let z = &mut self.zoom;
            take(s, "min", "zoom.", &mut z.min, w);
            take(s, "max", "zoom.", &mut z.max, w);
            take(s, "scroll_speed", "zoom.", &mut z.scroll_speed, w);
            take(s, "drag_friction", "zoom.", &mut z.drag_friction, w);
            take(s, "scale_friction", "zoom.", &mut z.scale_friction, w);
            take(s, "smooth", "zoom.", &mut z.smooth, w);
            take(s, "nearest_from", "zoom.", &mut z.nearest_from, w);
            take(s, "flashlight", "zoom.", &mut z.flashlight, w);
            take(s, "flashlight_radius", "zoom.", &mut z.flashlight_radius, w);
            take(s, "flashlight_dim", "zoom.", &mut z.flashlight_dim, w);
            take(s, "flashlight_pixel", "zoom.", &mut z.flashlight_pixel, w);
            let mut filter = String::new();
            if take(s, "filter", "zoom.", &mut filter, w) {
                match filter.as_str() {
                    "linear" => z.nearest = false,
                    "nearest" => z.nearest = true,
                    _ => w.push(format!("zoom.filter: {filter:?} is not linear or nearest")),
                }
            }
            // below 1.0 the viewporter source rectangle would leave the buffer
            z.min = z.min.max(1.0);
            z.max = z.max.max(z.min);
            z.scroll_speed = z.scroll_speed.clamp(0.1, 10.0);
            z.flashlight_dim = z.flashlight_dim.clamp(0.0, 1.0);
            z.flashlight_pixel = z.flashlight_pixel.clamp(1.0, 64.0);
            z.flashlight_radius = z.flashlight_radius.clamp(10.0, 4000.0);
        }
    }
}

pub const DEFAULT_PALETTE: [[u8; 3]; 7] = [
    [0xff, 0x3b, 0x30],
    [0xff, 0x95, 0x00],
    [0xff, 0xd6, 0x0a],
    [0x30, 0xd1, 0x58],
    [0x0a, 0x84, 0xff],
    [0xff, 0xff, 0xff],
    [0x1c, 0x1c, 0x1e],
];

/// Names in the keys table, in lookup order.
const KEY_CMDS: &[(&str, Cmd)] = &[
    ("arrow", Cmd::Tool(Tool::Arrow)),
    ("line", Cmd::Tool(Tool::Line)),
    ("rect", Cmd::Tool(Tool::Rect)),
    ("ellipse", Cmd::Tool(Tool::Ellipse)),
    ("pen", Cmd::Tool(Tool::Pen)),
    ("highlighter", Cmd::Tool(Tool::Highlighter)),
    ("text", Cmd::Tool(Tool::Text)),
    ("pixelate", Cmd::Tool(Tool::Pixelate)),
    ("step", Cmd::Tool(Tool::Step)),
    ("confirm", Cmd::Confirm),
    ("cancel", Cmd::Cancel),
    ("undo", Cmd::Undo),
    ("redo", Cmd::Redo),
];

fn default_keys() -> Vec<(Cmd, Vec<Chord>)> {
    let c = |s: &str| parse_chord(s).expect("built-in chord");
    let tool = |t, k: &str| (Cmd::Tool(t), vec![c(k)]);
    vec![
        tool(Tool::Arrow, "a"),
        tool(Tool::Line, "l"),
        tool(Tool::Rect, "r"),
        tool(Tool::Ellipse, "e"),
        tool(Tool::Pen, "p"),
        tool(Tool::Highlighter, "h"),
        tool(Tool::Text, "t"),
        tool(Tool::Pixelate, "b"),
        tool(Tool::Step, "n"),
        (Cmd::Confirm, vec![c("Return"), c("ctrl+c")]),
        (Cmd::Cancel, vec![c("Escape")]),
        (Cmd::Undo, vec![c("ctrl+z")]),
        (Cmd::Redo, vec![c("ctrl+y"), c("ctrl+shift+z")]),
    ]
}

/// "ctrl+shift+z" -> modifiers + evdev codes of the physical key.
pub fn parse_chord(s: &str) -> Option<Chord> {
    let mut ch = Chord { codes: Vec::new(), ctrl: false, shift: false, alt: false };
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last()?;
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ch.ctrl = true,
            "shift" => ch.shift = true,
            "alt" => ch.alt = true,
            _ => return None,
        }
    }
    ch.codes = key_codes(&key.to_ascii_lowercase())?;
    Some(ch)
}

fn key_codes(name: &str) -> Option<Vec<u32>> {
    const LETTERS: &str = "qwertyuiop";
    const ROW2: &str = "asdfghjkl";
    const ROW3: &str = "zxcvbnm";
    if name.len() == 1 {
        let c = name.chars().next()?;
        if let Some(i) = LETTERS.find(c) {
            return Some(vec![16 + i as u32]);
        }
        if let Some(i) = ROW2.find(c) {
            return Some(vec![30 + i as u32]);
        }
        if let Some(i) = ROW3.find(c) {
            return Some(vec![44 + i as u32]);
        }
        if let Some(d) = c.to_digit(10) {
            return Some(vec![if d == 0 { 11 } else { 1 + d }]);
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            1..=10 => Some(vec![58 + n]),
            11 => Some(vec![87]),
            12 => Some(vec![88]),
            _ => None,
        };
    }
    Some(match name {
        "return" | "enter" => vec![28, 96],
        "escape" | "esc" => vec![1],
        "space" => vec![57],
        "backspace" => vec![14],
        "tab" => vec![15],
        "delete" => vec![111],
        "up" => vec![103],
        "down" => vec![108],
        "left" => vec![105],
        "right" => vec![106],
        "minus" => vec![12],
        "equal" => vec![13],
        "comma" => vec![51],
        "period" => vec![52],
        "slash" => vec![53],
        _ => return None,
    })
}

/// Old layout: top-level `save_dir`, `notify`, `dim`; color names in `[draw]`.
fn migrate_legacy(t: &mut toml::Table) {
    let mut moved = Vec::new();
    for (old, section, new) in [("save_dir", "output", "save_dir"), ("notify", "output", "notify"), ("dim", "ui", "dim")] {
        if let Some(v) = t.remove(old) {
            moved.push((section, new, v));
        }
    }
    for (section, key, v) in moved {
        if let toml::Value::Table(s) = t.entry(section).or_insert_with(|| toml::Value::Table(Default::default())) {
            s.entry(key).or_insert(v);
        }
    }
    if let Some(toml::Value::Table(d)) = t.get_mut("draw") {
        if let Some(toml::Value::String(name)) = d.get("color") {
            if let Some(i) = ["red", "orange", "yellow", "green", "blue", "white", "black"].iter().position(|n| n == name) {
                d.insert("color".into(), toml::Value::Integer(i as i64));
            }
        }
    }
}

fn section<'a>(t: &'a toml::Table, name: &str, w: &mut Vec<String>) -> Option<&'a toml::Table> {
    match t.get(name)? {
        toml::Value::Table(s) => Some(s),
        _ => {
            w.push(format!("{name}: expected a [{name}] table"));
            None
        }
    }
}

fn check_keys(t: &toml::Table, known: &[&str], prefix: &str, w: &mut Vec<String>) {
    for k in t.keys() {
        if !known.contains(&k.as_str()) {
            w.push(format!("unknown key {prefix}{k}"));
        }
    }
}

/// Read `key` into `dst`. Returns whether a valid value was present.
fn take<T: DeserializeOwned>(t: &toml::Table, key: &str, prefix: &str, dst: &mut T, w: &mut Vec<String>) -> bool {
    let Some(v) = t.get(key) else { return false };
    match v.clone().try_into() {
        Ok(x) => {
            *dst = x;
            true
        }
        Err(e) => {
            w.push(format!("{prefix}{key}: {}", e.message()));
            false
        }
    }
}

fn take_opt(t: &toml::Table, key: &str, prefix: &str, dst: &mut Option<String>, w: &mut Vec<String>) {
    let mut v = String::new();
    if take(t, key, prefix, &mut v, w) {
        *dst = Some(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_is_clean_and_equal_to_builtin() {
        let (cfg, w) = Config::parse(DEFAULT_TOML);
        assert!(w.is_empty(), "{w:?}");
        let b = Config::builtin();
        assert_eq!(format!("{cfg:?}"), format!("{b:?}"));
    }

    #[test]
    fn unknown_and_bad_keys_warn_but_keep_defaults() {
        let (cfg, w) = Config::parse("[ui]\nborder = \"x\"\nbogus = 1\n[nope]\n");
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(cfg.ui.border, 1);
    }

    #[test]
    fn legacy_keys() {
        let (cfg, w) = Config::parse("save_dir = \"/tmp/x\"\nnotify = false\ndim = 0.3\n[draw]\ntool = \"rect\"\ncolor = \"blue\"\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(cfg.output.save_dir, "/tmp/x");
        assert!(!cfg.output.notify);
        assert!((cfg.ui.dim - 0.3).abs() < 1e-6);
        assert_eq!(cfg.draw.tool, Tool::Rect);
        assert_eq!(cfg.draw.color, 4);
    }

    #[test]
    fn chords() {
        let c = parse_chord("ctrl+shift+z").unwrap();
        assert!(c.matches(44, true, true, false));
        assert!(!c.matches(44, true, false, false));
        assert_eq!(parse_chord("Return").unwrap().codes, vec![28, 96]);
        assert!(parse_chord("hyper+a").is_none());
    }

    #[test]
    fn default_keys_lookup() {
        let c = Config::default();
        assert_eq!(c.keys.lookup(30, false, false, false), Some(Cmd::Tool(Tool::Arrow)));
        assert_eq!(c.keys.lookup(44, true, true, false), Some(Cmd::Redo));
        assert_eq!(c.keys.lookup(44, true, false, false), Some(Cmd::Undo));
        assert_eq!(c.keys.lookup(1, false, false, false), Some(Cmd::Cancel));
    }

    #[test]
    fn rebinding() {
        let (cfg, w) = Config::parse("[keys]\nundo = \"u\"\n");
        assert!(w.is_empty());
        assert_eq!(cfg.keys.lookup(22, false, false, false), Some(Cmd::Undo));
        assert_eq!(cfg.keys.lookup(44, true, false, false), None);
    }

    #[test]
    fn zoom_min_never_below_one() {
        let (cfg, _) = Config::parse("[zoom]\nmin = 0.2\nfilter = \"nearest\"\n");
        assert_eq!(cfg.zoom.min, 1.0);
        assert!(cfg.zoom.nearest);
    }
}
