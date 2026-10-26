//! Colors for the UI: eight tokens named like matugen's Material colors.
//! Later layers win: built-in, quickshell's `colors.css`, `theme.file`,
//! then the `[theme]` overrides in config.toml.

use std::{collections::HashMap, fs, path::Path};

use crate::config::{ThemeCfg, config_dir, expand};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `0xAARRGGBB`, which is wl_shm Argb8888 on little-endian.
    pub fn argb(self, a: u8) -> u32 {
        let p = |c: u8| (c as u32 * a as u32 + 127) / 255;
        (a as u32) << 24 | p(self.0) << 16 | p(self.1) << 8 | p(self.2)
    }

    pub fn parse(s: &str) -> Option<Rgb> {
        let hex = s.trim().strip_prefix('#')?;
        // #rrggbb, or #rrggbbaa (alpha is ignored: tokens are opaque)
        if hex.len() != 6 && hex.len() != 8 {
            return None;
        }
        let n = u32::from_str_radix(&hex[..6], 16).ok()?;
        Some(Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
    }

    /// `a` blended towards `b` by `t` (0 = a, 1 = b).
    pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
        let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
        Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
    }
}

pub const TOKENS: [&str; 8] = [
    "surface",
    "on_surface",
    "on_surface_variant",
    "outline",
    "surface_container_high",
    "primary",
    "on_primary",
    "error",
];

#[derive(Clone, Debug)]
pub struct Theme {
    /// panel fill
    pub surface: Rgb,
    /// text, active icons
    pub text: Rgb,
    /// inactive icons
    pub text_dim: Rgb,
    /// panel border, dividers
    pub outline: Rgb,
    /// hovered button
    pub hover: Rgb,
    /// selection border, handles, active tool
    pub primary: Rgb,
    /// icon on an active tool
    pub on_primary: Rgb,
    pub error: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        // the mono desktop style
        Self {
            surface: Rgb(0x13, 0x13, 0x13),
            text: Rgb(0xe2, 0xe2, 0xe2),
            text_dim: Rgb(0xc6, 0xc6, 0xc6),
            outline: Rgb(0x91, 0x91, 0x91),
            hover: Rgb(0x2a, 0x2a, 0x2a),
            primary: Rgb(0xff, 0xff, 0xff),
            on_primary: Rgb(0x1b, 0x1b, 0x1b),
            error: Rgb(0xff, 0xb4, 0xab),
        }
    }
}

impl Theme {
    pub fn load(cfg: &ThemeCfg) -> Self {
        let mut vars: HashMap<String, Rgb> = HashMap::new();
        let mut layer = |path: &Path| -> bool {
            let Ok(text) = fs::read_to_string(path) else { return false };
            if path.extension().is_some_and(|e| e == "css") {
                parse_css(&text, &mut vars);
            } else {
                parse_toml(&text, &mut vars);
            }
            true
        };
        layer(&config_dir().join("quickshell/taskbar/colors.css"));
        if let Some(f) = &cfg.file {
            layer(&expand(f));
        }
        for (k, v) in &cfg.tokens {
            if let Some(c) = Rgb::parse(v) {
                vars.insert(k.clone(), c);
            }
        }
        Self::from_vars(&vars)
    }

    pub fn from_vars(vars: &HashMap<String, Rgb>) -> Self {
        let mut t = Self::default();
        let get = |name: &str, slot: &mut Rgb| {
            if let Some(c) = vars.get(name) {
                *slot = *c;
            }
        };
        get("surface", &mut t.surface);
        get("on_surface", &mut t.text);
        get("on_surface_variant", &mut t.text_dim);
        get("outline", &mut t.outline);
        get("surface_container_high", &mut t.hover);
        get("primary", &mut t.primary);
        get("on_primary", &mut t.on_primary);
        get("error", &mut t.error);
        t
    }
}

/// `@define-color name value;` lines. Values other than `#hex` are skipped.
pub fn parse_css(text: &str, out: &mut HashMap<String, Rgb>) {
    for stmt in text.split(';') {
        let Some(pos) = stmt.find("@define-color") else { continue };
        let mut parts = stmt[pos + "@define-color".len()..].trim().splitn(2, char::is_whitespace);
        let (Some(name), Some(value)) = (parts.next(), parts.next()) else { continue };
        if let Some(c) = Rgb::parse(value) {
            out.insert(name.to_owned(), c);
        }
    }
}

/// Flat `name = "#rrggbb"` pairs (what the matugen template writes).
pub fn parse_toml(text: &str, out: &mut HashMap<String, Rgb>) {
    let Ok(t) = text.parse::<toml::Table>() else { return };
    for (k, v) in t {
        if let Some(c) = v.as_str().and_then(Rgb::parse) {
            out.insert(k, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_and_toml_agree() {
        let mut a = HashMap::new();
        parse_css("@define-color primary #aabbcc;\n@define-color on_primary #112233;", &mut a);
        let mut b = HashMap::new();
        parse_toml("primary = \"#aabbcc\"\non_primary = \"#112233\"", &mut b);
        assert_eq!(a, b);
        assert_eq!(Theme::from_vars(&a).primary, Rgb(0xaa, 0xbb, 0xcc));
    }

    #[test]
    fn hex_forms() {
        assert_eq!(Rgb::parse("#ffffff26"), Some(Rgb(255, 255, 255)));
        assert_eq!(Rgb::parse("fff"), None);
        assert_eq!(Rgb::parse("#fff"), None);
    }

    #[test]
    fn premultiplied() {
        assert_eq!(Rgb(255, 255, 255).argb(255), 0xffff_ffff);
        assert_eq!(Rgb(255, 0, 0).argb(128), 0x8080_0000);
    }
}
