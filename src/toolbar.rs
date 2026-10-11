//! The glass toolbar under the selection: layout, hit testing, drawing.

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Stroke, Transform,
};

use crate::{
    annot::{COLORS, Tool, WIDTHS, TOOLS},
    geom::{Pt, Rect},
    glass,
    theme::Theme,
};

#[derive(Clone, Copy, PartialEq, Debug)]
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
    /// x positions of the dividers
    seps: Vec<f32>,
}

/// What is currently selected, for highlighting.
pub struct State {
    pub tool: Tool,
    pub color: usize,
    pub width: f32,
    pub can_undo: bool,
    pub can_redo: bool,
    pub hover: Option<Action>,
}

const H: f32 = 48.0;
const PAD: f32 = 8.0;
const BTN: f32 = 32.0;
const SEP: f32 = 13.0;

/// Place the toolbar below the selection (above, or inside, if there is no
/// room), centered on it and kept inside `area`.
pub fn layout(sel: Rect, area: Rect) -> Toolbar {
    let mut buttons = Vec::new();
    let mut seps = Vec::new();
    let mut x = PAD;
    let mut add = |x: &mut f32, w: f32, action| {
        buttons.push(Button { rect: Rect::new(*x, (H - BTN) / 2.0, w, BTN), action });
        *x += w + 2.0;
    };
    for t in TOOLS {
        add(&mut x, BTN, Action::Tool(t));
    }
    seps.push(x + SEP / 2.0 - 1.0);
    x += SEP;
    for i in 0..COLORS.len() {
        add(&mut x, 24.0, Action::Color(i));
    }
    seps.push(x + SEP / 2.0 - 1.0);
    x += SEP;
    for i in 0..WIDTHS.len() {
        add(&mut x, 28.0, Action::Width(i));
    }
    seps.push(x + SEP / 2.0 - 1.0);
    x += SEP;
    add(&mut x, BTN, Action::Undo);
    add(&mut x, BTN, Action::Redo);
    seps.push(x + SEP / 2.0 - 1.0);
    x += SEP;
    add(&mut x, BTN, Action::Cancel);
    add(&mut x, BTN, Action::Confirm);
    let w = x - 2.0 + PAD;

    let gap = 12.0;
    let below = sel.bottom() + gap;
    let y = if below + H <= area.bottom() - 8.0 {
        below
    } else if sel.y - gap - H - 34.0 >= area.y + 8.0 {
        sel.y - gap - H - 34.0 // leaves room for the size label
    } else {
        (sel.bottom() - gap - H).max(area.y + 8.0)
    };
    let x0 = (sel.x + sel.w / 2.0 - w / 2.0)
        .min(area.right() - w - 8.0)
        .max(area.x + 8.0);
    for b in &mut buttons {
        b.rect = b.rect.translate(x0, y);
    }
    for s in &mut seps {
        *s += x0;
    }
    Toolbar { rect: Rect::new(x0, y, w, H), buttons, seps }
}

impl Toolbar {
    pub fn hit(&self, p: Pt) -> Option<Action> {
        self.buttons.iter().find(|b| b.rect.contains(p)).map(|b| b.action)
    }
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

fn alpha(c: Color, a: f32) -> Color {
    Color::from_rgba(c.red(), c.green(), c.blue(), (c.alpha() * a).clamp(0.0, 1.0)).unwrap()
}

/// Draw the toolbar. `to_px` maps logical rects to the target buffer.
pub fn draw(
    pm: &mut PixmapMut,
    tb: &Toolbar,
    st: &State,
    th: &Theme,
    k: f32,
    to_px: &dyn Fn(Rect) -> Rect,
) {
    let panel = to_px(tb.rect);
    glass::panel(pm, panel, 20.0 * k, k, th);

    for &sx in &tb.seps {
        let r = to_px(Rect::new(sx, tb.rect.y + 13.0, 1.0, H - 26.0));
        glass::pill(pm, r, 0.5 * k, alpha(th.rim, 1.4));
    }

    for b in &tb.buttons {
        let r = to_px(b.rect);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let hovered = st.hover == Some(b.action);
        let active = match b.action {
            Action::Tool(t) => t == st.tool,
            Action::Width(i) => (WIDTHS[i] - st.width).abs() < 0.01,
            _ => false,
        };
        let disabled = (b.action == Action::Undo && !st.can_undo)
            || (b.action == Action::Redo && !st.can_redo);

        match b.action {
            Action::Color(i) => {
                let c = COLORS[i];
                let col = Color::from_rgba8(c[0], c[1], c[2], 255);
                if i == st.color {
                    ring(pm, cx, cy, 10.5 * k, th.text, 1.8 * k);
                }
                dot(pm, cx, cy, 7.0 * k, col);
                ring(pm, cx, cy, 7.0 * k, alpha(th.rim, 2.0), 1.0 * k);
            }
            Action::Width(i) => {
                if active {
                    glass::pill(pm, r.inflate(-1.0 * k), 9.0 * k, th.hover_strong);
                } else if hovered {
                    glass::pill(pm, r.inflate(-1.0 * k), 9.0 * k, th.hover);
                }
                dot(pm, cx, cy, (WIDTHS[i] * 0.5 + 1.2) * k, th.text);
            }
            action => {
                if active {
                    glass::pill(pm, r, 10.0 * k, th.hover_strong);
                    ring(pm, cx, cy, 0.0, th.rim, 0.0);
                } else if hovered && !disabled {
                    glass::pill(pm, r, 10.0 * k, th.hover);
                }
                let color = match action {
                    Action::Confirm => th.primary,
                    Action::Cancel if hovered => th.error,
                    _ if active => th.text,
                    _ => th.text_dim,
                };
                let color = if disabled { alpha(color, 0.35) } else { color };
                icon(pm, action, cx, cy, 8.5 * k, color, k, th);
            }
        }
    }
}

/// Stroke a finished path builder with the icon style.
fn st(pm: &mut PixmapMut, p: &Paint, line: &Stroke, tf: Transform, pb: PathBuilder) {
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, p, line, tf, None);
    }
}

fn dot(pm: &mut PixmapMut, cx: f32, cy: f32, r: f32, c: Color) {
    if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

fn ring(pm: &mut PixmapMut, cx: f32, cy: f32, r: f32, c: Color, w: f32) {
    if r <= 0.0 || w <= 0.0 {
        return;
    }
    if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
        let s = Stroke { width: w, ..Default::default() };
        pm.stroke_path(&p, &paint(c), &s, Transform::identity(), None);
    }
}

/// Icons live in a [-1, 1] box scaled by `h` (half size in pixels).
#[allow(clippy::too_many_arguments)]
fn icon(pm: &mut PixmapMut, a: Action, cx: f32, cy: f32, h: f32, c: Color, k: f32, _th: &Theme) {
    let tf = Transform::from_translate(cx, cy).pre_scale(h, h);
    let line = Stroke {
        width: 1.7 * k / h,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    let p = paint(c);
    let poly = |pts: &[(f32, f32)]| {
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0, pts[0].1);
        for q in &pts[1..] {
            pb.line_to(q.0, q.1);
        }
        pb
    };
    match a {
        Action::Tool(Tool::Arrow) => {
            st(pm, &p, &line, tf, poly(&[(-0.75, 0.75), (0.7, -0.7)]));
            st(pm, &p, &line, tf, poly(&[(0.1, -0.7), (0.7, -0.7), (0.7, -0.1)]));
        }
        Action::Tool(Tool::Line) => st(pm, &p, &line, tf, poly(&[(-0.75, 0.6), (0.75, -0.6)])),
        Action::Tool(Tool::Rect) => {
            if let Some(r) = glass::rrect(Rect::new(-0.78, -0.6, 1.56, 1.2), 0.18) {
                pm.stroke_path(&r, &p, &line, tf, None);
            }
        }
        Action::Tool(Tool::Ellipse) => {
            if let Some(r) = tiny_skia::Rect::from_xywh(-0.8, -0.6, 1.6, 1.2) {
                if let Some(o) = PathBuilder::from_oval(r) {
                    pm.stroke_path(&o, &p, &line, tf, None);
                }
            }
        }
        Action::Tool(Tool::Pen) => {
            let mut pb = PathBuilder::new();
            pb.move_to(-0.8, 0.45);
            pb.cubic_to(-0.35, -1.0, 0.1, 1.0, 0.8, -0.45);
            st(pm, &p, &line, tf, pb);
        }
        Action::Tool(Tool::Highlighter) => {
            let wide = Stroke { width: 0.62, line_cap: LineCap::Butt, ..Default::default() };
            let mut pb = PathBuilder::new();
            pb.move_to(-0.7, 0.1);
            pb.line_to(0.7, -0.5);
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &paint(alpha(c, 0.55)), &wide, tf, None);
            }
            st(pm, &p, &line, tf, poly(&[(-0.8, 0.8), (0.8, 0.8)]));
        }
        Action::Tool(Tool::Text) => {
            st(pm, &p, &line, tf, poly(&[(-0.7, -0.65), (0.7, -0.65)]));
            st(pm, &p, &line, tf, poly(&[(0.0, -0.65), (0.0, 0.75)]));
        }
        Action::Tool(Tool::Pixelate) => {
            for j in 0..3 {
                for i in 0..3 {
                    let (x, y) = (-0.78 + i as f32 * 0.54, -0.78 + j as f32 * 0.54);
                    let fill = if (i + j) % 2 == 0 { c } else { alpha(c, 0.3) };
                    if let Some(r) = tiny_skia::Rect::from_xywh(x, y, 0.48, 0.48) {
                        pm.fill_rect(r, &paint(fill), tf, None);
                    }
                }
            }
        }
        Action::Tool(Tool::Step) => {
            if let Some(circle) = PathBuilder::from_circle(0.0, 0.0, 0.82) {
                pm.stroke_path(&circle, &p, &line, tf, None);
            }
            st(pm, &p, &line, tf, poly(&[(-0.22, -0.22), (0.12, -0.5), (0.12, 0.5)]));
        }
        Action::Undo | Action::Redo => {
            let m = if a == Action::Redo { -1.0 } else { 1.0 };
            let mut pb = PathBuilder::new();
            pb.move_to(0.7 * m, 0.6);
            pb.cubic_to(0.7 * m, 0.0, 0.3 * m, -0.35, -0.35 * m, -0.35);
            st(pm, &p, &line, tf, pb);
            st(pm, &p, &line, tf, poly(&[(0.0 * m, -0.8), (-0.55 * m, -0.35), (0.0 * m, 0.1)]));
        }
        Action::Cancel => {
            st(pm, &p, &line, tf, poly(&[(-0.6, -0.6), (0.6, 0.6)]));
            st(pm, &p, &line, tf, poly(&[(0.6, -0.6), (-0.6, 0.6)]));
        }
        Action::Confirm => {
            let bold = Stroke { width: 2.1 * k / h, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
            if let Some(path) = poly(&[(-0.72, 0.05), (-0.2, 0.58), (0.74, -0.55)]).finish() {
                pm.stroke_path(&path, &p, &bold, tf, None);
            }
        }
        _ => {}
    }
}
