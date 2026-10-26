//! Finishing a screenshot: crop, PNG file, clipboard, notification.

use std::{
    ffi::CString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::{capture::Shot, config::Output, geom::Rect};

/// Union of all outputs in logical coordinates.
pub fn bounds(shots: &[Shot]) -> Rect {
    shots.iter().map(Shot::logical).reduce(|a, b| a.union(&b)).unwrap_or_default()
}

/// Physical pixels per logical pixel of the result: the sharpest output wins.
pub fn scale(shots: &[Shot]) -> f32 {
    shots.iter().map(Shot::scale).fold(1.0, f32::max)
}

/// Cut a logical (compositor space) rectangle out of the captures, which
/// may span several outputs. Outputs with a lower scale are upsampled.
/// Only the pixels inside the rectangle are read.
pub fn crop(shots: &[Shot], r: Rect) -> Result<Pixmap> {
    let scale = scale(shots);
    let w = (r.w * scale).round() as u32;
    let h = (r.h * scale).round() as u32;
    let mut out = Pixmap::new(w, h).context("selection is empty")?;
    for s in shots {
        let Some(part) = r.intersect(&s.logical()) else { continue };
        let sk = s.scale();
        let px0 = ((part.x - s.x as f32) * sk).floor().max(0.0) as u32;
        let py0 = ((part.y - s.y as f32) * sk).floor().max(0.0) as u32;
        let px1 = (((part.right() - s.x as f32) * sk).ceil() as u32).min(s.w);
        let py1 = (((part.bottom() - s.y as f32) * sk).ceil() as u32).min(s.h);
        let Some(src) = s.read_rgba(px0, py0, px1.saturating_sub(px0), py1.saturating_sub(py0)) else { continue };
        // where the top-left of `src` lands in the result, in result pixels
        let k = scale / sk;
        let dx = ((s.x as f32 - r.x) * scale) + px0 as f32 * k;
        let dy = ((s.y as f32 - r.y) * scale) + py0 as f32 * k;
        let quality = if (k - 1.0).abs() < 1e-3 { FilterQuality::Nearest } else { FilterQuality::Bilinear };
        let tf = if (k - 1.0).abs() < 1e-3 {
            Transform::from_translate(dx.round(), dy.round())
        } else {
            Transform::from_row(k, 0.0, 0.0, k, dx, dy)
        };
        out.draw_pixmap(0, 0, src.as_ref(), &PixmapPaint { quality, ..Default::default() }, tf, None);
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
    // screens are opaque, so premultiplied == straight alpha; only convert
    // when something translucent got through
    if pm.data().chunks_exact(4).all(|p| p[3] == 255) {
        w.write_image_data(pm.data())?;
    } else {
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
    }
    w.finish()?;
    Ok(out)
}

/// strftime in local time (the C library knows the time zone).
pub fn format_time(pattern: &str) -> String {
    let Ok(fmt) = CString::new(pattern) else { return "screenshot".into() };
    let mut buf = [0u8; 512];
    let n = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), fmt.as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// File name from the configured pattern, safe to join to a directory.
pub fn file_name(pattern: &str) -> String {
    let mut name: String = format_time(pattern).chars().map(|c| if c == '/' { '-' } else { c }).collect();
    if name.trim().is_empty() {
        name = "screenshot".into();
    }
    if !name.to_ascii_lowercase().ends_with(".png") {
        name.push_str(".png");
    }
    name
}

/// Write `png` to `dir`, never overwriting an existing file.
pub fn save(png: &[u8], dir: &Path, name: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let mut path = dir.join(name);
    if path.exists() {
        let stem = name.trim_end_matches(".png");
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{stem} ({n}).png"));
            n += 1;
        }
    }
    fs::write(&path, png)?;
    Ok(path)
}

pub const SERVE_CLIPBOARD: &str = "--serve-clipboard";

/// Hand the PNG to a detached copy of ourselves that keeps serving the
/// clipboard after we exit (Wayland clipboards live only as long as their
/// owner, like `wl-copy` which forks for the same reason).
pub fn copy(png: &[u8]) -> Result<()> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg(SERVE_CLIPBOARD)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child.stdin.take().context("no stdin")?.write_all(png)?;
    Ok(())
}

/// Body of the detached child: read a PNG from stdin and own the clipboard
/// until something else replaces it.
pub fn serve_clipboard() -> Result<()> {
    use std::io::Read;
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    let mut png = Vec::new();
    std::io::stdin().read_to_end(&mut png)?;
    let mut opts = Options::new();
    opts.foreground(true);
    opts.copy(Source::Bytes(png.into()), MimeType::Specific("image/png".into()))?;
    Ok(())
}

pub fn notify(cfg: &Output, path: &Path) {
    let Some((prog, args)) = cfg.notify_cmd.split_first() else { return };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let args = args.iter().map(|a| a.replace("{path}", &path.to_string_lossy()).replace("{name}", &name));
    let _ = Command::new(prog).args(args).stdin(Stdio::null()).spawn();
}

/// PNG to file and clipboard, notify, as configured. Returns the saved path.
pub fn finish(cfg: &Output, png: &[u8]) -> Result<Option<PathBuf>> {
    let dir = crate::config::expand(&cfg.save_dir);
    let name = file_name(&cfg.filename);
    // file and clipboard in parallel: the clipboard child needs a moment to start
    let (path, clip) = std::thread::scope(|s| {
        let save = cfg.save.then(|| s.spawn(|| save(png, &dir, &name)));
        let clip = if cfg.clipboard { copy(png) } else { Ok(()) };
        (save.map(|h| h.join().expect("save thread")), clip)
    });
    let path = path.transpose()?;
    if let Some(p) = &path {
        if cfg.notify {
            notify(cfg, p);
        }
    }
    clip?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert!(file_name("a/b %Y").ends_with(".png"));
        assert!(!file_name("a/b").contains('/'));
        assert_eq!(file_name("x.PNG"), "x.PNG");
        assert_eq!(file_name(""), "screenshot.png");
        assert_eq!(format_time("%Y").len(), 4);
    }

    #[test]
    fn save_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("opal-test-{}", std::process::id()));
        let a = save(b"1", &dir, "x.png").unwrap();
        let b = save(b"2", &dir, "x.png").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(&a).unwrap(), b"1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn png_opaque_roundtrip() {
        let mut pm = Pixmap::new(2, 1).unwrap();
        pm.data_mut().copy_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255]);
        let bytes = encode_png(&pm).unwrap();
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut buf = vec![0; dec.output_buffer_size().unwrap()];
        dec.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..8], &[1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
