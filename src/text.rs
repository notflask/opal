//! Text rendering with cosmic-text onto tiny-skia pixmaps.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use tiny_skia::{Color, PixmapMut};

pub struct Fonts {
    fs: FontSystem,
    cache: SwashCache,
}

impl Fonts {
    /// Placeholder with no fonts loaded, used while the real one is moved out.
    pub fn empty() -> Self {
        Self {
            fs: FontSystem::new_with_fonts(std::iter::empty()),
            cache: SwashCache::new(),
        }
    }

    pub fn new() -> Self {
        Self { fs: FontSystem::new(), cache: SwashCache::new() }
    }

    fn buffer(&mut self, text: &str, size: f32, weight: u16) -> Buffer {
        let mut buf = Buffer::new(&mut self.fs, Metrics::new(size, (size * 1.25).round()));
        buf.set_size(None, None);
        let attrs = Attrs::new().family(Family::Name("Inter")).weight(Weight(weight));
        buf.set_text(text, &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.fs, false);
        buf
    }

    /// Width and height in pixels.
    pub fn measure(&mut self, text: &str, size: f32, weight: u16) -> (f32, f32) {
        let buf = self.buffer(text, size, weight);
        let w = buf.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        (w, (size * 1.25).round())
    }

    /// Draw with the top-left of the text box at `x`, `y`.
    pub fn draw(
        &mut self,
        pm: &mut PixmapMut,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        weight: u16,
        color: Color,
    ) {
        let mut buf = self.buffer(text, size, weight);
        let c8 = color.to_color_u8();
        let base = cosmic_text::Color::rgba(c8.red(), c8.green(), c8.blue(), c8.alpha());
        let (ox, oy) = (x.round() as i32, y.round() as i32);
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        let data = pm.data_mut();
        buf.draw(&mut self.fs, &mut self.cache, base, |gx, gy, gw, gh, c| {
            let a = c.a() as u32;
            if a == 0 {
                return;
            }
            for yy in (oy + gy).max(0)..(oy + gy + gh as i32).min(h) {
                for xx in (ox + gx).max(0)..(ox + gx + gw as i32).min(w) {
                    let i = ((yy * w + xx) * 4) as usize;
                    let inv = 255 - a;
                    let src = [c.r() as u32, c.g() as u32, c.b() as u32, 255];
                    let px = &mut data[i..i + 4];
                    for k in 0..4 {
                        px[k] = ((src[k] * a + px[k] as u32 * inv + 127) / 255).min(255) as u8;
                    }
                }
            }
        });
    }
}
