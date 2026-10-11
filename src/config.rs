//! `~/.config/opal/config.toml`, every key optional.

use std::path::PathBuf;

use serde::Deserialize;

use crate::{annot::Tool, theme::config_dir};

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Where PNGs go. Default `~/Screenshots`.
    pub save_dir: Option<String>,
    /// Desktop notification after copying. Default true.
    pub notify: Option<bool>,
    /// How much the screen is dimmed outside the selection, 0..1. Default 0.42.
    pub dim: Option<f32>,
    pub draw: Draw,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Draw {
    /// arrow, line, rect, ellipse, pen, highlighter, text, pixelate, step
    pub tool: Option<String>,
    /// red, orange, yellow, green, blue, white, black
    pub color: Option<String>,
    /// Stroke width in logical pixels.
    pub width: Option<f32>,
}

impl Config {
    pub fn load() -> Self {
        let path = config_dir().join("opal/config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("opal: ignoring {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save_dir(&self) -> PathBuf {
        match self.save_dir.as_deref() {
            Some(d) => match d.strip_prefix("~/") {
                Some(rest) => home().join(rest),
                None => PathBuf::from(d),
            },
            None => home().join("Screenshots"),
        }
    }

    pub fn tool(&self) -> Tool {
        match self.draw.tool.as_deref() {
            Some("line") => Tool::Line,
            Some("rect") => Tool::Rect,
            Some("ellipse") => Tool::Ellipse,
            Some("pen") => Tool::Pen,
            Some("highlighter") => Tool::Highlighter,
            Some("text") => Tool::Text,
            Some("pixelate") => Tool::Pixelate,
            Some("step") => Tool::Step,
            _ => Tool::Arrow,
        }
    }

    pub fn color_index(&self) -> usize {
        ["red", "orange", "yellow", "green", "blue", "white", "black"]
            .iter()
            .position(|n| Some(*n) == self.draw.color.as_deref())
            .unwrap_or(0)
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}
