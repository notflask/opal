//! Finishing a screenshot: PNG file, clipboard, notification.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::{capture::Shot, geom::Rect};

/// Union of all outputs in logical coordinates.
pub fn bounds(shots: &[Shot]) -> Rect {
    shots
        .iter()
        .map(|s| Rect::new(s.x as f32, s.y as f32, s.lw as f32, s.lh as f32))
        .reduce(|a, b| a.union(&b))
        .unwrap_or_default()
}

/// Physical pixels per logical pixel of the result: the sharpest output wins.
pub fn scale(shots: &[Shot]) -> f32 {
    shots.iter().map(Shot::scale).fold(1.0, f32::max)
}

/// Cut a logical (compositor space) rectangle out of the captures, which
/// may span several outputs. Outputs with a lower scale are upsampled.
pub fn crop(shots: &[Shot], r: Rect) -> Result<Pixmap> {
    let scale = scale(shots);
    let w = (r.w * scale).round() as u32;
    let h = (r.h * scale).round() as u32;
    let mut out = Pixmap::new(w, h).context("selection is empty")?;
    for s in shots {
        let k = scale / s.scale();
        let tx = (s.x as f32 - r.x) * scale;
        let ty = (s.y as f32 - r.y) * scale;
        let quality = if (k - 1.0).abs() < 1e-3 {
            tiny_skia::FilterQuality::Nearest
        } else {
            tiny_skia::FilterQuality::Bilinear
        };
        out.draw_pixmap(
            0,
            0,
            s.pixmap.as_ref(),
            &PixmapPaint { quality, ..Default::default() },
            Transform::from_row(k, 0.0, 0.0, k, tx, ty),
            None,
        );
    }
    Ok(out)
}

pub fn encode_png(pm: &Pixmap) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, pm.width(), pm.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    // fast compression: clipboard and notification appear ~10x sooner
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header()?;
    // opaque captures, so premultiplied == straight alpha for the screen
    let mut data = pm.data().to_vec();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    w.write_image_data(&data)?;
    w.finish()?;
    Ok(out)
}

pub fn save(png: &[u8], dir: &Path, stamp: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("Screenshot from {stamp}.png"));
    fs::write(&path, png)?;
    Ok(path)
}

/// Serves the clipboard from a forked background process (like `wl-copy`).
pub fn copy(png: &[u8]) -> Result<()> {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    let mut opts = Options::new();
    opts.foreground(false);
    opts.copy(Source::Bytes(png.to_vec().into()), MimeType::Specific("image/png".into()))?;
    Ok(())
}

pub fn notify(path: &Path) {
    let _ = Command::new("notify-send")
        .args(["-a", "Screenshot", "-i"])
        .arg(path)
        .args(["Screenshot kopiert"])
        .arg(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        .spawn();
}

/// `YYYY-MM-DD HH-MM-SS` in local time, via `date` (no tz database needed).
pub fn timestamp() -> String {
    Command::new("date")
        .arg("+%Y-%m-%d %H-%M-%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}
