//! The flat UI: pixel canvas, toolbar, labels. Everything here is integer
//! rectangles and bitmaps, no anti-aliasing, no blur, no rounded corners.
//! Layout is in logical pixels; drawing multiplies by the output scale `k`.

use crate::{
    annot::{TOOLS, Tool, WIDTHS},
    geom::{Pt, Rect},
    pixfont,
    theme::{Rgb, Theme},
};

/// Pixel buffer in wl_shm Argb8888 (bytes B, G, R, A).
pub struct Canvas<'a> {
    pub data: &'a mut [u8],
    pub w: i32,
    pub h: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(data: &'a mut [u8], w: i32, h: i32) -> Self {
        debug_assert!(data.len() >= (w * h * 4) as usize);
        Self { data, w, h }
    }

    /// Overwrite a rectangle (clipped) with `argb`.
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, argb: u32) {
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let px = argb.to_le_bytes();
        for yy in y0..y1 {
            let row = &mut self.data[((yy * self.w + x0) * 4) as usize..((yy * self.w + x1) * 4) as usize];
            for p in row.chunks_exact_mut(4) {
                p.copy_from_slice(&px);
            }
        }
    }

    pub fn clear(&mut self, argb: u32) {
        self.fill(0, 0, self.w, self.h, argb);
    }

    /// Rectangle outline `t` px thick, drawn inside the rectangle.
    pub fn outline(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, argb: u32) {
        self.fill(x, y, w, t, argb);
        self.fill(x, y + h - t, w, t, argb);
        self.fill(x, y + t, t, h - 2 * t, argb);
        self.fill(x + w - t, y + t, t, h - 2 * t, argb);
    }

    /// Draw `s` with the built-in font, top-left at `x`,`y`, each font pixel
    /// `scale` physical pixels.
    pub fn text(&mut self, x: i32, y: i32, s: &str, scale: i32, argb: u32) {
        let mut cx = x;
        for c in s.chars() {
            let g = pixfont::glyph(c);
            for (row, bits) in g.iter().enumerate() {
                let mut run = 0;
                for col in 0..=8 {
                    let on = col < 8 && bits & (0x80 >> col) != 0;
                    if on {
                        run += 1;
                    } else if run > 0 {
                        self.fill(cx + (col - run) * scale, y + row as i32 * scale, run * scale, scale, argb);
                        run = 0;
                    }
                }
            }
            cx += pixfont::GW * scale;
        }
    }
}

// ── layout ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    Tool(Tool),
    Color(usize),
    Width(usize),
    Undo,
    Redo,
    Cancel,
    Confirm,
}

pub struct Button {
    pub rect: Rect,
    pub action: Action,
}

pub struct Toolbar {
    pub rect: Rect,
    pub buttons: Vec<Button>,
    /// x positions (global) of the dividers
    seps: Vec<f32>,
}

/// What is currently selected, for highlighting.
pub struct State<'a> {
    pub tool: Tool,
    pub color: usize,
    pub width: f32,
    pub palette: &'a [[u8; 3]],
    pub can_undo: bool,
    pub can_redo: bool,
    pub hover: Option<Action>,
}

pub const BAR_H: f32 = 32.0;
const BTN: f32 = 28.0;
const SWATCH_BTN: f32 = 20.0;
const WIDTH_BTN: f32 = 22.0;
const GAP: f32 = 2.0;
const SEP_GAP: f32 = 9.0;
const PAD: f32 = 3.0;

/// Toolbar next to the selection: below it by default, above or inside when
/// there is no room, centered on it and kept inside `area`.
pub fn layout(sel: Rect, area: Rect, palette_len: usize, place: &str) -> Toolbar {
    let mut buttons = Vec::new();
    let mut seps = Vec::new();
    let y = (BAR_H - BTN) / 2.0;
    let mut x = PAD;
    let mut add = |x: &mut f32, w: f32, action| {
        buttons.push(Button { rect: Rect::new(*x, y, w, BTN), action });
        *x += w + GAP;
    };
    let group_end = |x: &mut f32, seps: &mut Vec<f32>| {
        *x -= GAP;
        seps.push(*x + (SEP_GAP - 1.0) / 2.0);
        *x += SEP_GAP;
    };
    for t in TOOLS {
        add(&mut x, BTN, Action::Tool(t));
    }
    group_end(&mut x, &mut seps);
    for i in 0..palette_len {
        add(&mut x, SWATCH_BTN, Action::Color(i));
    }
    group_end(&mut x, &mut seps);
    for i in 0..WIDTHS.len() {
        add(&mut x, WIDTH_BTN, Action::Width(i));
    }
    group_end(&mut x, &mut seps);
    add(&mut x, BTN, Action::Undo);
    add(&mut x, BTN, Action::Redo);
    group_end(&mut x, &mut seps);
    add(&mut x, BTN, Action::Cancel);
    add(&mut x, BTN, Action::Confirm);
    let w = x - GAP + PAD;

    let gap = 8.0;
    let label = 20.0 + 4.0; // room for the size label above the selection
    let below = sel.bottom() + gap;
    let above = sel.y - gap - BAR_H - label;
    let inside = (sel.bottom() - gap - BAR_H).max(area.y + 8.0);
    let fits_below = below + BAR_H <= area.bottom() - 8.0;
    let fits_above = above >= area.y + 8.0;
    let ty = match place {
        "top" if fits_above => above,
        "bottom" if fits_below => below,
        _ if fits_below => below,
        _ if fits_above => above,
        _ => inside,
    };
    let x0 = (sel.x + sel.w / 2.0 - w / 2.0).min(area.right() - w - 8.0).max(area.x + 8.0);
    for b in &mut buttons {
        b.rect = b.rect.translate(x0, ty);
    }
    for s in &mut seps {
        *s += x0;
    }
    Toolbar { rect: Rect::new(x0, ty, w, BAR_H), buttons, seps }
}

impl Toolbar {
    pub fn hit(&self, p: Pt) -> Option<Action> {
        self.buttons.iter().find(|b| b.rect.contains(p)).map(|b| b.action)
    }
}

// ── drawing ──────────────────────────────────────────────────────────────

fn phys(v: f32, k: f32) -> i32 {
    (v * k).round() as i32
}

/// Filled panel with an `outline` border of `b` logical px.
pub fn frame(c: &mut Canvas, th: &Theme, b: f32, k: f32) {
    c.clear(th.surface.argb(255));
    let t = phys(b, k).max(1);
    c.outline(0, 0, c.w, c.h, t, th.outline.argb(255));
}

/// Draw the toolbar into a canvas that covers exactly `tb.rect`.
pub fn draw_toolbar(c: &mut Canvas, tb: &Toolbar, st: &State, th: &Theme, border: f32, k: f32) {
    frame(c, th, border, k);
    let s = (k.round() as i32).max(1);
    let (ox, oy) = (tb.rect.x, tb.rect.y);
    let local = |r: Rect| (phys(r.x - ox, k), phys(r.y - oy, k), phys(r.w, k), phys(r.h, k));

    for &sx in &tb.seps {
        let (x, y) = (phys(sx - ox, k), phys(7.0, k));
        c.fill(x, y, s, phys(BAR_H - 14.0, k), th.outline.argb(255));
    }

    for b in &tb.buttons {
        let (x, y, w, h) = local(b.rect);
        let hovered = st.hover == Some(b.action);
        let active = match b.action {
            Action::Tool(t) => t == st.tool,
            Action::Width(i) => (WIDTHS[i] - st.width).abs() < 0.01,
            _ => false,
        };
        let disabled =
            (b.action == Action::Undo && !st.can_undo) || (b.action == Action::Redo && !st.can_redo);
        let (cx, cy) = (x + w / 2, y + h / 2);
        match b.action {
            Action::Color(i) => {
                if hovered {
                    c.fill(x, y, w, h, th.hover.argb(255));
                }
                let col = st.palette.get(i).copied().unwrap_or([255, 0, 255]);
                let sw = phys(14.0, k);
                let (sx, sy) = (cx - sw / 2, cy - sw / 2);
                if i == st.color {
                    let ring = phys(2.0, k);
                    c.fill(sx - ring, sy - ring, sw + 2 * ring, sw + 2 * ring, th.text.argb(255));
                    c.fill(sx - ring + s, sy - ring + s, sw + 2 * (ring - s), sw + 2 * (ring - s), th.surface.argb(255));
                }
                c.fill(sx, sy, sw, sw, Rgb(col[0], col[1], col[2]).argb(255));
                c.outline(sx, sy, sw, sw, s, th.outline.argb(255));
            }
            Action::Width(i) => {
                let (bg, fg) = if active {
                    (Some(th.primary), th.on_primary)
                } else if hovered {
                    (Some(th.hover), th.text)
                } else {
                    (None, th.text_dim)
                };
                if let Some(bg) = bg {
                    c.fill(x, y, w, h, bg.argb(255));
                }
                let d = phys(WIDTHS[i], k).max(s);
                c.fill(cx - d / 2, cy - d / 2, d, d, fg.argb(255));
            }
            action => {
                let (bg, fg) = if active {
                    (Some(th.primary), th.on_primary)
                } else if hovered && !disabled {
                    (Some(th.hover), if action == Action::Cancel { th.error } else { th.text })
                } else {
                    (None, if action == Action::Confirm { th.primary } else { th.text_dim })
                };
                if let Some(bg) = bg {
                    c.fill(x, y, w, h, bg.argb(255));
                }
                let col = if disabled { Rgb::mix(fg, th.surface, 0.6) } else { fg };
                let bits = icon_bits(action);
                let ix = cx - (ICON as i32 * s) / 2;
                let iy = cy - (ICON as i32 * s) / 2;
                for (r, row) in bits.iter().enumerate() {
                    let mut run = 0;
                    for cidx in 0..=ICON {
                        let on = cidx < ICON && row & (1 << (ICON - 1 - cidx)) != 0;
                        if on {
                            run += 1;
                        } else if run > 0 {
                            c.fill(ix + (cidx as i32 - run) * s, iy + r as i32 * s, run * s, s, col.argb(255));
                            run = 0;
                        }
                    }
                }
            }
        }
    }
}

/// Small label box: size of the selection, or the start hint.
pub fn label_size(text: &str) -> (f32, f32) {
    (pixfont::width(text) as f32 + 10.0, pixfont::GH as f32 + 4.0)
}

pub fn draw_label(c: &mut Canvas, text: &str, th: &Theme, border: f32, k: f32) {
    frame(c, th, border, k);
    let s = (k.round() as i32).max(1);
    c.text(phys(5.0, k), phys(2.0, k), text, s, th.text.argb(255));
}

// ── icons ────────────────────────────────────────────────────────────────

pub const ICON: usize = 12;
type Bits = [u16; ICON];

struct Grid(Bits);

impl Grid {
    fn set(&mut self, x: i32, y: i32) {
        if (0..ICON as i32).contains(&x) && (0..ICON as i32).contains(&y) {
            self.0[y as usize] |= 1 << (ICON as i32 - 1 - x);
        }
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            self.set(x, y);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    fn thick_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.line(x0, y0, x1, y1);
        self.line(x0 + 1, y0, x1 + 1, y1);
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.line(x0, y0, x1, y0);
        self.line(x0, y1, x1, y1);
        self.line(x0, y0, x0, y1);
        self.line(x1, y0, x1, y1);
    }

    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, dither: bool) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                if !dither || (x + y) % 2 == 0 {
                    self.set(x, y);
                }
            }
        }
    }

    /// Ellipse outline: pixel centers between the inner and outer ellipse.
    fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32) {
        for y in 0..ICON as i32 {
            for x in 0..ICON as i32 {
                let inside = |rx: f32, ry: f32| {
                    let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
                    dx * dx + dy * dy <= 1.0
                };
                if inside(rx, ry) && !inside(rx - 1.0, ry - 1.0) {
                    self.set(x, y);
                }
            }
        }
    }

    fn mirrored(&self) -> Bits {
        let mut out = [0u16; ICON];
        for (y, row) in self.0.iter().enumerate() {
            for x in 0..ICON {
                if row & (1 << x) != 0 {
                    out[y] |= 1 << (ICON - 1 - x);
                }
            }
        }
        out
    }
}

pub fn icon_bits(a: Action) -> Bits {
    let mut g = Grid([0; ICON]);
    match a {
        Action::Tool(Tool::Arrow) => {
            g.line(1, 10, 10, 1);
            g.line(10, 1, 5, 1);
            g.line(10, 1, 10, 6);
            g.line(1, 9, 9, 1);
        }
        Action::Tool(Tool::Line) => {
            g.line(1, 10, 10, 1);
            g.line(1, 9, 9, 1);
        }
        Action::Tool(Tool::Rect) => g.rect(1, 2, 10, 9),
        Action::Tool(Tool::Ellipse) => g.ellipse(6.0, 6.0, 5.5, 4.0),
        Action::Tool(Tool::Pen) => {
            g.thick_line(3, 8, 8, 3);
            g.line(2, 9, 2, 9);
            g.line(1, 10, 2, 10);
            g.line(1, 9, 1, 10);
        }
        Action::Tool(Tool::Highlighter) => {
            g.fill(1, 2, 10, 6, true);
            g.line(1, 10, 10, 10);
        }
        Action::Tool(Tool::Text) => {
            g.line(2, 2, 9, 2);
            g.line(2, 3, 9, 3);
            g.line(5, 3, 5, 10);
            g.line(6, 3, 6, 10);
        }
        Action::Tool(Tool::Pixelate) => {
            for j in 0..3 {
                for i in 0..3 {
                    if (i + j) % 2 == 0 {
                        g.fill(i * 4, j * 4, i * 4 + 3, j * 4 + 3, false);
                    }
                }
            }
        }
        Action::Tool(Tool::Step) => {
            g.ellipse(6.0, 6.0, 5.5, 5.5);
            g.line(6, 3, 6, 8);
            g.line(5, 4, 5, 4);
            g.line(4, 5, 4, 5);
        }
        Action::Undo | Action::Redo => {
            g.line(4, 2, 1, 5);
            g.line(1, 5, 4, 8);
            g.line(1, 5, 8, 5);
            g.line(8, 5, 10, 7);
            g.line(10, 7, 10, 8);
            g.line(10, 8, 8, 10);
            g.line(8, 10, 4, 10);
            if a == Action::Redo {
                return g.mirrored();
            }
        }
        Action::Cancel => {
            g.thick_line(2, 2, 8, 8);
            g.thick_line(2, 9, 8, 3);
        }
        Action::Confirm => {
            for dy in 0..2 {
                g.line(1, 6 + dy, 4, 9 + dy);
                g.line(4, 9 + dy, 10, 3 + dy);
            }
        }
        _ => {}
    }
    g.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(c: &Canvas) -> String {
        let mut s = String::new();
        for y in 0..c.h {
            for x in 0..c.w {
                let i = ((y * c.w + x) * 4) as usize;
                s.push(if c.data[i + 3] != 0 && c.data[i] > 100 { '#' } else { '.' });
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn every_icon_is_drawn_and_inside_the_grid() {
        let mut actions = vec![Action::Undo, Action::Redo, Action::Cancel, Action::Confirm];
        actions.extend(TOOLS.map(Action::Tool));
        for a in actions {
            let bits = icon_bits(a);
            assert!(bits.iter().any(|r| *r != 0), "{a:?} is empty");
            assert!(bits.iter().all(|r| *r < 1 << ICON), "{a:?} leaves the grid");
        }
    }

    #[test]
    fn redo_mirrors_undo() {
        let mut u = icon_bits(Action::Undo);
        let r = icon_bits(Action::Redo);
        let mut g = Grid(u);
        u = g.mirrored();
        g.0 = u;
        assert_eq!(u, r);
    }

    #[test]
    fn toolbar_fits_and_hits() {
        let area = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let sel = Rect::new(100.0, 100.0, 400.0, 300.0);
        let tb = layout(sel, area, 7, "auto");
        assert!(tb.rect.x >= 8.0 && tb.rect.right() <= area.right() - 8.0);
        assert_eq!(tb.rect.y, sel.bottom() + 8.0);
        for b in &tb.buttons {
            assert!(b.rect.x >= tb.rect.x && b.rect.right() <= tb.rect.right(), "{:?}", b.action);
            let c = Pt::new(b.rect.x + b.rect.w / 2.0, b.rect.y + b.rect.h / 2.0);
            assert_eq!(tb.hit(c), Some(b.action));
        }
        // no room below: goes above
        let low = layout(Rect::new(100.0, 900.0, 400.0, 170.0), area, 7, "auto");
        assert!(low.rect.bottom() < 900.0);
        // forced top
        assert!(layout(sel, area, 7, "top").rect.bottom() < sel.y);
    }

    /// `OPAL_PREVIEW_DIR=/some/dir cargo test preview` writes PNGs of the UI.
    #[test]
    fn preview() {
        let Some(dir) = std::env::var_os("OPAL_PREVIEW_DIR") else { return };
        let th = Theme::default();
        let pal = crate::config::DEFAULT_PALETTE;
        for (k, name) in [(1.0f32, "ui-1x"), (2.0, "ui-2x")] {
            let area = Rect::new(0.0, 0.0, 1920.0, 1080.0);
            let tb = layout(Rect::new(300.0, 100.0, 500.0, 200.0), area, pal.len(), "auto");
            let st = State {
                tool: Tool::Rect,
                color: 1,
                width: 4.0,
                palette: &pal,
                can_undo: true,
                can_redo: false,
                hover: Some(Action::Tool(Tool::Pen)),
            };
            let (w, h) = (phys(tb.rect.w, k), phys(tb.rect.h, k));
            let mut buf = vec![0u8; (w * h * 4) as usize];
            draw_toolbar(&mut Canvas::new(&mut buf, w, h), &tb, &st, &th, 1.0, k);
            let (lw, lh) = label_size("2560 x 1440");
            let (lpw, lph) = (phys(lw, k), phys(lh, k));
            let mut lbuf = vec![0u8; (lpw * lph * 4) as usize];
            draw_label(&mut Canvas::new(&mut lbuf, lpw, lph), "2560 x 1440", &th, 1.0, k);
            // compose on a mid-gray backdrop
            let (cw, ch) = (w + 20, h + lph + 30);
            let mut out = vec![0x55u8; (cw * ch * 4) as usize];
            let blit = |out: &mut Vec<u8>, src: &[u8], sw: i32, sh: i32, ox: i32, oy: i32| {
                for y in 0..sh {
                    for x in 0..sw {
                        let (i, o) = (((y * sw + x) * 4) as usize, (((y + oy) * cw + x + ox) * 4) as usize);
                        out[o..o + 4].copy_from_slice(&[src[i + 2], src[i + 1], src[i], 255]);
                    }
                }
            };
            blit(&mut out, &lbuf, lpw, lph, 10, 10);
            blit(&mut out, &buf, w, h, 10, lph + 20);
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            let file = std::fs::File::create(path).unwrap();
            let mut enc = png::Encoder::new(file, cw as u32, ch as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&out).unwrap();
        }
        let _ = render;
    }
}
