//! Text for annotations. A TrueType font found through fontconfig and
//! rasterized by fontdue, or the built-in pixel font when none is available.
//! Nothing is loaded until the first piece of text is drawn.

use std::{collections::HashMap, process::Command};

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{Color, PixmapMut};

use crate::pixfont;

pub struct Fonts {
    pattern: String,
    main: Option<Font>,
    loaded: bool,
    /// fallback fonts by code point, `None` when fontconfig had nothing
    fallback: HashMap<char, Option<Font>>,
    glyphs: HashMap<(char, u32, bool), (Metrics, Vec<u8>)>,
}

impl Fonts {
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_owned(),
            main: None,
            loaded: false,
            fallback: HashMap::new(),
            glyphs: HashMap::new(),
        }
    }

    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if self.pattern != "builtin" {
            self.main = fc_font(&self.pattern);
            if self.main.is_none() {
                eprintln!("opal: no font for {:?}, using the built-in one", self.pattern);
            }
        }
    }

    fn font_for(&mut self, c: char) -> Option<&Font> {
        self.load();
        let main_has = self.main.as_ref().is_some_and(|f| f.lookup_glyph_index(c) != 0);
        if main_has {
            return self.main.as_ref();
        }
        if self.main.is_some() && !c.is_whitespace() && !c.is_control() {
            let pattern = format!(":charset={:x}", c as u32);
            let f = self.fallback.entry(c).or_insert_with(|| fc_font(&pattern));
            if f.as_ref().is_some_and(|f| f.lookup_glyph_index(c) != 0) {
                return f.as_ref();
            }
        }
        self.main.as_ref()
    }

    fn raster(&mut self, c: char, size: f32) -> Option<&(Metrics, Vec<u8>)> {
        let key = (c, size.to_bits(), false);
        if !self.glyphs.contains_key(&key) {
            let font = self.font_for(c)?;
            let g = font.rasterize(c, size);
            self.glyphs.insert(key, g);
        }
        self.glyphs.get(&key)
    }

    pub fn line_height(size: f32) -> f32 {
        (size * 1.25).round()
    }

    fn builtin_scale(size: f32) -> i32 {
        ((size / pixfont::GH as f32).round() as i32).max(1)
    }

    pub fn line_width(&mut self, line: &str, size: f32) -> f32 {
        self.load();
        if self.main.is_none() {
            return (pixfont::width(line) * Self::builtin_scale(size)) as f32;
        }
        line.chars()
            .map(|c| self.font_for(c).map_or(0.0, |f| f.metrics(c, size).advance_width))
            .sum()
    }

    /// Draw with the top-left of the text box at `x`, `y`.
    pub fn draw(&mut self, pm: &mut PixmapMut, text: &str, x: f32, y: f32, size: f32, color: Color) {
        self.load();
        let lh = Self::line_height(size);
        for (i, line) in text.split('\n').enumerate() {
            let ly = y + i as f32 * lh;
            if self.main.is_none() {
                draw_pixel_text(pm, line, x.round() as i32, ly.round() as i32, Self::builtin_scale(size), color);
                continue;
            }
            let ascent = self
                .main
                .as_ref()
                .and_then(|f| f.horizontal_line_metrics(size))
                .map_or(size * 0.8, |m| m.ascent);
            let mut pen = x;
            for c in line.chars() {
                let Some((m, cov)) = self.raster(c, size).map(|g| (g.0, g.1.clone())) else { continue };
                if m.width > 0 {
                    let gx = (pen + m.xmin as f32).round() as i32;
                    let gy = (ly + ascent).round() as i32 - m.ymin - m.height as i32;
                    blend_coverage(pm, &cov, m.width as i32, m.height as i32, gx, gy, color);
                }
                pen += m.advance_width;
            }
        }
    }
}

/// Premultiplied source-over of `color` through an 8-bit coverage mask.
fn blend_coverage(pm: &mut PixmapMut, cov: &[u8], w: i32, h: i32, x: i32, y: i32, color: Color) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let c = color.to_color_u8();
    let ca = c.alpha() as u32;
    let data = pm.data_mut();
    for row in 0..h {
        let yy = y + row;
        if yy < 0 || yy >= ph {
            continue;
        }
        for col in 0..w {
            let xx = x + col;
            if xx < 0 || xx >= pw {
                continue;
            }
            let a = cov[(row * w + col) as usize] as u32 * ca / 255;
            if a == 0 {
                continue;
            }
            over(&mut data[((yy * pw + xx) * 4) as usize..][..4], [c.red(), c.green(), c.blue()], a);
        }
    }
}

fn over(px: &mut [u8], rgb: [u8; 3], a: u32) {
    let inv = 255 - a;
    for k in 0..3 {
        px[k] = ((rgb[k] as u32 * a + px[k] as u32 * inv + 127) / 255) as u8;
    }
    px[3] = (a + (px[3] as u32 * inv + 127) / 255).min(255) as u8;
}

pub fn draw_pixel_text(pm: &mut PixmapMut, line: &str, x: i32, y: i32, scale: i32, color: Color) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let c = color.to_color_u8();
    let data = pm.data_mut();
    let mut cx = x;
    for ch in line.chars() {
        for (row, bits) in pixfont::glyph(ch).iter().enumerate() {
            for col in 0..8 {
                if bits & (0x80 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (xx, yy) = (cx + col * scale + dx, y + row as i32 * scale + dy);
                        if xx >= 0 && xx < pw && yy >= 0 && yy < ph {
                            over(&mut data[((yy * pw + xx) * 4) as usize..][..4], [c.red(), c.green(), c.blue()], c.alpha() as u32);
                        }
                    }
                }
            }
        }
        cx += pixfont::GW * scale;
    }
}

fn fc_font(pattern: &str) -> Option<Font> {
    let out = Command::new("fc-match").args(["-f", "%{file}\n%{index}", pattern]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut it = text.lines();
    let file = it.next()?;
    let index: u32 = it.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);
    let bytes = std::fs::read(file).ok()?;
    Font::from_bytes(bytes, FontSettings { collection_index: index, ..Default::default() }).ok()
}
