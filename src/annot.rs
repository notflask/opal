//! Annotations: the shapes you draw on the screenshot, with undo/redo and
//! rendering. Coordinates are logical (compositor space); the render target
//! gets a transform, so the live overlay and the exported PNG share one path.

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Stroke, Transform,
};

use crate::{
    capture::Shot,
    geom::{Pt, Rect},
    text::Fonts,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

pub const COLORS: [[u8; 3]; 7] = [
    [0xff, 0x3b, 0x30], // red
    [0xff, 0x95, 0x00], // orange
    [0xff, 0xd6, 0x0a], // yellow
    [0x30, 0xd1, 0x58], // green
    [0x0a, 0x84, 0xff], // blue
    [0xff, 0xff, 0xff], // white
    [0x1c, 0x1c, 0x1e], // black
];

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

    pub fn undo(&mut self) -> bool {
        match self.items.pop() {
            Some(i) => {
                self.redo.push(i);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(i) => {
                self.items.push(i);
                true
            }
            None => false,
        }
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

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
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
    let solid = paint(st.color(1.0));
    match &it.kind {
        Kind::Pen(pts) => draw_pen(pm, ctx, pts, &solid, st.width),
        Kind::Highlighter(pts) => {
            let p = paint(st.color(0.38));
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
                pm.stroke_path(&path, &solid, &stroke(st.width), ctx.tf, None);
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
            let c = st.color(1.0);
            // soft dark halo keeps light text readable on bright screenshots
            if st.color != [0x1c, 0x1c, 0x1e] {
                let halo = Color::from_rgba8(0, 0, 0, 70);
                fonts.draw(pm, text, o.x + 0.6 * ctx.k, o.y + 1.0 * ctx.k, size, 600, halo);
            }
            fonts.draw(pm, text, o.x, o.y, size, 600, c);
        }
        Kind::Pixelate(a, b) => pixelate(pm, ctx, Rect::from_points(*a, *b)),
        Kind::Step { at, n } => {
            let r = 11.0 + st.width * 1.2;
            if let Some(c) = PathBuilder::from_circle(at.x, at.y, r) {
                let shadow = paint(Color::from_rgba8(0, 0, 0, 80));
                pm.fill_path(&c, &shadow, FillRule::Winding, ctx.tf.pre_translate(0.0, 1.2), None);
                pm.fill_path(&c, &solid, FillRule::Winding, ctx.tf, None);
                if let Some(rim) = PathBuilder::from_circle(at.x, at.y, r - 0.75) {
                    let p = paint(Color::from_rgba8(255, 255, 255, 140));
                    pm.stroke_path(&rim, &p, &stroke(1.5), ctx.tf, None);
                }
            }
            let label = n.to_string();
            let size = r * 1.15 * ctx.k;
            let (w, h) = fonts.measure(&label, size, 700);
            let o = ctx.map(*at);
            let dark = st.color[0] as u32 + st.color[1] as u32 + st.color[2] as u32 > 480;
            let fg = if dark { Color::from_rgba8(0x1c, 0x1c, 0x1e, 255) } else { Color::WHITE };
            fonts.draw(pm, &label, o.x - w / 2.0, o.y - h / 2.0, size, 700, fg);
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
            for w in pts.windows(2).skip(0) {
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
    let px = (((x - s.x as f32) * k) as u32).min(s.pixmap.width() - 1);
    let py = (((y - s.y as f32) * k) as u32).min(s.pixmap.height() - 1);
    let i = ((py * s.pixmap.width() + px) * 4) as usize;
    let d = s.pixmap.data();
    Some([d[i], d[i + 1], d[i + 2]])
}

/// Mosaic: blocks in a logical grid anchored at the region's corner, each
/// filled with the average of a few samples of the capture.
fn pixelate(pm: &mut PixmapMut, ctx: &Ctx, r: Rect) {
    const BLOCK: f32 = 10.0;
    if r.w < 1.0 || r.h < 1.0 {
        return;
    }
    let mut y = r.y;
    while y < r.bottom() {
        let bh = BLOCK.min(r.bottom() - y);
        let mut x = r.x;
        while x < r.right() {
            let bw = BLOCK.min(r.right() - x);
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
            x += BLOCK;
        }
        y += BLOCK;
    }
}
