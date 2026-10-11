//! Colors for the glass UI. They come from the same matugen files the
//! Quickshell taskbar reads, so Opal follows wallpaper and light/dark mode.

use std::{collections::HashMap, fs, path::PathBuf};

use tiny_skia::Color;

#[derive(Clone, Debug)]
pub struct Theme {
    pub glass: Color,
    pub rim: Color,
    pub shine: Color,
    pub shine_low: Color,
    pub hover: Color,
    pub hover_strong: Color,
    pub shadow: Color,
    pub text: Color,
    pub text_dim: Color,
    pub primary: Color,
    pub error: Color,
    pub dark: bool,
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, (a * 255.0).round() as u8)
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            glass: rgba(18, 18, 24, 0.55),
            rim: rgba(255, 255, 255, 0.14),
            shine: rgba(255, 255, 255, 0.22),
            shine_low: rgba(255, 255, 255, 0.04),
            hover: rgba(255, 255, 255, 0.08),
            hover_strong: rgba(255, 255, 255, 0.13),
            shadow: rgba(0, 0, 0, 0.35),
            text: Color::from_rgba8(0xe8, 0xe1, 0xdd, 255),
            text_dim: Color::from_rgba8(0xcc, 0xc5, 0xc2, 255),
            primary: Color::from_rgba8(0xda, 0xc2, 0xac, 255),
            error: Color::from_rgba8(0xff, 0xb4, 0xab, 255),
            dark: true,
        }
    }
}

impl Theme {
    pub fn load() -> Self {
        let mut t = Self::default();
        let dir = config_dir().join("quickshell/taskbar");
        let mut vars = HashMap::new();
        for file in ["colors.css", "glass.css"] {
            if let Ok(text) = fs::read_to_string(dir.join(file)) {
                parse_css(&text, &mut vars);
            }
        }
        let get = |name: &str, slot: &mut Color| {
            if let Some(c) = vars.get(name) {
                *slot = *c;
            }
        };
        get("glass", &mut t.glass);
        get("glass_rim", &mut t.rim);
        get("glass_shine", &mut t.shine);
        get("glass_shine_low", &mut t.shine_low);
        get("glass_hover", &mut t.hover);
        get("glass_hover_strong", &mut t.hover_strong);
        get("glass_shadow", &mut t.shadow);
        get("on_surface", &mut t.text);
        get("on_surface_variant", &mut t.text_dim);
        get("primary", &mut t.primary);
        get("error", &mut t.error);
        // dark glass has a dark tint, light glass a bright one
        let g = t.glass;
        t.dark = (g.red() + g.green() + g.blue()) / 3.0 < 0.5;
        t
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `@define-color name value;` lines, value is `#rrggbb` or `rgba(r, g, b, a)`.
fn parse_css(text: &str, out: &mut HashMap<String, Color>) {
    for stmt in text.split(';') {
        let Some(pos) = stmt.find("@define-color") else { continue };
        let mut parts = stmt[pos + "@define-color".len()..].trim().splitn(2, char::is_whitespace);
        let (Some(name), Some(value)) = (parts.next(), parts.next()) else { continue };
        if let Some(c) = parse_color(value.trim()) {
            out.insert(name.to_owned(), c);
        }
    }
}

fn parse_color(v: &str) -> Option<Color> {
    if let Some(hex) = v.strip_prefix('#') {
        let n = u32::from_str_radix(hex, 16).ok()?;
        return match hex.len() {
            6 => Some(Color::from_rgba8((n >> 16) as u8, (n >> 8) as u8, n as u8, 255)),
            8 => Some(Color::from_rgba8((n >> 16) as u8, (n >> 8) as u8, n as u8, (n >> 24) as u8)),
            _ => None,
        };
    }
    let inner = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb("))?.strip_suffix(')')?;
    let nums: Vec<f32> = inner.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    match nums[..] {
        [r, g, b] => Some(rgba(r as u8, g as u8, b as u8, 1.0)),
        [r, g, b, a] => Some(rgba(r as u8, g as u8, b as u8, a)),
        _ => None,
    }
}
