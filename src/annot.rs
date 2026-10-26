//! Annotations: the shapes you draw on the screenshot, with undo/redo and
//! rendering. Coordinates are logical (compositor space); the render target
//! gets a transform, so the live overlay and the exported PNG share one path.

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Stroke, Transform,
};

use crate::{
    capture::Shot,
    geom::{Pt, Rect},
    pixfont,
    text::{self, Fonts},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tool {
    Pen,
    Highlighter,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Text,
    Pixelate,
    Step,
}

pub const TOOLS: [Tool; 9] = [
    Tool::Arrow,
    Tool::Line,
    Tool::Rect,
    Tool::Ellipse,
    Tool::Pen,
    Tool::Highlighter,
    Tool::Text,
    Tool::Pixelate,
    Tool::Step,
];

impl Tool {
    pub fn from_name(s: &str) -> Option<Tool> {
        Some(match s {
            "arrow" => Tool::Arrow,
            "line" => Tool::Line,
            "rect" => Tool::Rect,
            "ellipse" => Tool::Ellipse,
            "pen" => Tool::Pen,
            "highlighter" => Tool::Highlighter,
            "text" => Tool::Text,
            "pixelate" => Tool::Pixelate,
            "step" => Tool::Step,
            _ => return None,
        })
    }
}

pub const WIDTHS: [f32; 3] = [2.0, 4.0, 8.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub color: [u8; 3],
    /// Stroke width in logical px (also scales text and markers).
    pub width: f32,
}

impl Style {
    fn color(&self, alpha: f32) -> Color {
        Color::from_rgba8(self.color[0], self.color[1], self.color[2], (alpha * 255.0) as u8)
    }

    fn luma(&self) -> u32 {
        self.color[0] as u32 + self.color[1] as u32 + self.color[2] as u32
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    Pen(Vec<Pt>),
    Highlighter(Vec<Pt>),
    Line(Pt, Pt),
    Arrow(Pt, Pt),
    Rect(Pt, Pt),
    Ellipse(Pt, Pt),
    Text { at: Pt, text: String },
    Pixelate(Pt, Pt),
    Step { at: Pt, n: u32 },
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub style: Style,
}

/// Committed shapes plus the redo stack.
#[derive(Default)]
pub struct History {
    pub items: Vec<Item>,
    redo: Vec<Item>,
}

impl History {
    pub fn push(&mut self, item: Item) {
        self.items.push(item);
        self.redo.clear();
    }

    pub fn undo(&mut self) -> Option<Rect> {
        let i = self.items.pop()?;
        let r = i.rough_bounds();
        self.redo.push(i);
        Some(r)
    }

    pub fn redo(&mut self) -> Option<Rect> {
        let i = self.redo.pop()?;
        let r = i.rough_bounds();
        self.items.push(i);
        Some(r)
    }

    pub fn can_undo(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn next_step(&self) -> u32 {
        self.items.iter().filter(|i| matches!(i.kind, Kind::Step { .. })).count() as u32 + 1
    }
}

pub fn text_size(style: &Style) -> f32 {
    12.0 + style.width * 3.0
}

fn step_radius(style: &Style) -> f32 {
    11.0 + style.width * 1.2
}

impl Item {
    /// Area the item can touch, in logical px. Text uses a generous guess
    /// instead of measuring, so this needs no font.
    pub fn rough_bounds(&self) -> Rect {
        let w = self.style.width;
        match &self.kind {
            Kind::Pen(p) | Kind::Highlighter(p) => {
                let grow = if matches!(self.kind, Kind::Highlighter(_)) { w * 3.5 } else { w } / 2.0 + 2.0;
                let mut it = p.iter();
                let Some(first) = it.next() else { return Rect::default() };
                let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
                for q in it {
                    x0 = x0.min(q.x);
                    y0 = y0.min(q.y);
                    x1 = x1.max(q.x);
                    y1 = y1.max(q.y);
                }
                Rect::new(x0, y0, x1 - x0, y1 - y0).inflate(grow)
            }
            Kind::Line(a, b) | Kind::Rect(a, b) | Kind::Ellipse(a, b) | Kind::Pixelate(a, b) => {
                Rect::from_points(*a, *b).inflate(w + 2.0)
            }
            Kind::Arrow(a, b) => Rect::from_points(*a, *b).inflate(w * 4.5 + 10.0),
            Kind::Text { at, text } => {
                let size = text_size(&self.style);
                let lines = text.split('\n').count() as f32;
                let longest = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0) as f32;
                Rect::new(at.x - 2.0, at.y - 2.0, longest * size * 0.9 + size, lines * size * 1.25 + 6.0)
            }
            Kind::Step { at, .. } => {
                let r = step_radius(&self.style);
                Rect::new(at.x - r - 3.0, at.y - r - 3.0, 2.0 * r + 6.0, 2.0 * r + 6.0)
            }
        }
    }
}

fn paint(c: Color, aa: bool) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = aa;
    p
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

/// Everything the renderers need besides the shape itself.
pub struct Ctx<'a> {
    /// Logical -> target pixels.
    pub tf: Transform,
    /// Target pixels per logical pixel.
    pub k: f32,
    /// Source for the pixelate tool.
    pub shots: &'a [Shot],
    /// Anti-alias the shapes.
    pub aa: bool,
    /// Pixelate block size, logical px.
    pub block: f32,
}

impl Ctx<'_> {
    fn map(&self, p: Pt) -> Pt {
        Pt::new(p.x * self.tf.sx + self.tf.tx, p.y * self.tf.sy + self.tf.ty)
    }
}

pub fn draw_all(pm: &mut PixmapMut, ctx: &Ctx, items: &[Item], fonts: &mut Fonts) {
    for it in items {
        draw(pm, ctx, it, fonts);
    }
}

pub fn draw(pm: &mut PixmapMut, ctx: &Ctx, it: &Item, fonts: &mut Fonts) {
    let st = &it.style;
    let aa = ctx.aa;
    let solid = paint(st.color(1.0), aa);
    match &it.kind {
        Kind::Pen(pts) => draw_pen(pm, ctx, pts, &solid, st.width),
        Kind::Highlighter(pts) => {
            let p = paint(st.color(0.38), aa);
            draw_pen(pm, ctx, pts, &p, st.width * 3.5);
        }
        Kind::Line(a, b) => {
            let mut pb = PathBuilder::new();
            pb.move_to(a.x, a.y);
            pb.line_to(b.x, b.y);
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &solid, &stroke(st.width), ctx.tf, None);
            }
        }
        Kind::Arrow(a, b) => draw_arrow(pm, ctx, *a, *b, &solid, st.width),
        Kind::Rect(a, b) => {
            let r = Rect::from_points(*a, *b);
            if let Some(rect) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)) {
                let path = PathBuilder::from_rect(rect);
                let s = Stroke { line_join: LineJoin::Miter, line_cap: LineCap::Butt, width: st.width, ..Default::default() };
                pm.stroke_path(&path, &solid, &s, ctx.tf, None);
            }
        }
        Kind::Ellipse(a, b) => {
            let r = Rect::from_points(*a, *b);
            if let Some(rect) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)) {
                if let Some(path) = PathBuilder::from_oval(rect) {
                    pm.stroke_path(&path, &solid, &stroke(st.width), ctx.tf, None);
                }
            }
        }
        Kind::Text { at, text } => {
            let size = text_size(st) * ctx.k;
            let o = ctx.map(*at);
            // soft dark halo keeps light text readable on bright screenshots
            if st.luma() > 100 {
                let halo = Color::from_rgba8(0, 0, 0, 70);
                fonts.draw(pm, text, o.x + 0.6 * ctx.k, o.y + 1.0 * ctx.k, size, halo);
            }
            fonts.draw(pm, text, o.x, o.y, size, st.color(1.0));
        }
        Kind::Pixelate(a, b) => pixelate(pm, ctx, Rect::from_points(*a, *b)),
        Kind::Step { at, n } => {
            let r = step_radius(st);
            if let Some(c) = PathBuilder::from_circle(at.x, at.y, r) {
                let shadow = paint(Color::from_rgba8(0, 0, 0, 80), aa);
                pm.fill_path(&c, &shadow, FillRule::Winding, ctx.tf.pre_translate(0.0, 1.2), None);
                pm.fill_path(&c, &solid, FillRule::Winding, ctx.tf, None);
                if let Some(rim) = PathBuilder::from_circle(at.x, at.y, r - 0.75) {
                    let p = paint(Color::from_rgba8(255, 255, 255, 140), aa);
                    pm.stroke_path(&rim, &p, &stroke(1.5), ctx.tf, None);
                }
            }
            // digits in the pixel font, at a whole-number scale
            let label = n.to_string();
            let s = ((1.1 * r * ctx.k / pixfont::GH as f32).round() as i32).max(1);
            let (w, h) = (pixfont::width(&label) * s, pixfont::GH * s);
            let o = ctx.map(*at);
            let fg = if st.luma() > 480 { Color::from_rgba8(0x1c, 0x1c, 0x1e, 255) } else { Color::WHITE };
            text::draw_pixel_text(pm, &label, (o.x - w as f32 / 2.0).round() as i32, (o.y - h as f32 / 2.0).round() as i32, s, fg);
        }
    }
}

fn draw_pen(pm: &mut PixmapMut, ctx: &Ctx, pts: &[Pt], p: &Paint, width: f32) {
    match pts {
        [] => {}
        [a] => {
            if let Some(c) = PathBuilder::from_circle(a.x, a.y, width / 2.0) {
                pm.fill_path(&c, p, FillRule::Winding, ctx.tf, None);
            }
        }
        _ => {
            let mut pb = PathBuilder::new();
            pb.move_to(pts[0].x, pts[0].y);
            // quadratic curves through segment midpoints smooth out mouse jitter
            for w in pts.windows(2) {
                let mid = Pt::new((w[0].x + w[1].x) / 2.0, (w[0].y + w[1].y) / 2.0);
                pb.quad_to(w[0].x, w[0].y, mid.x, mid.y);
            }
            let last = pts[pts.len() - 1];
            pb.line_to(last.x, last.y);
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, p, &stroke(width), ctx.tf, None);
            }
        }
    }
}

fn draw_arrow(pm: &mut PixmapMut, ctx: &Ctx, a: Pt, b: Pt, p: &Paint, width: f32) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head = (width * 4.5 + 8.0).min(len * 0.8);
    let half = head * 0.42;
    let base = Pt::new(b.x - ux * head * 0.85, b.y - uy * head * 0.85);
    let mut shaft = PathBuilder::new();
    shaft.move_to(a.x, a.y);
    shaft.line_to(base.x, base.y);
    if let Some(path) = shaft.finish() {
        pm.stroke_path(&path, p, &stroke(width), ctx.tf, None);
    }
    let mut hd = PathBuilder::new();
    hd.move_to(b.x, b.y);
    hd.line_to(b.x - ux * head + -uy * half, b.y - uy * head + ux * half);
    hd.line_to(b.x - ux * head - -uy * half, b.y - uy * head - ux * half);
    hd.close();
    if let Some(path) = hd.finish() {
        pm.fill_path(&path, p, FillRule::Winding, ctx.tf, None);
        pm.stroke_path(&path, p, &stroke(width * 0.35), ctx.tf, None);
    }
}

/// Color of the capture at a logical point (nearest pixel).
fn sample(shots: &[Shot], x: f32, y: f32) -> Option<[u8; 3]> {
    let s = shots.iter().find(|s| {
        x >= s.x as f32 && x < (s.x + s.lw) as f32 && y >= s.y as f32 && y < (s.y + s.lh) as f32
    })?;
    let k = s.scale();
    Some(s.rgb(((x - s.x as f32) * k) as u32, ((y - s.y as f32) * k) as u32))
}

/// Mosaic: blocks in a logical grid anchored at the region's corner, each
/// filled with the average of a few samples of the capture.
fn pixelate(pm: &mut PixmapMut, ctx: &Ctx, r: Rect) {
    let block = ctx.block;
    if r.w < 1.0 || r.h < 1.0 {
        return;
    }
    let mut y = r.y;
    while y < r.bottom() {
        let bh = block.min(r.bottom() - y);
        let mut x = r.x;
        while x < r.right() {
            let bw = block.min(r.right() - x);
            let (mut sr, mut sg, mut sb, mut n) = (0u32, 0u32, 0u32, 0u32);
            for j in 0..4 {
                for i in 0..4 {
                    let sx = x + bw * (i as f32 + 0.5) / 4.0;
                    let sy = y + bh * (j as f32 + 0.5) / 4.0;
                    if let Some(c) = sample(ctx.shots, sx, sy) {
                        sr += c[0] as u32;
                        sg += c[1] as u32;
                        sb += c[2] as u32;
                        n += 1;
                    }
                }
            }
            if n > 0 {
                let mut p = Paint::default();
                p.set_color_rgba8((sr / n) as u8, (sg / n) as u8, (sb / n) as u8, 255);
                p.anti_alias = false;
                if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, bw, bh) {
                    pm.fill_rect(rect, &p, ctx.tf, None);
                }
            }
            x += block;
        }
        y += block;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> Style {
        Style { color: [255, 0, 0], width: 4.0 }
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut h = History::default();
        h.push(Item { kind: Kind::Line(Pt::new(0.0, 0.0), Pt::new(10.0, 10.0)), style: style() });
        h.push(Item { kind: Kind::Step { at: Pt::new(5.0, 5.0), n: 1 }, style: style() });
        assert_eq!(h.next_step(), 2);
        assert!(h.undo().is_some());
        assert!(h.can_redo());
        assert!(h.redo().is_some());
        assert!(h.undo().is_some() && h.undo().is_some() && h.undo().is_none());
    }

    #[test]
    fn bounds_cover_the_shape() {
        let it = Item { kind: Kind::Arrow(Pt::new(10.0, 10.0), Pt::new(100.0, 50.0)), style: style() };
        let b = it.rough_bounds();
        assert!(b.x < 10.0 && b.y < 10.0 && b.right() > 100.0 && b.bottom() > 50.0);
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;

    fn ctx(k: f32) -> Ctx<'static> {
        Ctx { tf: Transform::from_scale(k, k), k, shots: &[], aa: true, block: 10.0 }
    }

    fn lit(pm: &tiny_skia::Pixmap) -> usize {
        pm.data().chunks_exact(4).filter(|p| p[3] > 0).count()
    }

    #[test]
    fn text_draws_with_a_system_font_and_with_the_builtin_one() {
        for pattern in ["sans-serif:bold", "builtin"] {
            let mut fonts = Fonts::new(pattern);
            let mut pm = tiny_skia::Pixmap::new(200, 60).unwrap();
            let it = Item {
                kind: Kind::Text { at: Pt::new(10.0, 10.0), text: "Hi Жук".into() },
                style: Style { color: [255, 0, 0], width: 4.0 },
            };
            draw(&mut pm.as_mut(), &ctx(1.0), &it, &mut fonts);
            assert!(lit(&pm) > 100, "{pattern}: {} px", lit(&pm));
        }
    }

    /// `OPAL_PREVIEW_DIR=/dir cargo test preview_annotations` writes /dir/annot.png
    #[test]
    fn preview_annotations() {
        let Some(dir) = std::env::var_os("OPAL_PREVIEW_DIR") else { return };
        let mut pm = tiny_skia::Pixmap::new(640, 260).unwrap();
        pm.fill(Color::from_rgba8(0x30, 0x34, 0x3c, 255));
        let mut fonts = Fonts::new("sans-serif:bold");
        let st = |c: [u8; 3], w: f32| Style { color: c, width: w };
        let p = Pt::new;
        let items = [
            Item { kind: Kind::Arrow(p(20.0, 40.0), p(150.0, 100.0)), style: st([0xff, 0x3b, 0x30], 4.0) },
            Item { kind: Kind::Line(p(180.0, 40.0), p(280.0, 100.0)), style: st([0xff, 0x95, 0x00], 2.0) },
            Item { kind: Kind::Rect(p(310.0, 30.0), p(410.0, 100.0)), style: st([0xff, 0xd6, 0x0a], 4.0) },
            Item { kind: Kind::Ellipse(p(440.0, 30.0), p(540.0, 100.0)), style: st([0x30, 0xd1, 0x58], 4.0) },
            Item { kind: Kind::Pen(vec![p(20.0, 160.0), p(50.0, 130.0), p(80.0, 180.0), p(110.0, 130.0), p(140.0, 170.0)]), style: st([0x0a, 0x84, 0xff], 4.0) },
            Item { kind: Kind::Highlighter(vec![p(180.0, 150.0), p(300.0, 150.0)]), style: st([0xff, 0xd6, 0x0a], 4.0) },
            Item { kind: Kind::Text { at: p(180.0, 170.0), text: "Text Жук 123".into() }, style: st([0xff, 0xff, 0xff], 4.0) },
            Item { kind: Kind::Step { at: p(400.0, 160.0), n: 1 }, style: st([0xff, 0x3b, 0x30], 4.0) },
            Item { kind: Kind::Step { at: p(450.0, 160.0), n: 12 }, style: st([0xff, 0xff, 0x00], 4.0) },
            Item { kind: Kind::Step { at: p(500.0, 160.0), n: 3 }, style: st([0x0a, 0x84, 0xff], 8.0) },
        ];
        draw_all(&mut pm.as_mut(), &ctx(1.0), &items, &mut fonts);
        pm.save_png(std::path::Path::new(&dir).join("annot.png")).unwrap();
    }
}
