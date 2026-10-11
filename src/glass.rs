//! Liquid-glass panels. The compositor-side blur used by the taskbar is not
//! available to us (we draw on a frozen screenshot), so we blur the pixels
//! behind a panel ourselves, boost their color, then add tint, sheen, rim
//! and shadow.

use tiny_skia::{
    Color, FillRule, GradientStop, IntRect, LinearGradient, Paint, Path, PathBuilder, Pattern,
    Pixmap, PixmapMut, PixmapPaint, Point, SpreadMode, Stroke, Transform, FilterQuality,
};

use crate::{geom::Rect, theme::Theme};

/// Rounded rectangle path (all radii equal, clamped to half the short side).
pub fn rrect(r: Rect, radius: f32) -> Option<Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    let k = rad * 0.552_284_75;
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - rad + k, y0, x1, y0 + rad - k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - rad + k, x1 - rad + k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + rad - k, y1, x0, y1 - rad + k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + rad - k, x0 + rad - k, y0, x0 + rad, y0);
    pb.close();
    pb.finish()
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

fn with_alpha(c: Color, factor: f32) -> Color {
    Color::from_rgba(c.red(), c.green(), c.blue(), (c.alpha() * factor).clamp(0.0, 1.0)).unwrap()
}

/// In-place separable box blur on premultiplied RGBA, edge-clamped.
pub fn box_blur(data: &mut [u8], w: usize, h: usize, radius: usize, passes: usize) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let mut scratch = vec![0u8; w.max(h) * 4];
    let mut col = vec![0u8; h * 4];
    for _ in 0..passes {
        for y in 0..h {
            blur_line(&mut data[y * w * 4..(y + 1) * w * 4], 4, w, radius, &mut scratch);
        }
        for x in 0..w {
            for y in 0..h {
                col[y * 4..y * 4 + 4].copy_from_slice(&data[(y * w + x) * 4..(y * w + x) * 4 + 4]);
            }
            blur_line(&mut col, 4, h, radius, &mut scratch);
            for y in 0..h {
                data[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&col[y * 4..y * 4 + 4]);
            }
        }
    }
}

fn blur_line(px: &mut [u8], ch: usize, n: usize, r: usize, scratch: &mut [u8]) {
    let src = &mut scratch[..n * ch];
    src.copy_from_slice(&px[..n * ch]);
    let win = (2 * r + 1) as u32;
    for c in 0..ch {
        let at = |i: isize| src[i.clamp(0, n as isize - 1) as usize * ch + c] as u32;
        let mut sum = 0u32;
        for i in -(r as isize)..=(r as isize) {
            sum += at(i);
        }
        for i in 0..n {
            px[i * ch + c] = ((sum + win / 2) / win) as u8;
            sum += at(i as isize + r as isize + 1);
            sum -= at(i as isize - r as isize);
        }
    }
}

/// Push colors away from gray (Liquid Glass looks vivid behind the sheen).
fn vibrance(data: &mut [u8], amount: f32) {
    for px in data.chunks_exact_mut(4) {
        let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        for (k, v) in [r, g, b].into_iter().enumerate() {
            px[k] = (l + (v - l) * amount).clamp(0.0, 255.0) as u8;
        }
    }
}

/// Soft drop shadow below a rounded rect.
fn shadow(dst: &mut PixmapMut, r: Rect, radius: f32, k: f32, color: Color) {
    let spread = 22.0 * k;
    let dy = 7.0 * k;
    let box_ = r.inflate(spread);
    let (bw, bh) = (box_.w.ceil() as u32, box_.h.ceil() as u32);
    let Some(mut tmp) = Pixmap::new(bw.max(1), bh.max(1)) else { return };
    let local = Rect::new(spread, spread + dy, r.w, r.h);
    if let Some(p) = rrect(local, radius) {
        tmp.fill_path(&p, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
    let (w, h) = (tmp.width() as usize, tmp.height() as usize);
    box_blur(tmp.data_mut(), w, h, (6.0 * k).round() as usize, 2);
    dst.draw_pixmap(
        box_.x.floor() as i32,
        box_.y.floor() as i32,
        tmp.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

/// Draw a glass panel at `r` (physical pixels). `k` is the pixel scale
/// (physical per logical) so blur radius and strokes keep their look.
pub fn panel(dst: &mut PixmapMut, r: Rect, radius: f32, k: f32, th: &Theme) {
    let Some(path) = rrect(r, radius) else { return };
    shadow(dst, r, radius, k, th.shadow);

    // blurred, boosted copy of what is behind the panel
    let bounds = Rect::new(0.0, 0.0, dst.width() as f32, dst.height() as f32);
    let margin = 16.0 * k;
    if let Some(src) = r.inflate(margin).intersect(&bounds) {
        let ir = IntRect::from_xywh(
            src.x.floor() as i32,
            src.y.floor() as i32,
            src.w.ceil() as u32,
            src.h.ceil() as u32,
        );
        if let Some(mut back) = ir.and_then(|ir| dst.as_ref().clone_rect(ir)) {
            let (bw, bh) = (back.width() as usize, back.height() as usize);
            box_blur(back.data_mut(), bw, bh, (7.0 * k).round().max(1.0) as usize, 3);
            vibrance(back.data_mut(), 1.35);
            let ox = ir.map_or(0, |i| i.x()) as f32;
            let oy = ir.map_or(0, |i| i.y()) as f32;
            let mut p = Paint::default();
            p.anti_alias = true;
            p.shader = Pattern::new(
                back.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Nearest,
                1.0,
                Transform::from_translate(ox, oy),
            );
            dst.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        }
    }

    // tint
    dst.fill_path(&path, &paint(th.glass), FillRule::Winding, Transform::identity(), None);

    // sheen: bright at the top edge, fading out
    let mut sheen = Paint::default();
    sheen.anti_alias = true;
    sheen.shader = LinearGradient::new(
        Point::from_xy(0.0, r.y),
        Point::from_xy(0.0, r.bottom()),
        vec![
            GradientStop::new(0.0, th.shine),
            GradientStop::new(0.45, th.shine_low),
            GradientStop::new(1.0, with_alpha(th.shine_low, 0.5)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap_or(tiny_skia::Shader::SolidColor(th.shine_low));
    dst.fill_path(&path, &sheen, FillRule::Winding, Transform::identity(), None);

    // rim: brighter on top (light catching the upper edge), faint at the bottom
    let inset = rrect(r.inflate(-0.5 * k), radius - 0.5 * k).unwrap_or(path);
    let mut rim = Paint::default();
    rim.anti_alias = true;
    rim.shader = LinearGradient::new(
        Point::from_xy(0.0, r.y),
        Point::from_xy(0.0, r.bottom()),
        vec![
            GradientStop::new(0.0, with_alpha(th.rim, 2.6)),
            GradientStop::new(0.5, th.rim),
            GradientStop::new(1.0, with_alpha(th.rim, 0.6)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap_or(tiny_skia::Shader::SolidColor(th.rim));
    let stroke = Stroke { width: k.max(1.0), ..Default::default() };
    dst.stroke_path(&inset, &rim, &stroke, Transform::identity(), None);
}

/// Flat highlight inside a panel (hover / active button).
pub fn pill(dst: &mut PixmapMut, r: Rect, radius: f32, color: Color) {
    if let Some(p) = rrect(r, radius) {
        dst.fill_path(&p, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
}
